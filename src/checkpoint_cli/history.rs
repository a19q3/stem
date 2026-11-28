use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ckb_types::{packed::Byte32, prelude::*};
use clap::Args;
use serde::Deserialize;
use serde_json::json;
use stem_ledger::{history::HistoricalHeaderProof, light::FinalityVerifier};
use stem_stake::jolteon::CommitteeChange;

use super::{decode, encode, read_hex};
use crate::bridge_cli::{print_json, read_json_limited, read_limited};

const MAX_TRUST_BYTES: usize = 16 * 1024;
const MAX_HISTORY_BYTES: usize = 8 * 1024;
const MAX_HANDOFFS: usize = 64;

#[derive(Args)]
pub struct VerifyHistory {
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
    /// Historical Header inclusion proof produced with --strand-output.
    #[arg(long)]
    history: PathBuf,
    /// Requested historical Header height.
    #[arg(long)]
    height: u64,
}

#[derive(Deserialize)]
struct GenesisTrust {
    network_id: String,
    genesis_id: String,
    initial_committee: String,
}

impl VerifyHistory {
    pub(super) fn run(self) -> Result<()> {
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
        for path in self.handoffs {
            verifier
                .accept_handoff(&read_hex(&path)?)
                .with_context(|| format!("verify committee handoff {}", path.display()))?;
        }
        let checkpoint = verifier
            .verify(&read_hex(&self.finality)?, self.terminal)
            .context("verify checkpoint finality against trusted genesis")?;

        let proof =
            HistoricalHeaderProof::decode(&read_limited(&self.history, MAX_HISTORY_BYTES)?)?;
        checkpoint.verify_history(self.height, &proof)?;
        print_json(&json!({
            "network_id": encode(network.as_slice()),
            "genesis_id": encode(genesis.as_slice()),
            "height": proof.header.context.height,
            "id": encode(proof.header.id(&network).as_slice()),
            "header": encode(proof.header.encode()),
            "checkpoint_height": checkpoint.header().context.height,
            "checkpoint_id": encode(checkpoint.header().id(&network).as_slice()),
        }))
    }
}
