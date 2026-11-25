//! Offline experiment for the signed artifacts of a fixed-epoch two-chain protocol.
//! Production consensus, validator signing state and transport remain in CometBFT.

use anyhow::{Context, Result, ensure};
use ckb_hash::new_blake2b;
use ckb_types::{packed::Byte32, prelude::*};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use stem_ledger::block::{Header, HeaderContext, StateCommitment};
use stem_stake::{Attestation, Authority, Member};

#[path = "weighted_finality/journal.rs"]
mod journal;

struct Committee {
    network: Byte32,
    epoch: u64,
    generation: [u8; 32],
    members: Vec<Member>,
    quorum: Authority,
    total: u64,
}

impl Committee {
    fn new(network: Byte32, epoch: u64, members: Vec<Member>) -> Result<Self> {
        let mut hash = new_blake2b();
        hash.update(b"Stem weighted experiment committee\0");
        for member in &members {
            hash.update(&member.key);
            hash.update(&member.power.to_le_bytes());
        }
        let mut generation = [0; 32];
        hash.finalize(&mut generation);
        let quorum = Authority::new(generation, &members)?;
        let total = members.iter().map(|member| member.power).sum();
        Ok(Self {
            network,
            epoch,
            generation,
            members,
            quorum,
            total,
        })
    }

    fn leader(&self, view: u64) -> Result<usize> {
        let mut slot = view.checked_sub(1).context("invalid view")? % self.total;
        for (index, member) in self.members.iter().enumerate() {
            match slot.checked_sub(member.power) {
                Some(after) => slot = after,
                None => return Ok(index),
            }
        }
        unreachable!("nonzero checked committee weight")
    }

    fn digest(&self, domain: &[u8], view: u64, commitment: &[u8]) -> [u8; 32] {
        let mut hash = new_blake2b();
        hash.update(domain);
        hash.update(self.network.as_slice());
        hash.update(&self.epoch.to_le_bytes());
        hash.update(&self.generation);
        hash.update(&view.to_le_bytes());
        hash.update(commitment);
        let mut digest = [0; 32];
        hash.finalize(&mut digest);
        digest
    }

    fn vote_digest(&self, view: u64, header: &Header) -> [u8; 32] {
        self.digest(
            b"Stem weighted experiment vote\0",
            view,
            header.id(&self.network).as_slice(),
        )
    }

    fn timeout_digest(&self, view: u64, high: &Quorum) -> [u8; 32] {
        self.digest(
            b"Stem weighted experiment timeout\0",
            view,
            &self.vote_digest(high.view, &high.header),
        )
    }

    fn verify_quorum(&self, qc: &Quorum) -> Result<()> {
        ensure!(qc.view != 0, "invalid certified view");
        Ok(self
            .quorum
            .verify(&self.vote_digest(qc.view, &qc.header), &qc.votes)?)
    }

    fn verify_timeout(&self, tc: &Timeout) -> Result<u64> {
        ensure!(
            !tc.high.is_empty() && tc.high.len() <= self.members.len(),
            "invalid high QC list"
        );
        let mut previous = 0;
        for high in &tc.high {
            ensure!(
                previous < high.view && high.view < tc.view,
                "invalid high QC view"
            );
            self.verify_quorum(high)?;
            previous = high.view;
        }
        let mut seen = vec![false; tc.high.len()];
        let mut signed = 0;
        let mut previous_signer = None;
        for report in &tc.reports {
            let signer = report.vote.guardian;
            ensure!(
                previous_signer.is_none_or(|last| last < signer),
                "duplicate or unordered timeout signer"
            );
            previous_signer = Some(signer);
            let member = self
                .members
                .get(usize::from(signer))
                .context("unknown signer")?;
            let high = tc.high.get(report.high_index).context("unknown high QC")?;
            VerifyingKey::from_bytes(&member.key)?.verify_strict(
                &self.timeout_digest(tc.view, high),
                &Signature::from_bytes(&report.vote.signature),
            )?;
            seen[report.high_index] = true;
            signed += member.power;
        }
        ensure!(
            seen.into_iter().all(|mentioned| mentioned),
            "timeout includes an unsigned high QC"
        );
        ensure!(
            u128::from(signed) * 3 > u128::from(self.total) * 2,
            "insufficient timeout weight"
        );
        Ok(previous)
    }

    fn verify_proposal(
        &self,
        view: u64,
        proposer: usize,
        header: &Header,
        parent: &Quorum,
        timeout: Option<&Timeout>,
    ) -> Result<()> {
        self.verify_quorum(parent)?;
        ensure!(self.leader(view)? == proposer, "wrong proposal leader");
        ensure!(
            parent.header.context.height.checked_add(1) == Some(header.context.height)
                && header.context.parent == parent.header.id(&self.network)
                && header.context.timestamp_ms >= parent.header.context.timestamp_ms,
            "proposal does not extend its justified execution Header"
        );
        if parent.view.checked_add(1) == Some(view) {
            ensure!(
                timeout.is_none(),
                "adjacent proposal carried an unnecessary timeout"
            );
        } else {
            let timeout =
                timeout.context("proposal skipped a view without a timeout certificate")?;
            ensure!(
                timeout.view.checked_add(1) == Some(view),
                "wrong timeout view"
            );
            ensure!(
                parent.view >= self.verify_timeout(timeout)?,
                "proposal fell below the high QC"
            );
        }
        Ok(())
    }

    fn finalize(&self, parent: &Quorum, child: &Quorum) -> Result<Byte32> {
        ensure!(
            parent.view.checked_add(1) == Some(child.view),
            "two-chain finality requires adjacent views"
        );
        self.verify_proposal(
            child.view,
            self.leader(child.view)?,
            &child.header,
            parent,
            None,
        )?;
        self.verify_quorum(child)?;
        Ok(parent.header.id(&self.network))
    }
}

#[derive(Clone)]
struct Quorum {
    header: Header,
    view: u64,
    votes: Vec<Attestation>,
}

struct Timeout {
    view: u64,
    high: Vec<Quorum>,
    reports: Vec<Report>,
}

struct Report {
    high_index: usize,
    vote: Attestation,
}

fn header() -> Header {
    Header {
        context: HeaderContext {
            height: 1,
            timestamp_ms: 1,
            parent: [3; 32].pack(),
            transaction_root: [4; 32].pack(),
        },
        state: StateCommitment {
            cell_root: [5; 32].pack(),
            system_root: [6; 32].pack(),
            history_root: [7; 32].pack(),
        },
        receipt_root: [8; 32].pack(),
    }
}

#[test]
fn weighted_certificates_bind_execution_and_timeout_ancestry() -> Result<()> {
    let mut keys: Vec<_> = (1..=4)
        .map(|seed| SigningKey::from_bytes(&[seed; 32]))
        .collect();
    keys.sort_by_key(|key| key.verifying_key().to_bytes());
    let members = keys
        .iter()
        .zip([1, 1, 2, 2])
        .map(|(key, power)| Member {
            key: key.verifying_key().to_bytes(),
            power,
        })
        .collect::<Vec<_>>();
    let committee = Committee::new([1; 32].pack(), 4, members.clone())?;
    let sign = |index: u16, digest: [u8; 32]| Attestation {
        guardian: index,
        signature: keys[usize::from(index)].sign(&digest).to_bytes(),
    };
    let certify = |header: Header, view| {
        let digest = committee.vote_digest(view, &header);
        Quorum {
            header,
            view,
            votes: [0, 2, 3].map(|index| sign(index, digest)).to_vec(),
        }
    };
    let parent = certify(header(), 1);
    let mut insufficient = parent.clone();
    insufficient.votes.remove(0);
    assert!(committee.verify_quorum(&insufficient).is_err());
    let mut child = header();
    child.context = HeaderContext::after(&committee.network, &parent.header, 2, [9; 32].pack())?;
    let next = certify(child.clone(), 2);
    assert_eq!(
        committee.finalize(&parent, &next)?,
        parent.header.id(&committee.network)
    );
    let mut wrong = next.clone();
    wrong.header.receipt_root = [1; 32].pack();
    assert!(committee.finalize(&parent, &wrong).is_err());
    let another_epoch = Committee::new(committee.network.clone(), 5, members)?;
    assert!(another_epoch.verify_quorum(&parent).is_err());

    let timeout_view = 3;
    let report = |index| Report {
        high_index: 0,
        vote: sign(index, committee.timeout_digest(timeout_view, &next)),
    };
    let mut timeout = Timeout {
        view: timeout_view,
        high: vec![next.clone()],
        reports: vec![report(2), report(3)],
    };
    assert!(committee.verify_timeout(&timeout).is_err()); // Exactly two-thirds.
    timeout.reports.insert(0, report(0));
    assert_eq!(committee.verify_timeout(&timeout)?, 2);
    let leader = committee.leader(4)?;
    assert!(
        committee
            .verify_proposal(4, leader, &child, &parent, Some(&timeout))
            .is_err()
    );
    let mut recovered = child.clone();
    recovered.context = HeaderContext::after(&committee.network, &next.header, 4, [11; 32].pack())?;
    committee.verify_proposal(4, leader, &recovered, &next, Some(&timeout))?;
    assert!(
        committee
            .verify_proposal(4, leader, &recovered, &next, None)
            .is_err()
    );
    assert!(committee.finalize(&next, &certify(recovered, 4)).is_err());
    timeout.reports.push(report(3));
    assert!(committee.verify_timeout(&timeout).is_err());
    let mut mixed = Timeout {
        view: timeout_view,
        high: vec![parent.clone(), next.clone()],
        reports: vec![
            Report {
                high_index: 0,
                vote: sign(0, committee.timeout_digest(timeout_view, &parent)),
            },
            Report {
                high_index: 1,
                ..report(2)
            },
            Report {
                high_index: 1,
                ..report(3)
            },
        ],
    };
    assert_eq!(committee.verify_timeout(&mixed)?, next.view);
    mixed.reports[0].high_index = 1;
    assert!(committee.verify_timeout(&mixed).is_err());
    let leaders = (1..=6)
        .map(|view| committee.leader(view))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(leaders, [0, 1, 2, 2, 3, 3]);
    Ok(())
}
