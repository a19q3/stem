//! A permissionless publisher driven only by finalized Stem history and the local CKB node.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use ckb_jsonrpc_types::{
    JsonBytes, OutPoint, Status, Transaction as JsonTransaction, TransactionWithStatusResponse,
    TxStatus, Uint64,
};
use ckb_types::{
    H256,
    bytes::Bytes,
    core::{Capacity, DepType, ScriptHashType, TransactionBuilder, TransactionView},
    packed::{self, Byte32, CellDep, CellInput, CellOutput, Script, WitnessArgs},
    prelude::*,
};
use clap::Args;
use prost::Message;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use stem::{jolteon::Client, native_store::HistoryIndex};
use stem_ledger::block::Header;
use stem_stake::jolteon::{CommitteeChange, HandoffProof, checkpoint as jolteon};

use super::{Bundle, build_jolteon_bundle, fund, read_hex};
use crate::{
    bridge_cli::{parse_hash, print_json, read_json_limited},
    ckb_wallet,
    l1::{Journal, Rpc},
};

mod adoption;
mod follow;

const SIGHASH_TYPE: &str = "9bd7e06f3ecf4be0f2fcd2188b23f1b9fcc88e5d4b65a8637b17723bbda3cce8";

#[derive(Args)]
pub struct Publish {
    /// Publisher configuration JSON. Use a private journal for each checkpoint and fee key.
    #[arg(long)]
    config: PathBuf,
    /// Publish this finalized Stem height on demand, after any preceding handoffs.
    #[arg(long)]
    at_least_height: Option<u64>,
    /// Pay for this update from the specified PoW Security Fund Cell.
    #[arg(long)]
    fee_fund: Option<PathBuf>,
    /// Adopt a confirmed successor by transaction hash. Repeat in spend order.
    #[arg(long, conflicts_with_all = ["at_least_height", "fee_fund"])]
    adopt: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    l1: String,
    replica: SocketAddr,
    archive_db: PathBuf,
    index: PathBuf,
    journal: PathBuf,
    policy: JsonBytes,
    initial_roster: PathBuf,
    checkpoint_code: OutPoint,
    checkpoint_code_hash: H256,
    seed: OutPoint,
    fee_dep_group: OutPoint,
    fee_key: PathBuf,
    /// Independent fee input after the shared checkpoint has been bootstrapped.
    update_funding: Option<OutPoint>,
    checkpoint_capacity: u64,
    fee: u64,
    interval_seconds: u64,
    /// Number of canonical CKB blocks required after the inclusion block.
    confirmations: u64,
    /// Discover confirmed competing checkpoint successors with the CKB Indexer.
    #[serde(default)]
    auto_follow: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FundInput {
    out_point: OutPoint,
    code: OutPoint,
}

#[derive(Deserialize, Serialize)]
struct State {
    identity: H256,
    confirmed: Option<Confirmed>,
    pending: Option<Pending>,
}

#[derive(Deserialize, Serialize)]
struct Confirmed {
    transaction: H256,
    #[serde(default)]
    output_index: u32,
    l1_height: u64,
    l1_block: H256,
    tip: JsonBytes,
    roster: JsonBytes,
    // Only needed when the checkpoint transaction returned change to the fund.
    #[serde(default)]
    wallet_change: Option<OutPoint>,
}

#[derive(Deserialize, Serialize)]
struct Pending {
    signed: JsonBytes,
    next_roster: JsonBytes,
    witness_bytes: usize,
    estimated_cycles: u64,
    #[serde(default)]
    uses_fund: bool,
}

struct Candidate {
    height: u64,
    proof: Vec<u8>,
    terminal: bool,
}

struct Publisher {
    config: Config,
    node: Rpc,
    client: Client,
    journal: Journal,
    policy: jolteon::Policy,
    checkpoint_type: Script,
    checkpoint_lock: Script,
    fee_lock: Script,
    initial_roster: Vec<u8>,
    interval_ms: u64,
}

impl Publish {
    pub fn run(self) -> Result<()> {
        let config = read_json_limited(&self.config, 64 * 1024)?;
        let (publisher, mut state) = Publisher::open(config)?;
        if !self.adopt.is_empty() {
            let hashes = self
                .adopt
                .iter()
                .map(|hash| parse_hash(hash))
                .collect::<Result<Vec<_>>>()?;
            return print_json(&publisher.adopt(&mut state, &hashes)?);
        }
        let fund = self
            .fee_fund
            .as_deref()
            .map(|path| read_json_limited(path, 4096))
            .transpose()?;
        print_json(&publisher.tick(&mut state, self.at_least_height, fund)?)
    }
}

impl Publisher {
    fn open(config: Config) -> Result<(Self, State)> {
        ensure!(
            config.fee > 0 && config.interval_seconds > 0 && config.confirmations > 0,
            "specify a positive fee, publication interval and CKB confirmation depth"
        );
        let interval_ms = config
            .interval_seconds
            .checked_mul(1000)
            .context("interval overflow")?;
        let policy = jolteon::Policy::decode(config.policy.as_bytes())?;
        let initial_roster = read_hex(&config.initial_roster)?;
        ensure!(
            jolteon::roster_id(&initial_roster) == policy.initial_roster
                && CommitteeChange::decode(&initial_roster)?.epoch == 0,
            "initial roster differs from the pinned checkpoint policy"
        );
        let seed = CellInput::new(config.seed.clone().into(), 0);
        let expected_id = ckb_hash::blake2b_256([seed.as_slice(), &0u64.to_le_bytes()].concat());
        ensure!(
            config.policy.as_bytes()[..32] == expected_id,
            "seed input does not create the pinned checkpoint Type ID"
        );
        let checkpoint_type = Script::new_builder()
            .code_hash(Byte32::from(&config.checkpoint_code_hash))
            .hash_type(ScriptHashType::Data2)
            .args(config.policy.as_bytes().pack())
            .build();
        let checkpoint_lock = checkpoint_type
            .clone()
            .as_builder()
            .args(checkpoint_type.calc_script_hash().as_slice().pack())
            .build();
        let (_, fee_args) = ckb_wallet::public_key(&config.fee_key)?;
        let fee_lock = Script::new_builder()
            .code_hash(Byte32::from(&parse_hash(SIGHASH_TYPE)?))
            .hash_type(ScriptHashType::Type)
            .args(fee_args.as_slice().pack())
            .build();
        let node = Rpc::new(&config.l1)?;
        ensure!(
            node.block_hash(0)?.as_bytes() == &policy.ckb_genesis,
            "L1 RPC serves a different CKB genesis"
        );
        let identity = ckb_hash::blake2b_256(
            [
                &b"Stem checkpoint publisher configuration\0"[..],
                checkpoint_type.as_slice(),
                packed::OutPoint::from(config.checkpoint_code.clone()).as_slice(),
                packed::OutPoint::from(config.fee_dep_group.clone()).as_slice(),
                fee_lock.as_slice(),
                &config.checkpoint_capacity.to_le_bytes(),
                &config.confirmations.to_le_bytes(),
            ]
            .concat(),
        );
        let journal = Journal::open(&config.journal, [0u8; 32].pack())?;
        let state: State = journal.read_record()?.unwrap_or(State {
            identity: identity.into(),
            confirmed: None,
            pending: None,
        });
        ensure!(
            state.identity.as_bytes() == &identity,
            "journal belongs to a different checkpoint or wallet configuration"
        );
        let client = Client::new(config.replica, Duration::from_secs(10));
        Ok((
            Self {
                config,
                node,
                client,
                journal,
                policy,
                checkpoint_type,
                checkpoint_lock,
                fee_lock,
                initial_roster,
                interval_ms,
            },
            state,
        ))
    }

    fn tick(
        &self,
        state: &mut State,
        requested: Option<u64>,
        fund: Option<FundInput>,
    ) -> Result<Value> {
        self.validate_anchor(state.confirmed.as_ref())?;
        if self.config.auto_follow
            && let Some(result) = self.follow(state)?
        {
            return Ok(result);
        }
        if state.pending.is_some() {
            return self.resume(state);
        }

        let previous = state
            .confirmed
            .as_ref()
            .map(|entry| jolteon::Tip::decode(entry.tip.as_bytes()))
            .transpose()?;
        let roster = state
            .confirmed
            .as_ref()
            .map_or(self.initial_roster.as_slice(), |entry| {
                entry.roster.as_bytes().as_ref()
            });
        let committee = CommitteeChange::decode(roster)?;
        let latest = self.client.status()?.height;
        let pinned = previous.as_ref().map_or(0, jolteon::Tip::height);
        let Some(candidate) =
            self.candidate(latest, previous.as_ref(), committee.epoch, requested)?
        else {
            return Ok(
                json!({"status": "idle", "stem_height": pinned, "latest_finalized": latest}),
            );
        };
        let mut index = HistoryIndex::open(
            &self.config.archive_db,
            &self.config.index,
            self.policy.network.pack(),
        )?;
        index.sync(candidate.height)?;
        let bundle = build_jolteon_bundle(
            &index,
            &self.policy,
            candidate.height,
            previous.as_ref(),
            roster.to_vec(),
            candidate.proof,
            candidate.terminal,
        )?;
        let uses_fund = fund.is_some();
        let signed = if let Some(fund) = fund {
            let previous = state
                .confirmed
                .as_ref()
                .context("initialize the checkpoint with the fee wallet first")?;
            let funded = fund::Update {
                checkpoint: previous.point(),
                checkpoint_code: self.config.checkpoint_code.clone(),
                fund: fund.out_point,
                fund_code: fund.code,
                fee: self.config.fee,
                witness: JsonBytes::from_vec(bundle.witness.clone()),
            }
            .build(&self.node)?;
            ensure!(
                funded.tip == bundle.tip,
                "funded checkpoint differs from the journal update"
            );
            funded.transaction
        } else {
            self.build_transaction(state.confirmed.as_ref(), &bundle)?
        };
        let json = JsonTransaction::from(signed.data());
        let estimate: Cycles = self.node.call("estimate_cycles", json!([json]))?;
        self.validate_anchor(state.confirmed.as_ref())?;
        state.pending = Some(Pending {
            signed: JsonBytes::from_bytes(signed.data().as_bytes()),
            next_roster: JsonBytes::from_vec(bundle.next_roster),
            witness_bytes: bundle.witness.len(),
            estimated_cycles: estimate.cycles.value(),
            uses_fund,
        });
        // The next invocation must be able to resume even if the RPC receives the
        // transaction and the process dies before it reads the RPC reply.
        self.journal.write_record(state)?;
        self.resume(state)
    }

    fn candidate(
        &self,
        latest: u64,
        previous: Option<&jolteon::Tip>,
        epoch: u64,
        requested: Option<u64>,
    ) -> Result<Option<Candidate>> {
        let pinned = previous.map_or(0, jolteon::Tip::height);
        if latest <= pinned {
            return Ok(None);
        }
        let requested = requested.filter(|height| pinned < *height && *height <= latest);
        let target = requested.unwrap_or(latest);
        let certificate = self.client.finality(target)?;
        ensure!(
            certificate.epoch >= epoch,
            "replica returned a regressed checkpoint epoch"
        );
        if certificate.epoch > epoch {
            let proof = self.client.handoff(epoch)?;
            let terminal = HandoffProof::decode(proof.as_slice())?;
            let height = Header::decode(&terminal.boundary_header)?.context.height;
            ensure!(
                height > pinned && height <= target,
                "handoff outside finalized interval"
            );
            return Ok(Some(Candidate {
                height,
                proof,
                terminal: true,
            }));
        }
        if !certificate.handoff
            && requested.is_none()
            && let Some(tip) = previous
        {
            let last = Header::decode(&tip.header)?.context.timestamp_ms;
            let now = self.client.header(target)?.context.timestamp_ms;
            if now.saturating_sub(last) < self.interval_ms {
                return Ok(None);
            }
        }
        Ok(Some(Candidate {
            height: target,
            proof: certificate.proof,
            terminal: certificate.handoff,
        }))
    }

    fn validate_anchor(&self, previous: Option<&Confirmed>) -> Result<()> {
        if let Some(previous) = previous {
            let block = self
                .node
                .block_hash(previous.l1_height)
                .context("confirmed checkpoint anchor is unavailable")?;
            ensure!(
                block == previous.l1_block,
                "confirmed checkpoint anchor was reorganized"
            );
        }
        Ok(())
    }

    fn build_transaction(
        &self,
        previous: Option<&Confirmed>,
        bundle: &Bundle,
    ) -> Result<TransactionView> {
        let code = self
            .node
            .live_cell(&self.config.checkpoint_code)?
            .data
            .context("CKB omitted checkpoint code")?;
        ensure!(
            ckb_hash::blake2b_256(code.content.as_bytes())
                == *self.config.checkpoint_code_hash.as_bytes(),
            "CKB code dep differs from the pinned checkpoint code hash"
        );
        let mut inputs = Vec::with_capacity(2);
        let (fee_point, checkpoint_capacity) = match previous {
            Some(previous) => {
                let point = previous.point();
                let cell = self.node.live_cell(&point)?;
                ensure!(
                    packed::Script::from(cell.output.lock) == self.checkpoint_lock
                        && cell.output.type_.map(Script::from).as_ref()
                            == Some(&self.checkpoint_type)
                        && cell.data.is_some_and(|data| data.content == previous.tip),
                    "live checkpoint differs from the journal"
                );
                inputs.push(CellInput::new(point.into(), 0));
                (self.wallet_point(previous), cell.output.capacity.value())
            }
            None => (self.config.seed.clone(), self.config.checkpoint_capacity),
        };
        let fee = self.node.live_cell(&fee_point)?;
        ensure!(
            packed::Script::from(fee.output.lock) == self.fee_lock
                && fee.output.type_.is_none()
                && fee.data.is_some_and(|data| data.content.is_empty()),
            "funding Cell must be a plain CKB Cell locked by the fee key"
        );
        inputs.push(CellInput::new(fee_point.into(), 0));

        let change = fee
            .output
            .capacity
            .value()
            .checked_sub(self.config.fee)
            .and_then(|change| {
                change.checked_sub(if previous.is_some() {
                    0
                } else {
                    checkpoint_capacity
                })
            })
            .context("publisher funding cannot cover the checkpoint and fee")?;
        let checkpoint = CellOutput::new_builder()
            .capacity(checkpoint_capacity)
            .lock(self.checkpoint_lock.clone())
            .type_(Some(self.checkpoint_type.clone()).pack())
            .build();
        let change = CellOutput::new_builder()
            .capacity(change)
            .lock(self.fee_lock.clone())
            .build();
        let tip = bundle.tip.encode();
        ensure!(
            !checkpoint.is_lack_of_capacity(Capacity::bytes(tip.len())?)?
                && !change.is_lack_of_capacity(Capacity::zero())?,
            "checkpoint or fee change is below its occupied capacity"
        );
        let witness = WitnessArgs::new_builder()
            .input_type(Some(Bytes::copy_from_slice(&bundle.witness)).pack())
            .build();
        let fee_index = inputs.len() - 1;
        let tx = TransactionBuilder::default()
            .set_inputs(inputs)
            .output(checkpoint)
            .output(change)
            .output_data(tip.pack())
            .output_data(packed::Bytes::default())
            .cell_dep(
                CellDep::new_builder()
                    .out_point(self.config.checkpoint_code.clone())
                    .build(),
            )
            .cell_dep(
                CellDep::new_builder()
                    .out_point(self.config.fee_dep_group.clone())
                    .dep_type(DepType::DepGroup)
                    .build(),
            )
            .witness(witness.as_bytes().pack())
            .build();
        ckb_wallet::sign_single(&tx, fee_index, &self.config.fee_key)
    }

    fn resume(&self, state: &mut State) -> Result<Value> {
        let pending = state
            .pending
            .as_ref()
            .context("missing pending publication")?;
        let signed = packed::Transaction::from_slice(pending.signed.as_bytes())?.into_view();
        let hash = H256::from(signed.hash());
        let tip = jolteon::Tip::decode(
            &signed
                .outputs_data()
                .get(0)
                .context("missing checkpoint output")?
                .raw_data(),
        )?;
        let next = CommitteeChange::decode(pending.next_roster.as_bytes())?;
        ensure!(
            jolteon::roster_id(pending.next_roster.as_bytes()) == tip.roster,
            "pending roster differs from the checkpoint output"
        );
        let mut result = json!({
            "status": "submitted", "transaction": hash, "stem_height": tip.height(),
            "next_epoch": next.epoch, "estimated_cycles": pending.estimated_cycles,
            "witness_bytes": pending.witness_bytes,
            "fee_source": if pending.uses_fund { "fund" } else { "wallet" },
        });
        if pending.uses_fund {
            result["fund_change"] = json!(out_point(&hash, 1));
        }
        let status = self.transaction_status(&hash, false)?;
        match status.status {
            Status::Unknown => {
                let submitted: H256 = self.node.call(
                    "send_transaction",
                    json!([JsonTransaction::from(signed.data()), "passthrough"]),
                )?;
                ensure!(
                    submitted == hash,
                    "CKB returned a different transaction hash"
                );
            }
            Status::Pending | Status::Proposed => {
                result["status"] = json!("in_pool");
            }
            Status::Rejected => bail!("CKB rejected the journaled checkpoint: {:?}", status.reason),
            Status::Committed => {
                let height = status
                    .block_number
                    .context("missing CKB inclusion height")?
                    .value();
                let block = status.block_hash.context("missing CKB inclusion block")?;
                ensure!(
                    self.node.block_hash(height)? == block,
                    "checkpoint inclusion reorganized"
                );
                let current: Uint64 = self.node.call("get_tip_block_number", json!([]))?;
                result["status"] = json!("awaiting_confirmations");
                result["l1_height"] = json!(height);
                result["l1_block"] = json!(block);
                let depth = current
                    .value()
                    .checked_sub(height)
                    .context("CKB tip regressed")?;
                result["depth"] = json!(depth);
                if depth >= self.config.confirmations {
                    self.validate_anchor(state.confirmed.as_ref())?;
                    ensure!(
                        self.node.block_hash(height)? == block,
                        "checkpoint inclusion reorganized"
                    );
                    let (_, canonical_height, canonical_block) = self.node.committed(&hash)?;
                    ensure!(
                        canonical_height == height && canonical_block == block,
                        "checkpoint inclusion changed during confirmation"
                    );
                    let wallet_change = if pending.uses_fund {
                        self.retained_wallet(state.confirmed.as_ref())?
                    } else {
                        Some(out_point(&hash, 1))
                    };
                    state.confirmed = Some(Confirmed {
                        transaction: hash,
                        output_index: 0,
                        l1_height: height,
                        l1_block: block,
                        tip: JsonBytes::from_vec(tip.encode()),
                        roster: pending.next_roster.clone(),
                        wallet_change,
                    });
                    state.pending = None;
                    self.journal.write_record(state)?;
                    result["status"] = json!("confirmed");
                }
            }
        }
        Ok(result)
    }

    fn transaction_status(&self, hash: &H256, only_committed: bool) -> Result<TxStatus> {
        let response: TransactionWithStatusResponse = self
            .node
            .call("get_transaction", json!([hash, "0x1", only_committed]))?;
        Ok(response.tx_status)
    }

    fn wallet_point(&self, previous: &Confirmed) -> OutPoint {
        previous
            .wallet_change
            .clone()
            .or_else(|| self.config.update_funding.clone())
            .unwrap_or_else(|| out_point(&previous.transaction, 1))
    }

    fn retained_wallet(&self, previous: Option<&Confirmed>) -> Result<Option<OutPoint>> {
        let Some(previous) = previous else {
            return Ok(None);
        };
        if previous.wallet_change.is_some() || self.config.update_funding.is_some() {
            return Ok(previous.wallet_change.clone());
        }
        let (raw, _, _) = self.node.committed(&previous.transaction)?;
        let tx = packed::Transaction::from_slice(raw.as_bytes())?;
        let change = tx
            .raw()
            .outputs()
            .get(1)
            .zip(tx.raw().outputs_data().get(1));
        Ok(change
            .filter(|(output, data)| self.is_fee_output(output, &data.raw_data()))
            .map(|_| out_point(&previous.transaction, 1)))
    }

    fn is_fee_output(&self, output: &CellOutput, data: &[u8]) -> bool {
        output.lock() == self.fee_lock && output.type_().is_none() && data.is_empty()
    }
}

impl Confirmed {
    fn point(&self) -> OutPoint {
        out_point(&self.transaction, self.output_index)
    }
}

#[derive(Deserialize)]
struct Cycles {
    cycles: Uint64,
}

fn out_point(transaction: &H256, index: u32) -> OutPoint {
    packed::OutPoint::new(transaction.into(), index).into()
}
