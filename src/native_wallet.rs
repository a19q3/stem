use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use ckb_crypto::secp::Privkey;
use ckb_hash::blake2b_256;
use ckb_jsonrpc_types::{CellDep, JsonBytes, OutPoint, Script};
use ckb_types::{
    H256,
    bytes::Bytes,
    packed::{self, Script as PackedScript},
    prelude::*,
};
use clap::Subcommand;
use serde::Deserialize;
use stem_ledger::{Cell, CellInput, GroupAction, GroupMessage, GroupWitness, Since, Transaction};

use crate::{
    bridge_cli::{print_json, read_json_limited},
    native_spec,
};

mod intent;
mod lineage;

#[derive(Subcommand)]
pub enum Wallet {
    /// Encode a native Stem transaction from a JSON request.
    Build {
        #[arg(long)]
        request: PathBuf,
    },
    /// Read a canonical transaction from stdin and print its network-bound ID.
    Id {
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        network: H256,
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
        /// Resolved Cell and Header contents obtained from trusted proofs or a trusted node.
        #[arg(long)]
        context: PathBuf,
        #[arg(long)]
        group_index: u16,
        #[arg(long)]
        lock: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        /// Authorize a fee-only contribution of at most this many shannons from this Lock Group.
        #[arg(long)]
        sponsor_max_debit: Option<u64>,
        /// Check the deployed Intent Lock ELF before signing a cancellation.
        #[arg(long, conflicts_with = "sponsor_max_debit")]
        intent_code: Option<PathBuf>,
        /// Check the Lineage Lock ELF before signing an exact Cell spend.
        #[arg(long, conflicts_with_all = ["intent_code", "sponsor_max_debit"])]
        lineage_code: Option<PathBuf>,
    },
    /// Sign and assemble independently authorized Cells.
    Intent {
        #[command(subcommand)]
        command: intent::Command,
    },
    /// Build or sign a bounded command for registered Lineage heads.
    Lineage {
        #[command(subcommand)]
        command: lineage::Command,
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
    #[serde(default)]
    messages: Vec<Message>,
    keeper: Option<KeeperRequest>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum KeeperRequest {
    Schedule(JsonBytes),
    Cancel { height: u64, fee_input: OutPoint },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    group_index: u16,
    actions: Vec<Action>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    kind: u16,
    data: JsonBytes,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SigningContext {
    cells: Vec<SigningCell>,
    #[serde(default)]
    headers: Vec<JsonBytes>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SigningCell {
    out_point: OutPoint,
    cell: JsonBytes,
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
            Self::Id { network } => {
                use std::io::Read;
                let mut json = Vec::new();
                std::io::stdin()
                    .take(2 * stem_ledger::MAX_TRANSACTION_BYTES as u64 + 1024)
                    .read_to_end(&mut json)?;
                let encoded: JsonBytes = serde_json::from_slice(&json)?;
                let tx = Transaction::from_slice(encoded.as_bytes())?;
                return print_json(&H256::from(tx.id(&network.pack())));
            }
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
                if let Some(action) = request.keeper {
                    let action = match action {
                        KeeperRequest::Schedule(bytes) => {
                            let command = stem_ledger::keeper::decode_command(bytes.as_bytes())?;
                            stem_ledger::keeper::Action::schedule(&command)?
                        }
                        KeeperRequest::Cancel { height, fee_input } => {
                            stem_ledger::keeper::Action::Cancel {
                                height,
                                fee_input: fee_input.into(),
                            }
                        }
                    };
                    tx = tx.with_keeper_action(action)?;
                }
                tx = tx.with_messages(
                    request
                        .messages
                        .into_iter()
                        .map(|message| GroupMessage {
                            group_index: message.group_index,
                            actions: message
                                .actions
                                .into_iter()
                                .map(|action| GroupAction {
                                    kind: action.kind,
                                    data: Bytes::copy_from_slice(action.data.as_bytes()),
                                })
                                .collect(),
                        })
                        .collect(),
                )?;
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
                context,
                group_index,
                lock,
                key_file,
                sponsor_max_debit,
                intent_code,
                lineage_code,
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
                if let Some(code) = intent_code {
                    intent::verify_owner(&lock, &key, &code)?;
                    ensure!(
                        tx.witness(group_index)
                            .is_none_or(|witness| witness.proof_data.is_empty()),
                        "Intent cancellation requires empty proof data"
                    );
                } else if let Some(code) = lineage_code {
                    lineage::verify_owner(&lock, &key, &code)?;
                } else {
                    ensure!(
                        lock.args().raw_data().as_ref()
                            == &blake2b_256(key.pubkey()?.serialize())[..20],
                        "private key does not own the specified Lock Script"
                    );
                }
                let context = ResolvedSigningContext::load(&context, &network)?;
                let get_cell = |point: &packed::OutPoint| context.cell(point);
                if let Some(max_debit) = sponsor_max_debit {
                    check_sponsor(&tx, &lock, max_debit, get_cell)?;
                }
                let digest = tx.signing_digest(&network, group_index, &lock, get_cell, |id| {
                    context.header(id)
                })?;
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
            Self::Intent { command } => return command.run(),
            Self::Lineage { command } => return command.run(),
        };
        print_json(&JsonBytes::from_vec(tx.as_slice().to_vec()))
    }
}

struct ResolvedSigningContext {
    cells: BTreeMap<packed::OutPoint, JsonBytes>,
    headers: BTreeMap<packed::Byte32, stem_ledger::block::Header>,
}

impl ResolvedSigningContext {
    fn load(path: &Path, network: &packed::Byte32) -> Result<Self> {
        let source: SigningContext = read_json_limited(
            path,
            stem_ledger::MAX_RESOLVED_CELL_BYTES * 2 + 2 * 1024 * 1024,
        )?;
        let mut cells = BTreeMap::new();
        for record in source.cells {
            ensure!(
                cells.insert(record.out_point.into(), record.cell).is_none(),
                "duplicate Cell in signing context"
            );
        }
        let mut headers = BTreeMap::new();
        for header in source.headers {
            let header = stem_ledger::block::Header::decode(header.as_bytes())?;
            ensure!(
                headers.insert(header.id(network), header).is_none(),
                "duplicate Header in signing context"
            );
        }
        Ok(Self { cells, headers })
    }

    fn cell(&self, point: &packed::OutPoint) -> Result<Cell> {
        Cell::from_slice(
            self.cells
                .get(point)
                .context("signing context is missing a Cell")?
                .as_bytes(),
        )
    }

    fn header(&self, id: &packed::Byte32) -> Result<stem_ledger::block::Header> {
        self.headers
            .get(id)
            .cloned()
            .context("signing context is missing a Header")
    }
}

fn check_sponsor(
    tx: &Transaction,
    lock: &PackedScript,
    max_debit: u64,
    mut load: impl FnMut(&packed::OutPoint) -> Result<Cell>,
) -> Result<()> {
    fn capacity(
        mut cells: impl Iterator<Item = Result<Cell>>,
        sponsor_lock: &PackedScript,
    ) -> Result<(u64, u64)> {
        cells.try_fold((0u64, 0u64), |(total, sponsored), item| {
            let cell = item?;
            let total = total
                .checked_add(cell.capacity())
                .context("Capacity overflow")?;
            if cell.lock() != *sponsor_lock {
                return Ok((total, sponsored));
            }
            ensure!(
                cell.type_scripts().len() == 0 && cell.data().is_empty(),
                "sponsor inputs and change must be plain Capacity Cells"
            );
            Ok((
                total,
                sponsored
                    .checked_add(cell.capacity())
                    .context("sponsor Capacity overflow")?,
            ))
        })
    }
    let (inputs, provided) = capacity(tx.inputs().map(|input| load(&input.previous_output)), lock)?;
    let (outputs, change) = capacity(tx.outputs().map(Ok), lock)?;
    let fee = inputs
        .checked_sub(outputs)
        .context("transaction creates Capacity")?;
    ensure!(provided > 0, "no sponsor inputs");
    let debit = provided.saturating_sub(change);
    ensure!(
        debit <= max_debit && debit <= fee,
        "sponsor debit exceeds authorized fee contribution"
    );
    Ok(())
}

fn read_code_hash(path: &Path) -> Result<packed::Byte32> {
    use std::io::Read;

    let mut code = Vec::new();
    std::fs::File::open(path)?
        .take(stem_ledger::MAX_CELL_BYTES as u64 + 1)
        .read_to_end(&mut code)?;
    ensure!(
        !code.is_empty() && code.len() <= stem_ledger::MAX_CELL_BYTES,
        "invalid Lock ELF size"
    );
    Ok(blake2b_256(code).pack())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_only_sponsor_checks_the_net_debit_and_the_actual_fee() -> Result<()> {
        let sponsor = PackedScript::default();
        let trader = PackedScript::new_builder()
            .args(Bytes::from_static(b"trader"))
            .build();
        let input = packed::OutPoint::new([1; 32].pack(), 0);
        let own = packed::OutPoint::new([2; 32].pack(), 0);
        let trade = Cell::new(100, trader.clone(), vec![], Bytes::new())?;
        let paid = Cell::new(30, sponsor.clone(), vec![], Bytes::new())?;
        let load = |point: &packed::OutPoint| {
            Ok(if point == &input {
                trade.clone()
            } else {
                paid.clone()
            })
        };
        let draft = |seller| -> Result<Transaction> {
            Transaction::new(
                vec![],
                vec![],
                vec![
                    CellInput::new(input.clone(), None),
                    CellInput::new(own.clone(), None),
                ],
                vec![
                    Cell::new(seller, trader.clone(), vec![], Bytes::new())?,
                    Cell::new(20, sponsor.clone(), vec![], Bytes::new())?,
                ],
                vec![],
            )
        };
        check_sponsor(&draft(100)?, &sponsor, 10, load)?;
        assert!(check_sponsor(&draft(100)?, &sponsor, 9, load).is_err());
        assert!(check_sponsor(&draft(101)?, &sponsor, 10, load).is_err());
        Ok(())
    }
}
