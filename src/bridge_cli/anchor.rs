use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{JsonBytes, Uint64};
use ckb_types::{H256, bytes::Bytes, prelude::*};
use clap::Args;
use ed25519_dalek::Signer;
use serde::{Deserialize, Serialize};
use serde_json::json;
use stem_ledger::{
    Transaction,
    protocol::{Certificate, L1Anchor, L1AnchorUpdate, Operation},
};
use stem_stake::Attestation;

use super::{check_l1, parse_hash, print_json, read_json_limited, read_key};
use crate::{
    l1::{Journal, Rpc},
    native_spec,
};

#[derive(Args)]
pub struct AttestAnchor {
    #[arg(long)]
    genesis: PathBuf,
    #[arg(long)]
    l1_rpc: String,
    #[arg(long)]
    previous_height: u64,
    #[arg(long, value_parser = parse_hash)]
    previous_hash: H256,
    #[arg(long)]
    height: u64,
    #[arg(long)]
    journal: PathBuf,
    #[arg(long)]
    key_file: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignedUpdate {
    statement: JsonBytes,
    signer: u16,
    signature: JsonBytes,
}

impl AttestAnchor {
    pub fn run(self) -> Result<()> {
        let spec = native_spec::load(&self.genesis)?;
        let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
        let key = read_key(&self.key_file)?;
        let signer = bridge
            .guardians
            .iter()
            .position(|member| member.key == key.verifying_key().to_bytes())
            .context("key is absent from the configured bridge guardians")?;
        ensure!(
            self.height > self.previous_height,
            "L1 anchor update must advance"
        );
        let rpc = Rpc::new(&self.l1_rpc)?;
        check_l1(&rpc, bridge)?;
        let journal = Journal::open(&self.journal, spec.network.clone())?;
        journal.pin(&rpc, self.previous_height, &self.previous_hash)?;
        let tip = rpc
            .call::<Uint64>("get_tip_block_number", json!([]))?
            .value();
        ensure!(
            self.height
                .checked_add(bridge.confirmations)
                .is_some_and(|height| height <= tip),
            "CKB anchor update has insufficient confirmations"
        );
        let confirmed_hash = rpc.block_hash(self.height)?;
        let update = L1AnchorUpdate {
            l1_genesis: bridge.l1_genesis.clone(),
            authority: bridge.guardian_generation.clone(),
            previous_anchor: L1Anchor {
                height: self.previous_height,
                hash: self.previous_hash.pack(),
            },
            anchor: L1Anchor {
                height: self.height,
                hash: confirmed_hash.pack(),
            },
        };
        update.validate()?;
        journal.pin(&rpc, self.height, &confirmed_hash)?;
        ensure!(
            rpc.block_hash(self.previous_height)? == self.previous_hash,
            "guardian halted: predecessor L1 anchor changed during approval"
        );
        let digest = update.digest(&spec.network);
        print_json(&SignedUpdate {
            statement: JsonBytes::from_bytes(update.encode()),
            signer: u16::try_from(signer)?,
            signature: JsonBytes::from_vec(key.sign(digest.as_slice()).to_bytes().to_vec()),
        })
    }
}

pub fn assemble(observations: &Path) -> Result<()> {
    let mut signed: Vec<SignedUpdate> = read_json_limited(observations, 256 * 1024)?;
    signed.sort_unstable_by_key(|vote| vote.signer);
    let encoded = &signed.first().context("no L1 anchor approvals")?.statement;
    let statement = L1AnchorUpdate::decode(encoded.as_bytes())?;
    let votes = signed
        .iter()
        .map(|vote| {
            ensure!(
                vote.statement == *encoded,
                "guardians approved different L1 anchor updates"
            );
            Ok(Attestation {
                guardian: vote.signer,
                signature: vote.signature.as_bytes().try_into()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let tx = Transaction::control(
        Operation::L1AnchorUpdate(statement),
        Certificate::new(Bytes::new(), votes)?,
    )?;
    print_json(&JsonBytes::from_vec(tx.as_slice().to_vec()))
}
