//! Restore a fresh, non-signing replica at a pinned finalized execution Header.

use anyhow::{Context, Result, ensure};
use ckb_types::prelude::*;
use prost::Message;
use serde::{Deserialize, Serialize};
use stem_ledger::block::Header;
use stem_stake::{
    Hash32,
    jolteon::{Committee, CommitteeChange, FinalityProof, FinalizedHeader, Quorum},
};

use crate::native_store::{StateSnapshot, Store};

use super::{STATE, finality_key, handoff_key, journal_identity, members, wire};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Certificate {
    handoffs: Vec<String>,
    proof: String,
    terminal: bool,
}

fn genesis(store: &Store) -> Result<Committee> {
    Ok(Committee::new(
        store.network().unpack(),
        store.genesis_id().unpack(),
        0,
        &members(store)?,
    )?)
}

fn advance(
    store: &Store,
    committee: &Committee,
    previous: u64,
    proof: &[u8],
) -> Result<(Committee, CommitteeChange, FinalizedHeader)> {
    let (change, header, _) = committee.verify_handoff(proof)?;
    ensure!(
        header.height() > previous,
        "snapshot handoff does not advance the execution history"
    );
    let next = Committee::at_anchor(
        store.network().unpack(),
        store.genesis_id().unpack(),
        change.epoch,
        header.id(),
        &change.members,
    )?;
    Ok((next, change, header))
}

pub fn export(store: &Store) -> Result<(StateSnapshot, Certificate)> {
    let image = store.export_state()?;
    let height = image.header.context.height;
    let record: wire::Finalized = wire::canonical(
        &store
            .consensus_record(&finality_key(height))?
            .context("snapshot tip is missing its finality certificate")?,
    )?;
    let mut committee = genesis(store)?;
    let mut previous = 0;
    let mut handoffs = Vec::new();
    while committee.epoch() < record.epoch {
        let raw = store
            .consensus_record(&handoff_key(committee.epoch()))?
            .context("archive is missing a snapshot committee handoff")?;
        let (next, _, header) = advance(store, &committee, previous, &raw)?;
        previous = header.height();
        committee = next;
        handoffs.push(hex::encode(raw));
    }
    ensure!(
        committee.epoch() == record.epoch,
        "snapshot certificate skipped its proof epoch"
    );
    let target = if record.handoff {
        advance(store, &committee, previous, &record.proof)?.2
    } else {
        committee.verify_finality(&record.proof)?
    };
    ensure!(
        target.height() > previous && target.as_bytes().as_slice() == image.header.encode(),
        "snapshot tip is not the exact execution Header finalized by its certificate"
    );
    Ok((
        image,
        Certificate {
            handoffs,
            proof: hex::encode(record.proof),
            terminal: record.handoff,
        },
    ))
}

pub fn install(
    store: &mut Store,
    image: StateSnapshot,
    certificate: Certificate,
    pinned_header: Hash32,
    observer: Hash32,
) -> Result<Header> {
    ensure!(
        store.consensus_record(STATE)?.is_none(),
        "snapshot restore requires an unused consensus journal"
    );
    let mut committee = genesis(store)?;
    let mut journal = wire::Journal {
        identity: journal_identity(&committee, observer).to_vec(),
        observer_only: true,
        ..Default::default()
    };
    let mut previous = 0;
    let mut records = Vec::new();
    for encoded in certificate.handoffs {
        let raw = hex::decode(encoded)?;
        let (next, change, header) = advance(store, &committee, previous, &raw)?;
        records.push((handoff_key(committee.epoch()), raw));
        journal.anchor = header.id().to_vec();
        journal.committee = change.encode();
        previous = header.height();
        committee = next;
    }
    let epoch = committee.epoch();
    let proof = hex::decode(certificate.proof)?;
    let verified = if certificate.terminal {
        let (next, change, verified) = advance(store, &committee, previous, &proof)?;
        let anchor = next.genesis_quorum();
        journal.anchor = anchor.point.header.to_vec();
        journal.committee = change.encode();
        journal.finalized = anchor.point.encode().to_vec();
        journal.high = anchor.encode();
        records.push((handoff_key(epoch), proof.clone()));
        verified
    } else {
        let verified = committee.verify_finality(&proof)?;
        let pair = FinalityProof::decode(proof.as_slice())?;
        let parent = Quorum::decode(&pair.parent_qc, committee.size())?;
        journal.lock = parent.point.view;
        journal.finalized = parent.point.encode().to_vec();
        journal.high = pair.child_qc;
        verified
    };
    ensure!(
        verified.height() > previous
            && verified.id() == pinned_header
            && verified.as_bytes().as_slice() == image.header.encode(),
        "snapshot differs from the pinned finalized Header"
    );
    records.push((
        finality_key(verified.height()),
        wire::Finalized {
            proof,
            epoch,
            handoff: certificate.terminal,
        }
        .encode_to_vec(),
    ));
    records.push((STATE.to_vec(), journal.encode_to_vec()));
    let header = image.header.clone();
    let app_hash = header.app_hash(store.network());
    store.install_certified_state(image, &app_hash, &records)?;
    Ok(header)
}
