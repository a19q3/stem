use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ckb_types::{packed::Byte32, prelude::*};
use clap::Args;
use serde::Deserialize;
use serde_json::json;
use stem_ledger::{
    history::HistoricalHeaderProof,
    light::{FinalityVerifier, VerifiedHeader},
};
use stem_stake::jolteon::CommitteeChange;

use super::{decode, encode, read_hex};
use crate::bridge_cli::{print_json, read_json_limited, read_limited};

const MAX_TRUST_BYTES: usize = 16 * 1024;
const MAX_HISTORY_BYTES: usize = 8 * 1024;
const MAX_HANDOFFS: usize = 64;
// A native query result must fit within Jolteon's 24 MiB response frame after
// the transport hex-encodes it.
const MAX_EXECUTION_JSON: usize = 12 * 1024 * 1024;

#[derive(Args)]
struct Checkpoint {
    /// Trusted network, genesis and initial roster exported by jolteon-trust.
    #[arg(long)]
    trust: PathBuf,
    /// JSON hex execution finality certificate for the containing checkpoint.
    #[arg(long)]
    finality: PathBuf,
    /// Preceding committee handoffs in chronological order, as JSON hex.
    #[arg(long = "handoff")]
    handoffs: Vec<PathBuf>,
    /// The finality certificate itself closes its committee's epoch.
    #[arg(long)]
    terminal: bool,
}

#[derive(Args)]
pub struct VerifyHistory {
    #[command(flatten)]
    checkpoint: Checkpoint,
    /// Historical Header inclusion proof produced with --strand-output.
    #[arg(long)]
    history: PathBuf,
    /// Requested historical Header height.
    #[arg(long)]
    height: u64,
}

#[derive(Args)]
pub struct VerifyExecution {
    #[command(flatten)]
    checkpoint: Checkpoint,
    /// Decoded JSON value from /stem/execution with prove=true.
    #[arg(long)]
    execution: PathBuf,
    /// A Strand proof for a Header older than the certified checkpoint.
    #[arg(long)]
    history: Option<PathBuf>,
    #[arg(long)]
    height: u64,
    #[arg(long)]
    index: u32,
}

#[derive(Deserialize)]
struct ExecutionQuery {
    transaction: String,
    receipt: String,
    proof: String,
}

#[derive(Deserialize)]
struct GenesisTrust {
    network_id: String,
    genesis_id: String,
    initial_committee: String,
}

impl Checkpoint {
    fn authenticate(&self) -> Result<(Byte32, VerifiedHeader)> {
        ensure!(
            self.handoffs.len() <= MAX_HANDOFFS,
            "at most {MAX_HANDOFFS} committee handoffs may be verified"
        );
        let trust: GenesisTrust = read_json_limited(&self.trust, MAX_TRUST_BYTES)?;
        let network: Byte32 = decode::<32>(&trust.network_id)?.pack();
        let genesis: Byte32 = decode::<32>(&trust.genesis_id)?.pack();
        let roster = hex::decode(
            trust
                .initial_committee
                .strip_prefix("0x")
                .unwrap_or(&trust.initial_committee),
        )?;
        let initial = CommitteeChange::decode(&roster)?;
        ensure!(
            initial.epoch == 0 && initial.activation_height == 1,
            "trusted roster must be the initial committee"
        );
        let mut verifier =
            FinalityVerifier::new(network.clone(), genesis.clone(), &initial.members)?;
        for path in &self.handoffs {
            verifier
                .accept_handoff(&read_hex(path)?)
                .with_context(|| format!("verify committee handoff {}", path.display()))?;
        }
        let checkpoint = verifier
            .verify(&read_hex(&self.finality)?, self.terminal)
            .context("verify checkpoint finality against trusted genesis")?;

        Ok((genesis, checkpoint))
    }
}

fn read_history(path: &std::path::Path) -> Result<HistoricalHeaderProof> {
    HistoricalHeaderProof::decode(&read_limited(path, MAX_HISTORY_BYTES)?)
}

impl VerifyHistory {
    pub(super) fn run(self) -> Result<()> {
        let (genesis, checkpoint) = self.checkpoint.authenticate()?;
        let header = checkpoint.historical_header(self.height, &read_history(&self.history)?)?;
        let network = header.network();
        print_json(&json!({
            "network_id": encode(network.as_slice()),
            "genesis_id": encode(genesis.as_slice()),
            "height": header.header().context.height,
            "id": encode(header.header().id(network).as_slice()),
            "header": encode(header.header().encode()),
            "checkpoint_height": checkpoint.header().context.height,
            "checkpoint_id": encode(checkpoint.header().id(network).as_slice()),
        }))
    }
}

impl VerifyExecution {
    pub(super) fn run(self) -> Result<()> {
        let (genesis, checkpoint) = self.checkpoint.authenticate()?;
        let header = match &self.history {
            Some(path) => checkpoint.historical_header(self.height, &read_history(path)?)?,
            None => {
                ensure!(
                    checkpoint.header().context.height == self.height,
                    "execution height requires a matching checkpoint or historical Header proof"
                );
                checkpoint.clone()
            }
        };
        let input: ExecutionQuery = read_json_limited(&self.execution, MAX_EXECUTION_JSON)?;
        let from_hex = |value: &str| hex::decode(value.strip_prefix("0x").unwrap_or(value));
        let execution = header.verify_execution(
            &from_hex(&input.transaction)?,
            &from_hex(&input.receipt)?,
            &from_hex(&input.proof)?,
        )?;
        ensure!(
            execution.position == self.index,
            "execution proof position differs from the request"
        );
        let network = header.network();
        print_json(&json!({
            "network_id": encode(network.as_slice()),
            "genesis_id": encode(genesis.as_slice()),
            "checkpoint_height": checkpoint.header().context.height,
            "checkpoint_id": encode(checkpoint.header().id(network).as_slice()),
            "height": header.header().context.height,
            "header_id": encode(header.header().id(network).as_slice()),
            "position": execution.position,
            "transaction_id": encode(execution.id.as_slice()),
            "fee": ckb_jsonrpc_types::Uint64::from(execution.receipt.fee),
            "cycles": ckb_jsonrpc_types::Uint64::from(execution.receipt.cycles),
            "protocol_result": encode(execution.receipt.protocol_result),
        }))
    }
}
