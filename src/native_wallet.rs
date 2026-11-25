use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context, Result, ensure};
use ckb_crypto::secp::Privkey;
use ckb_hash::blake2b_256;
use ckb_jsonrpc_types::{CellDep, JsonBytes, OutPoint, Script};
use ckb_types::{H256, bytes::Bytes, packed::Script as PackedScript, prelude::*};
use clap::Subcommand;
use stem_ledger::{Cell, CellInput, GroupWitness, Since, Transaction};
use serde::Deserialize;

use crate::{
    bridge_cli::{print_json, read_json_limited},
    native_spec,
};

#[derive(Subcommand)]
pub enum Wallet {
    /// Encode a native Stem transaction from a JSON request.
    Build {
        #[arg(long)]
        request: PathBuf,
    },
    /// Discover Live Cells by complete Lock or Type Script hash.
    Find {
        #[arg(long)]
        database: PathBuf,
        #[arg(long, value_parser = crate::bridge_cli::parse_hash, required_unless_present = "type_hash", conflicts_with = "type_hash")]
        lock_hash: Option<H256>,
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        type_hash: Option<H256>,
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        after: Option<H256>,
        #[arg(long)]
        height: Option<u64>,
        #[arg(long, default_value_t = 64)]
        limit: usize,
    },
    /// Sign one native Lock Script Group and add its compact seal.
    Sign {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        transaction: PathBuf,
        #[arg(long)]
        group_index: u16,
        #[arg(long)]
        lock: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Draft {
    inputs: Vec<Input>,
    outputs: Vec<JsonBytes>,
    #[serde(default)]
    cell_deps: Vec<CellDep>,
    #[serde(default)]
    header_deps: Vec<H256>,
    valid_before_height: Option<u64>,
    #[serde(default)]
    proofs: Vec<Proof>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    out_point: OutPoint,
    since: Option<LockTime>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum LockTime {
    AbsoluteHeight(u64),
    RelativeBlocks(u64),
    AbsoluteTime(u64),
    RelativeTime(u64),
}

impl From<LockTime> for Since {
    fn from(lock: LockTime) -> Self {
        match lock {
            LockTime::AbsoluteHeight(value) => Self::AbsoluteHeight(value),
            LockTime::RelativeBlocks(value) => Self::RelativeBlocks(value),
            LockTime::AbsoluteTime(value) => Self::AbsoluteTime(value),
            LockTime::RelativeTime(value) => Self::RelativeTime(value),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    group_index: u16,
    proof_data: JsonBytes,
}

impl Wallet {
    pub fn run(self) -> Result<()> {
        let tx = match self {
            Self::Find {
                database,
                lock_hash,
                type_hash,
                after,
                height,
                limit,
            } => {
                use stem::native_store::{ScriptQuery, find_cells};
                let query = match (lock_hash, type_hash) {
                    (Some(hash), None) => ScriptQuery::Lock(hash.pack()),
                    (None, Some(hash)) => ScriptQuery::Type(hash.pack()),
                    _ => anyhow::bail!("choose exactly one complete Script hash"),
                };
                let after = after
                    .as_ref()
                    .map(|hash| hash.as_bytes().try_into())
                    .transpose()?;
                let page = find_cells(&database, &query, after, limit, height)?;
                let cells = page
                    .cells
                    .iter()
                    .map(|(point, record)| {
                        serde_json::json!({
                            "out_point": OutPoint::from(point.clone()),
                            "cell": JsonBytes::from_vec(record.cell.as_slice().to_vec()),
                            "created_height": record.origin.height,
                            "created_time_ms": record.origin.timestamp_ms,
                        })
                    })
                    .collect::<Vec<_>>();
                return print_json(&serde_json::json!({
                    "height": page.header.context.height,
                    "cell_root": format!("0x{}", hex::encode(page.header.state.cell_root.as_slice())),
                    "cells": cells,
                    "next": page.next.map(H256::from),
                }));
            }
            Self::Build { request } => {
                let request: Draft = read_json_limited(&request, 2 * 1024 * 1024)?;
                let mut tx = Transaction::new(
                    request.cell_deps.into_iter().map(Into::into).collect(),
                    request
                        .header_deps
                        .into_iter()
                        .map(|hash| hash.pack())
                        .collect(),
                    request
                        .inputs
                        .into_iter()
                        .map(|input| {
                            CellInput::new(input.out_point.into(), input.since.map(Into::into))
                        })
                        .collect(),
                    request
                        .outputs
                        .iter()
                        .map(|output| Cell::from_slice(output.as_bytes()))
                        .collect::<Result<_>>()?,
                    vec![],
                )?;
                if let Some(height) = request.valid_before_height {
                    tx = tx.with_valid_before_height(height)?;
                }
                tx.with_witnesses(
                    request
                        .proofs
                        .into_iter()
                        .map(|proof| GroupWitness {
                            group_index: proof.group_index,
                            proof_data: Bytes::copy_from_slice(proof.proof_data.as_bytes()),
                            seal: Bytes::new(),
                        })
                        .collect(),
                )?
            }
            Self::Sign {
                genesis,
                transaction,
                group_index,
                lock,
                key_file,
            } => {
                let network = native_spec::load(&genesis)?.network;
                let transaction: JsonBytes = read_json_limited(&transaction, 2 * 1024 * 1024)?;
                let tx = Transaction::from_slice(transaction.as_bytes())?;
                ensure!(
                    tx.operation().is_none(),
                    "bridge and staking transactions use guardian certificates"
                );
                let lock: Script = read_json_limited(&lock, 64 * 1024)?;
                let lock: PackedScript = lock.into();
                let key = key(&key_file)?;
                ensure!(
                    lock.args().raw_data().as_ref()
                        == &blake2b_256(key.pubkey()?.serialize())[..20],
                    "private key does not own the specified Lock Script"
                );
                let digest = tx.lock_digest(&network, &lock);
                let signature = key.sign_recoverable(&digest.unpack())?;
                let mut witnesses = tx
                    .witnesses()
                    .map(|witness| (witness.group_index, witness))
                    .collect::<BTreeMap<_, _>>();
                let proof = witnesses
                    .remove(&group_index)
                    .map_or_else(Bytes::new, |witness| witness.proof_data);
                witnesses.insert(
                    group_index,
                    GroupWitness {
                        group_index,
                        proof_data: proof,
                        seal: Bytes::from(signature.serialize()),
                    },
                );
                tx.with_witnesses(witnesses.into_values().collect())?
            }
        };
        print_json(&JsonBytes::from_vec(tx.as_slice().to_vec()))
    }
}

fn key(path: &std::path::Path) -> Result<Privkey> {
    use std::{fs::File, io::Read, os::unix::fs::PermissionsExt};
    let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "wallet key file must be private"
    );
    let mut bytes = Vec::new();
    file.take(33).read_to_end(&mut bytes)?;
    ensure!(bytes.len() == 32, "wallet key must be 32 raw bytes");
    Ok(Privkey::from_slice(&bytes))
}
