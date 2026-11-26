use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ckb_hash::blake2b_256;
use ckb_jsonrpc_types::{
    CellWithStatus, HeaderView, JsonBytes, OutPoint as JsonPoint, TransactionAndWitnessProof,
};
use ckb_types::{
    H256,
    bytes::Bytes,
    core,
    packed::{Byte32, CellOutput, OutPoint, Transaction as CkbTransaction},
    prelude::*,
};
use clap::Args;
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};
use serde_json::json;
use stem_ledger::{
    MAX_TRANSACTION_BYTES, Transaction,
    protocol::{
        Certificate, L1Anchor,
        l1_reference::{
            L1Reference, L1ReferenceKind, MAX_REFERENCES, encode_reference_set, l1_reference_digest,
        },
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
    anchor_height: u64,
    anchor_hash: H256,
    references: Vec<Target>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Target {
    out_point: JsonPoint,
    live_at_anchor: bool,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Vote {
    records: Vec<JsonBytes>,
    signer: u16,
    signature: JsonBytes,
}

impl AttestReference {
    pub fn run(self) -> Result<()> {
        let spec = native_spec::load(&self.genesis)?;
        let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
        let request: Request = read_json_limited(&self.request, 4096)?;
        ensure!(
            (1..=MAX_REFERENCES).contains(&request.references.len()),
            "invalid L1 reference count"
        );
        let mut points = request
            .references
            .into_iter()
            .map(|target| (OutPoint::from(target.out_point), target.live_at_anchor))
            .collect::<Vec<_>>();
        points.sort_unstable_by(|a, b| a.0.as_slice().cmp(b.0.as_slice()));
        ensure!(
            points.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "L1 references must be ordered by unique OutPoint"
        );
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
        let anchor = L1Anchor {
            height: request.anchor_height,
            hash: request.anchor_hash.pack(),
        };
        let records = points
            .into_iter()
            .map(|(point, live)| observe(&rpc, &bridge.l1_genesis, &anchor, point, live))
            .collect::<Result<Vec<_>>>()?;
        let after: core::HeaderView = rpc.call::<HeaderView>("get_tip_header", json!([]))?.into();
        ensure!(
            tip.hash() == after.hash(),
            "CKB tip changed during reference observation"
        );
        let body = encode_reference_set(&records)?;
        let key = read_key(&self.key_file)?;
        let signer = bridge
            .guardians
            .iter()
            .position(|member| member.key == key.verifying_key().to_bytes())
            .context("key is absent from the configured L1 guardians")?;
        journal.pin(&rpc, request.anchor_height, &request.anchor_hash)?;
        let digest =
            l1_reference_digest(&spec.network, &bridge.guardian_generation.unpack(), &body);
        print_json(&Vote {
            records: records.into_iter().map(JsonBytes::from_bytes).collect(),
            signer: u16::try_from(signer)?,
            signature: JsonBytes::from_vec(key.sign(digest.as_slice()).to_bytes().to_vec()),
        })
    }
}

fn observe(
    rpc: &Rpc,
    genesis: &Byte32,
    anchor: &L1Anchor,
    point: OutPoint,
    live: bool,
) -> Result<Bytes> {
    let tx_hash: H256 = point.tx_hash().unpack();
    let (raw, created, block) = rpc.committed(&tx_hash)?;
    ensure!(
        created <= anchor.height && rpc.block_hash(created)? == block,
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
    let kind = if live {
        let observed: CellWithStatus = rpc.call(
            "get_live_cell",
            json!([JsonPoint::from(point.clone()), true]),
        )?;
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
    Ok(L1Reference {
        kind,
        genesis: genesis.clone(),
        out_point: point,
        creation: L1Anchor {
            height: created,
            hash: block.pack(),
        },
        anchor: anchor.clone(),
        output,
        data,
    }
    .encode())
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
        let vote_limit = bridge.guardians.len() * (2 * MAX_TRANSACTION_BYTES + 1024);
        let votes: Vec<Vote> = read_json_limited(&self.observations, vote_limit)?;
        ensure!(
            votes.len() <= bridge.guardians.len(),
            "too many L1 reference votes"
        );
        let agreed = votes.first().context("no L1 reference observations")?;
        let records = agreed
            .records
            .iter()
            .map(JsonBytes::as_bytes)
            .collect::<Vec<_>>();
        let body = encode_reference_set(&records)?;
        for record in &records {
            let reference = L1Reference::decode(record)?;
            ensure!(
                reference.genesis == bridge.l1_genesis,
                "L1 reference uses a different CKB genesis"
            );
        }
        let mut signatures = Vec::with_capacity(votes.len());
        for vote in &votes {
            ensure!(
                vote.records == agreed.records,
                "guardians observed different L1 reference sets"
            );
            signatures.push(Attestation {
                guardian: vote.signer,
                signature: vote.signature.as_bytes().try_into()?,
            });
        }
        signatures.sort_unstable_by_key(|vote| vote.guardian);
        let message = l1_reference_digest(&spec.network, &authority.generation(), &body);
        authority.verify(message.as_slice(), &signatures)?;
        let attached = tx.with_l1_reference_certificate(Certificate::new(body, signatures)?)?;
        print_json(&JsonBytes::from_vec(attached.as_slice().to_vec()))
    }
}
