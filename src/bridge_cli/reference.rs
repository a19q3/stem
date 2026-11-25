use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, ensure};
use ckb_hash::blake2b_256;
use ckb_jsonrpc_types::{
    CellWithStatus, HeaderView, JsonBytes, OutPoint as JsonPoint, TransactionAndWitnessProof,
};
use ckb_types::{
    H256, core,
    packed::{CellOutput, OutPoint, Transaction as CkbTransaction},
    prelude::*,
};
use clap::Args;
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};
use serde_json::json;
use stem_ledger::{
    Transaction,
    protocol::{
        Certificate, L1Anchor,
        l1_reference::{L1Reference, L1ReferenceKind, l1_reference_digest},
    },
};
use stem_stake::{Attestation, Authority};

use super::{check_l1, print_json, read_json_limited, read_key};
use crate::{
    l1::{Journal, Rpc},
    native_spec,
};

#[derive(Args)]
pub struct AttestReference {
    #[arg(long)]
    genesis: PathBuf,
    #[arg(long)]
    l1_rpc: String,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    journal: PathBuf,
    #[arg(long)]
    key_file: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    out_point: JsonPoint,
    anchor_height: u64,
    anchor_hash: H256,
    live_at_anchor: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Vote {
    record: JsonBytes,
    signer: u16,
    signature: JsonBytes,
}

impl AttestReference {
    pub fn run(self) -> Result<()> {
        let spec = native_spec::load(&self.genesis)?;
        let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
        let request: Request = read_json_limited(&self.request, 4096)?;
        let rpc = Rpc::new(&self.l1_rpc)?;
        check_l1(&rpc, bridge)?;
        let journal_key = blake2b_256(
            [
                b"Stem CKB reference journal\0".as_slice(),
                spec.network.as_slice(),
            ]
            .concat(),
        );
        let journal = Journal::open(&self.journal, journal_key.pack())?;
        journal.validate(&rpc)?;
        let tip: core::HeaderView = rpc.call::<HeaderView>("get_tip_header", json!([]))?.into();
        ensure!(
            request
                .anchor_height
                .checked_add(bridge.confirmations)
                .is_some_and(|confirmed| confirmed <= tip.number())
                && rpc.block_hash(request.anchor_height)? == request.anchor_hash,
            "CKB observation anchor is unconfirmed or noncanonical"
        );
        let tx_hash = request.out_point.tx_hash.clone();
        let point: OutPoint = request.out_point.clone().into();
        let (raw, created, block) = rpc.committed(&tx_hash)?;
        ensure!(
            created <= request.anchor_height && rpc.block_hash(created)? == block,
            "CKB output was created after or outside the observation anchor"
        );
        let header: core::HeaderView = rpc.call::<HeaderView>("get_header", json!([block]))?.into();
        let proof: TransactionAndWitnessProof = rpc.call(
            "get_transaction_and_witness_proof",
            json!([[tx_hash], block]),
        )?;
        let transaction = CkbTransaction::from_slice(raw.as_bytes())?.into_view();
        let (output, data) =
            stem::l1_proof::verify_output(&block.pack(), &header, &point, &transaction, &proof)?;
        let kind = if request.live_at_anchor {
            let observed: CellWithStatus =
                rpc.call("get_live_cell", json!([request.out_point, true]))?;
            ensure!(observed.status == "live", "CKB output is no longer live");
            let cell = observed.cell.context("missing live CKB output")?;
            let live_output: CellOutput = cell.output.into();
            ensure!(
                live_output == output
                    && cell
                        .data
                        .context("missing live CKB data")?
                        .content
                        .as_bytes()
                        == data.as_ref(),
                "CKB live output differs from historical inclusion"
            );
            L1ReferenceKind::LiveAtAnchor
        } else {
            L1ReferenceKind::Historical
        };
        let after: core::HeaderView = rpc.call::<HeaderView>("get_tip_header", json!([]))?.into();
        ensure!(
            tip.hash() == after.hash() && rpc.block_hash(created)? == block,
            "CKB tip changed during reference observation"
        );
        let record = L1Reference {
            kind,
            genesis: bridge.l1_genesis.clone(),
            out_point: point,
            creation: L1Anchor {
                height: created,
                hash: block.pack(),
            },
            anchor: L1Anchor {
                height: request.anchor_height,
                hash: request.anchor_hash.pack(),
            },
            output,
            data,
        }
        .encode();
        L1Reference::decode(&record)?;
        let key = read_key(&self.key_file)?;
        let signer = bridge
            .guardians
            .iter()
            .position(|member| member.key == key.verifying_key().to_bytes())
            .context("key is absent from the configured L1 guardians")?;
        journal.pin(&rpc, request.anchor_height, &request.anchor_hash)?;
        let digest =
            l1_reference_digest(&spec.network, &bridge.guardian_generation.unpack(), &record);
        print_json(&Vote {
            record: JsonBytes::from_bytes(record),
            signer: u16::try_from(signer)?,
            signature: JsonBytes::from_vec(key.sign(digest.as_slice()).to_bytes().to_vec()),
        })
    }
}

#[derive(Args)]
pub struct AttachReferences {
    #[arg(long)]
    genesis: PathBuf,
    #[arg(long)]
    transaction: PathBuf,
    #[arg(long)]
    observations: PathBuf,
}

impl AttachReferences {
    pub fn run(self) -> Result<()> {
        let spec = native_spec::load(&self.genesis)?;
        let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
        let authority = Authority::new(bridge.guardian_generation.unpack(), &bridge.guardians)?;
        let bytes: JsonBytes = read_json_limited(&self.transaction, 2 * 1024 * 1024)?;
        let tx = Transaction::from_slice(bytes.as_bytes())?;
        let votes: Vec<Vote> = read_json_limited(&self.observations, 1024 * 1024)?;
        ensure!(!votes.is_empty(), "no L1 reference observations");
        let mut records = BTreeMap::<Vec<u8>, (JsonBytes, Vec<Attestation>)>::new();
        for vote in votes {
            let reference = L1Reference::decode(vote.record.as_bytes())?;
            ensure!(
                reference.genesis == bridge.l1_genesis,
                "L1 reference uses a different CKB genesis"
            );
            let (body, votes) = records
                .entry(reference.out_point.as_slice().to_vec())
                .or_insert_with(|| (vote.record.clone(), Vec::new()));
            ensure!(
                *body == vote.record,
                "guardians observed different L1 references"
            );
            votes.push(Attestation {
                guardian: vote.signer,
                signature: vote.signature.as_bytes().try_into()?,
            });
        }
        let certificates = records
            .into_values()
            .map(|(record, mut votes)| {
                votes.sort_by_key(|vote| vote.guardian);
                let message =
                    l1_reference_digest(&spec.network, &authority.generation(), record.as_bytes());
                authority.verify(message.as_slice(), &votes)?;
                Certificate::new(record.into_bytes(), votes)
            })
            .collect::<Result<Vec<_>>>()?;
        let attached = tx.with_l1_references(certificates)?;
        print_json(&JsonBytes::from_vec(attached.as_slice().to_vec()))
    }
}
