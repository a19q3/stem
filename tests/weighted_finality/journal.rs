use std::path::Path;

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{DB, Options, WriteBatch, WriteOptions, prelude::*};
use ed25519_dalek::{Signer, SigningKey};
use stem_stake::Attestation;

use super::{Committee, Header, Quorum, Timeout};

const STATE: &[u8] = b"weighted-signing-journal";
const BINDING: &[u8] = b"weighted-signing-journal-identity";

#[derive(Clone, Copy, Default)]
struct ViewDigest {
    view: u64,
    digest: [u8; 32],
}

#[derive(Clone, Copy, Default)]
struct State {
    lock: u64,
    high: ViewDigest,
    vote: ViewDigest,
    timeout: ViewDigest,
}

impl State {
    fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() == 128, "malformed signing journal");
        let number = |offset| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        let entry = |offset| ViewDigest {
            view: number(offset),
            digest: bytes[offset + 8..offset + 40].try_into().unwrap(),
        };
        let state = Self {
            lock: number(0),
            high: entry(8),
            vote: entry(48),
            timeout: entry(88),
        };
        ensure!(
            state.lock <= state.high.view,
            "journal lock exceeds high QC"
        );
        Ok(state)
    }

    fn encode(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(128);
        bytes.extend(self.lock.to_le_bytes());
        for entry in [self.high, self.vote, self.timeout] {
            bytes.extend(entry.view.to_le_bytes());
            bytes.extend(entry.digest);
        }
        bytes
    }

    fn record_qc(&mut self, committee: &Committee, qc: &Quorum) -> Result<()> {
        let digest = committee.vote_digest(qc.view, &qc.header);
        if qc.view == self.high.view {
            ensure!(digest == self.high.digest, "conflicting certified view");
        } else if qc.view > self.high.view {
            self.high = ViewDigest {
                view: qc.view,
                digest,
            };
        }
        Ok(())
    }
}

struct Journal<'a> {
    db: DB,
    committee: &'a Committee,
    signer: u16,
    key: &'a SigningKey,
    state: State,
}

impl<'a> Journal<'a> {
    fn open(
        path: &Path,
        committee: &'a Committee,
        key: &'a SigningKey,
        create: bool,
    ) -> Result<Self> {
        let public = key.verifying_key().to_bytes();
        let signer = committee
            .members
            .binary_search_by_key(&public, |member| member.key)
            .ok()
            .context("key is not registered for the signing epoch")?;
        let binding = committee.digest(b"Stem weighted experiment signing journal\0", 0, &public);
        let mut options = Options::default();
        options.create_if_missing(create);
        options.set_error_if_exists(create);
        let db = DB::open(&options, path)?;
        let state = if create {
            let mut batch = WriteBatch::default();
            batch.put(BINDING, &binding)?;
            batch.put(STATE, &State::default().encode())?;
            sync(&db, &batch)?;
            State::default()
        } else {
            let found = db.get(BINDING)?.context("missing journal binding")?;
            ensure!(
                found.as_ref() == binding,
                "signing journal identity mismatch"
            );
            State::decode(&db.get(STATE)?.context("missing journal state")?)?
        };
        Ok(Self {
            db,
            committee,
            signer: u16::try_from(signer)?,
            key,
            state,
        })
    }

    fn learn_finality(&mut self, parent: &Quorum, child: &Quorum) -> Result<()> {
        self.committee.finalize(parent, child)?;
        let mut state = self.state;
        state.record_qc(self.committee, child)?;
        state.lock = state.lock.max(parent.view);
        self.persist(state)
    }

    fn vote(
        &mut self,
        view: u64,
        claimed_leader: usize,
        candidate: &Header,
        execution: &Header,
        parent: &Quorum,
        timeout: Option<&Timeout>,
    ) -> Result<Attestation> {
        ensure!(
            execution == candidate,
            "local execution differs from proposal"
        );
        self.committee
            .verify_proposal(view, claimed_leader, candidate, parent, timeout)?;
        let digest = self.committee.vote_digest(view, candidate);
        if self.state.vote.view == view {
            ensure!(self.state.vote.digest == digest, "conflicting vote");
            return Ok(self.sign(digest));
        }
        ensure!(
            view > self.state.vote.view
                && view > self.state.timeout.view
                && view > self.state.high.view
                && parent.view >= self.state.lock,
            "proposal violates persisted view or lock"
        );
        let mut state = self.state;
        state.record_qc(self.committee, parent)?;
        state.vote = ViewDigest { view, digest };
        self.persist(state)?;
        Ok(self.sign(digest))
    }

    fn timeout(&mut self, view: u64, high: &Quorum) -> Result<Attestation> {
        self.committee.verify_quorum(high)?;
        let digest = self.committee.timeout_digest(view, high);
        if self.state.timeout.view == view {
            ensure!(self.state.timeout.digest == digest, "conflicting timeout");
            return Ok(self.sign(digest));
        }
        ensure!(
            view > self.state.timeout.view
                && view >= self.state.vote.view
                && view > high.view
                && high.view >= self.state.high.view,
            "timeout violates persisted view or high QC"
        );
        let mut state = self.state;
        state.record_qc(self.committee, high)?;
        state.timeout = ViewDigest { view, digest };
        self.persist(state)?;
        Ok(self.sign(digest))
    }

    fn sign(&self, digest: [u8; 32]) -> Attestation {
        Attestation {
            guardian: self.signer,
            signature: self.key.sign(&digest).to_bytes(),
        }
    }

    fn persist(&mut self, state: State) -> Result<()> {
        let mut batch = WriteBatch::default();
        batch.put(STATE, &state.encode())?;
        sync(&self.db, &batch)?;
        self.state = state;
        Ok(())
    }
}

fn sync(db: &DB, batch: &WriteBatch) -> Result<()> {
    let mut options = WriteOptions::default();
    options.set_sync(true);
    db.write_opt(batch, &options)?;
    Ok(())
}

#[test]
fn restart_preserves_votes_timeout_watermark_and_lock() -> Result<()> {
    use super::{HeaderContext, Member, Report, header};
    use ckb_types::prelude::*;

    let mut keys: Vec<_> = (1..=4)
        .map(|seed| SigningKey::from_bytes(&[seed; 32]))
        .collect();
    keys.sort_by_key(|key| key.verifying_key().to_bytes());
    let members: Vec<_> = keys
        .iter()
        .map(|key| Member {
            key: key.verifying_key().to_bytes(),
            power: 1,
        })
        .collect();
    let committee = Committee::new([1; 32].pack(), 4, members.clone())?;
    let attest = |digest: [u8; 32]| {
        keys[..3]
            .iter()
            .enumerate()
            .map(|(index, key)| Attestation {
                guardian: index as u16,
                signature: key.sign(&digest).to_bytes(),
            })
            .collect::<Vec<_>>()
    };
    let certify = |view, header: &Header| Quorum {
        view,
        header: header.clone(),
        votes: attest(committee.vote_digest(view, header)),
    };
    let root = certify(1, &header());
    let next = |parent: &Header, value: u8| -> Result<Header> {
        let mut result = header();
        result.context = HeaderContext::after(&committee.network, parent, 2, [value; 32].pack())?;
        Ok(result)
    };
    let child = next(&root.header, 9)?;
    let child_qc = certify(2, &child);
    let third = next(&child, 10)?;
    let third_qc = certify(3, &third);
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("journal");
    assert!(Journal::open(&path, &committee, &keys[0], false).is_err());
    let mut journal = Journal::open(&path, &committee, &keys[0], true)?;
    let leader = committee.leader(2)?;
    let first_vote = journal.vote(2, leader, &child, &child, &root, None)?;
    drop(journal);
    assert!(Journal::open(&path, &committee, &keys[0], true).is_err());
    assert!(Journal::open(&path, &committee, &keys[1], false).is_err());
    let next_epoch = Committee::new(committee.network.clone(), 5, members)?;
    assert!(Journal::open(&path, &next_epoch, &keys[0], false).is_err());
    let mut journal = Journal::open(&path, &committee, &keys[0], false)?;
    let replay = journal.vote(2, leader, &child, &child, &root, None)?;
    assert_eq!(first_vote.signature, replay.signature);
    let conflicting = next(&root.header, 11)?;
    assert!(
        journal
            .vote(2, leader, &conflicting, &conflicting, &root, None)
            .is_err()
    );
    journal.learn_finality(&child_qc, &third_qc)?;
    assert_eq!(journal.state.lock, 2);
    let clock = journal.timeout(4, &third_qc)?;
    drop(journal);
    let mut journal = Journal::open(&path, &committee, &keys[0], false)?;
    assert_eq!(journal.state.lock, 2);
    assert!(journal.timeout(4, &child_qc).is_err());
    assert_eq!(journal.timeout(4, &third_qc)?.signature, clock.signature);
    let fourth = next(&third, 12)?;
    assert!(
        journal
            .vote(4, committee.leader(4)?, &fourth, &fourth, &third_qc, None)
            .is_err()
    );

    let tc = Timeout {
        view: 4,
        reports: attest(committee.timeout_digest(4, &root))
            .into_iter()
            .map(|vote| Report {
                high_index: 0,
                vote,
            })
            .collect(),
        high: vec![root.clone()],
    };
    let leader = committee.leader(5)?;
    committee.verify_proposal(5, leader, &child, &root, Some(&tc))?;
    assert!(
        journal
            .vote(5, leader, &child, &child, &root, Some(&tc))
            .is_err()
    );
    assert!(
        journal
            .vote(5, leader, &fourth, &third, &third_qc, Some(&tc))
            .is_err()
    );
    journal.vote(5, leader, &fourth, &fourth, &third_qc, Some(&tc))?;
    Ok(())
}
