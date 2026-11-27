//! Replay signed execution evidence from recorded native validator restarts.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::*,
};
use serde::Deserialize;
use stem_ledger::{
    Transaction,
    block::{Header, Receipt},
    light::{FinalityVerifier, VerifiedHeader},
    patricia::{Namespace, Patricia, Proof},
    proof::InclusionProof,
    record,
};
use stem_stake::Member;

#[derive(Deserialize)]
struct Corpus {
    name: String,
    network: String,
    genesis: String,
    members: Vec<RosterMember>,
    observations: Vec<Observation>,
}

#[derive(Deserialize)]
struct RosterMember {
    ed25519: String,
    power: u64,
}

#[derive(Deserialize)]
struct Observation {
    height: u64,
    header: String,
    app_hash: String,
    finality: String,
    #[serde(default)]
    terminal: bool,
    #[serde(default)]
    requires_prior_handoff: bool,
    cell: Option<CellEvidence>,
    execution: Option<ExecutionEvidence>,
}

#[derive(Deserialize)]
struct CellEvidence {
    point: String,
    cell: Option<String>,
    proof: String,
}

#[derive(Deserialize)]
struct ExecutionEvidence {
    transaction: String,
    receipt: String,
    proof: String,
    id: String,
}

fn bytes(hex: &str) -> Result<Vec<u8>> {
    Ok(hex::decode(hex.trim_start_matches("0x"))?)
}

#[test]
fn replay_signed_native_restart_and_committee_observations() -> Result<()> {
    let cases: Vec<Corpus> =
        serde_json::from_str(include_str!("fixtures/jolteon_restart_proofs.json"))?;
    assert_eq!(cases.len(), 4);
    assert_eq!(
        cases.iter().map(|case| case.observations.len()).sum::<usize>(),
        13
    );
    for case in cases {
        replay(&case).with_context(|| format!("{} restart corpus", case.name))?;
    }
    Ok(())
}

fn replay(case: &Corpus) -> Result<()> {
    let network = Byte32::from_slice(&bytes(&case.network)?)?;
    let genesis = Byte32::from_slice(&bytes(&case.genesis)?)?;
    let members = case
        .members
        .iter()
        .map(|member| {
            Ok(Member {
                key: bytes(&member.ed25519)?
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("invalid member key size"))?,
                power: member.power,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut verifier = FinalityVerifier::new(network.clone(), genesis.clone(), &members)?;
    let initial = FinalityVerifier::new(network.clone(), genesis, &members)?;
    let mut headers = BTreeMap::<u64, Header>::new();
    let mut first_handoff: Option<VerifiedHeader> = None;
    for sample in &case.observations {
        let proof = bytes(&sample.finality)?;
        if sample.requires_prior_handoff {
            assert!(initial.verify(&proof, false).is_err());
        }
        let verified = verifier.verify(&proof, sample.terminal)?;
        let header = verified.header();
        assert_eq!(header.context.height, sample.height);
        assert_eq!(header.encode().as_ref(), bytes(&sample.header)?);
        assert_eq!(header.app_hash(&network).as_slice(), bytes(&sample.app_hash)?);
        if let Some(previous) = headers.insert(sample.height, header.clone()) {
            assert_eq!(&previous, header);
        }
        if let Some(cell) = &sample.cell {
            let point = OutPoint::from_slice(&bytes(&cell.point)?)?;
            let encoded = cell.cell.as_deref().map(bytes).transpose()?;
            let stored = encoded.as_deref().map(record::decode_cell).transpose()?;
            if let Some((stored, _)) = &stored {
                assert_eq!(stored, &point);
            }
            let tree = Patricia::new(network.clone(), Namespace::Cells);
            assert!(record::verify_cell(
                &tree,
                &header.state.cell_root.unpack(),
                &point,
                stored.as_ref().map(|(_, cell)| cell),
                &Proof::decode(&bytes(&cell.proof)?)?,
            ));
        }
        if let Some(execution) = &sample.execution {
            let transaction = Transaction::from_slice(&bytes(&execution.transaction)?)?;
            let receipt = Receipt::decode(&bytes(&execution.receipt)?)?;
            assert_eq!(transaction.id(&network).as_slice(), bytes(&execution.id)?);
            let (tx_path, receipt_path) =
                InclusionProof::decode_execution_pair(&bytes(&execution.proof)?)?;
            assert!(tx_path.verify_execution(
                &network,
                &header.execution(),
                &transaction,
                &receipt_path,
                &receipt,
            ));
        }
        if sample.terminal {
            let epoch = verifier.epoch();
            let boundary = verifier.accept_handoff(&proof)?;
            assert_eq!(boundary.header(), header);
            assert!(verifier.epoch() > epoch);
            assert!(verifier.accept_handoff(&proof).is_err());
            first_handoff.get_or_insert(boundary);
        }
        if let Some(boundary) = &first_handoff {
            assert!(verified.same_trust_anchor(boundary));
        }
    }
    for (&height, header) in &headers {
        if let Some(previous) = height.checked_sub(1).and_then(|height| headers.get(&height)) {
            assert_eq!(header.context.parent, previous.id(&network));
        }
    }
    Ok(())
}
