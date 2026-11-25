use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{
    CellDep as JsonCellDep, JsonBytes, OutPoint as JsonOutPoint, Transaction as JsonTx,
};
use fern_bridge_wire as wire;
use ckb_types::{
    H256,
    bytes::Bytes,
    core::{Capacity, TransactionBuilder, TransactionView},
    packed::{Byte32, CellInput, CellOutput, OutPoint, Transaction},
    prelude::*,
};
use clap::Args;
use serde::Deserialize;
use serde_json::json;

use crate::{
    bridge::{self, Bridge},
    bridge_cli::{print_json, read_json_limited},
    comet_rpc,
    genesis::{Genesis, MAX_GENESIS_BYTES, MAX_TX_BYTES},
    l1::{Journal, Rpc},
    vault::{LiveVault, committed_cell, fee_change, is_vault, read_out_point},
};

#[derive(Args)]
pub struct CheckPayout {
    #[command(flatten)]
    request: PayoutRequest,
    /// The exact unsigned L1 transaction to check before signing.
    #[arg(long)]
    transaction: PathBuf,
}

#[derive(Args)]
pub struct PreparePayout {
    #[command(flatten)]
    request: PayoutRequest,
    /// JSON OutPoint of a plain live L1 Cell providing the transaction fee.
    #[arg(long)]
    fee_input: PathBuf,
    /// Fee to subtract from that Cell, in shannons.
    #[arg(long)]
    fee: u64,
    /// JSON array of L1 CellDeps for the vault, custody lock and fee lock.
    #[arg(long)]
    cell_deps: PathBuf,
    /// JSON OutPoint of the live escrow Cell for a typed withdrawal.
    #[arg(long)]
    escrow: Option<PathBuf>,
}

#[derive(Args)]
struct PayoutRequest {
    /// Trusted CometBFT genesis containing the full application state.
    #[arg(long)]
    comet_genesis: PathBuf,
    /// Trusted, locally installed fern-proof executable.
    #[arg(long)]
    proof_verifier: PathBuf,
    #[arg(long)]
    proof: PathBuf,
    /// CometBFT RPC of the custodian's replica for legacy receipts.
    #[arg(long)]
    sidechain_rpc: Option<String>,
    /// CKB RPC of the custodian's own L1 node.
    #[arg(long)]
    l1_rpc: String,
    /// Guardian anchor journals checked before authorizing release.
    #[arg(long, required = true)]
    journal: Vec<PathBuf>,
    /// JSON OutPoint for the canonical live vault input.
    #[arg(long)]
    vault: PathBuf,
}

#[derive(Deserialize)]
struct TrustedGenesis {
    chain_id: String,
    app_state: Genesis,
}

#[derive(Deserialize)]
struct Proof {
    chain_id: String,
    height: u64,
    ckb_tx_hash: H256,
    app_hash: H256,
    transaction: JsonBytes,
    withdrawal_start: Option<u64>,
}

#[derive(Deserialize)]
struct Withdrawal {
    nonce: u64,
    withdrawal_id: H256,
    out_point: JsonOutPoint,
    block_hash: H256,
}

struct AuthorizedPayout {
    bridge: Bridge,
    proof: Proof,
    vault_point: OutPoint,
    nonce: u64,
    withdrawal_id: Byte32,
    recipient: (CellOutput, Bytes),
    vault: (CellOutput, Bytes),
    units: Option<u128>,
}

impl CheckPayout {
    pub fn run(self) -> Result<()> {
        let l1 = Rpc::new(&self.request.l1_rpc)?;
        let payout = self.request.authorize(&l1)?;
        let candidate = Transaction::from(read_json_limited::<JsonTx>(
            &self.transaction,
            4 * MAX_TX_BYTES,
        )?)
        .into_view();
        payout.check(&candidate)?;
        print_json(&payout.receipt(&candidate))
    }
}

impl PreparePayout {
    pub fn run(self) -> Result<()> {
        let l1 = Rpc::new(&self.request.l1_rpc)?;
        let payout = self.request.authorize(&l1)?;
        let deps = read_json_limited::<Vec<JsonCellDep>>(&self.cell_deps, MAX_TX_BYTES)?;
        ensure!(!deps.is_empty(), "payout requires L1 CellDeps");
        let fee_point = read_out_point(&self.fee_input)?;
        ensure!(fee_point != payout.vault_point, "fee input is the vault");
        let change_output = fee_change(&l1, &fee_point, self.fee)?;
        let mut builder = TransactionBuilder::default()
            .cell_deps(deps.into_iter().map(Into::into))
            .input(CellInput::new(payout.vault_point.clone(), 0))
            .output(payout.recipient.0.clone())
            .output_data(payout.recipient.1.clone())
            .output(payout.vault.0.clone())
            .output_data(payout.vault.1.clone());

        match (payout.units, self.escrow) {
            (Some(units), Some(path)) => {
                let point = read_out_point(&path)?;
                ensure!(
                    point != payout.vault_point && point != fee_point,
                    "escrow input must be distinct"
                );
                let (output, data) = committed_cell(&l1, &point)?;
                ensure!(
                    output.lock() == payout.bridge.xudt_escrow_lock()?
                        && output.type_() == payout.recipient.0.type_(),
                    "L1 input is not the configured xUDT escrow"
                );
                let remaining = wire::amount(&data)
                    .context("invalid L1 escrow balance")?
                    .checked_sub(units)
                    .context("escrow cannot cover the withdrawal")?;
                builder = builder
                    .input(CellInput::new(point, 0))
                    .output(output)
                    .output_data(Bytes::copy_from_slice(&remaining.to_le_bytes()));
            }
            (Some(_), None) => anyhow::bail!("typed payout requires --escrow"),
            (None, Some(_)) => anyhow::bail!("native payout leaves the escrow untouched"),
            (None, None) => {}
        }
        let tx = builder
            .input(CellInput::new(fee_point, 0))
            .output(change_output)
            .output_data(Bytes::new())
            .build();
        for (output, data) in tx.outputs_with_data_iter() {
            ensure!(
                !output.is_lack_of_capacity(Capacity::bytes(data.len())?)?,
                "payout output has insufficient occupied capacity"
            );
        }
        payout.check(&tx)?;
        print_json(&JsonTx::from(tx.data()))
    }
}

impl PayoutRequest {
    fn authorize(&self, l1: &Rpc) -> Result<AuthorizedPayout> {
        let genesis: TrustedGenesis =
            read_json_limited(&self.comet_genesis, MAX_GENESIS_BYTES * 2)?;
        let spec = &genesis.app_state;
        let anchor = spec.validate()?.0.hash();
        let bridge = spec.bridge.as_ref().context("bridge is disabled")?;
        let proof = self.verify_proof()?;
        ensure!(
            proof.chain_id == genesis.chain_id && proof.chain_id == spec.chain_id,
            "proof authenticates a different sidechain"
        );
        let burn = Transaction::from_slice(proof.transaction.as_bytes())?.into_view();
        ensure!(
            burn.hash().as_slice() == proof.ckb_tx_hash.as_bytes(),
            "proof and packed CKB transaction hashes differ"
        );

        let point = read_out_point(&self.vault)?;
        let live = LiveVault::load(l1, bridge, &point)?;
        let nonce = live.nonce();
        let withdrawal_id = bridge::withdrawal_id(&bridge.vault_type_hash, nonce);
        let burn_index = if spec.authenticate_withdrawals {
            let start = proof
                .withdrawal_start
                .context("proof does not contain an authenticated withdrawal index")?;
            let offset = nonce
                .checked_sub(start)
                .context("proved withdrawal is ahead of the live vault slot")?;
            bridge::withdrawal_outputs(&burn)
                .nth(usize::try_from(offset)?)
                .context("proof does not certify the live vault slot")?
        } else {
            let record = self.withdrawal(nonce)?;
            let burn_point: OutPoint = record.out_point.into();
            ensure!(
                record.nonce == nonce
                    && record.withdrawal_id.as_bytes() == withdrawal_id.as_slice()
                    && record.block_hash == proof.app_hash
                    && burn_point.tx_hash() == burn.hash(),
                "proof does not certify the burn assigned to the live vault slot"
            );
            u32::from(burn_point.index()) as usize
        };
        let burned = burn.output(burn_index).context("missing burn output")?;
        let burned_data = burn
            .outputs_data()
            .get(burn_index)
            .context("missing burn data")?;
        let payout = bridge.payout(&burned, &burned_data.raw_data(), &anchor)?;
        let payout_type = if payout.units.is_some() {
            Some(live.asset.context("typed burn requires an xUDT vault")?)
        } else {
            None
        };
        let recipient = CellOutput::new_builder()
            .capacity(burned.capacity())
            .lock(payout.recipient)
            .type_(payout_type)
            .build();
        let recipient_data = payout
            .units
            .map(|units| Bytes::copy_from_slice(&units.to_le_bytes()))
            .unwrap_or_default();
        let amount = u64::from(burned.capacity());
        let remaining = u64::from(live.output.capacity())
            .checked_sub(amount)
            .context("L1 vault cannot cover the withdrawal")?;
        let vault = live.output.as_builder().capacity(remaining).build();
        let mut expected = live.data.to_vec();
        expected[1] = 1; // The L1 vault's release action.
        expected[2..10].copy_from_slice(
            &nonce
                .checked_add(1)
                .context("vault nonce exhausted")?
                .to_le_bytes(),
        );
        expected[74..82].copy_from_slice(&amount.to_le_bytes());
        expected[82..114].copy_from_slice(recipient.lock().calc_script_hash().as_slice());
        expected[114..wire::NATIVE_VAULT_SIZE].copy_from_slice(withdrawal_id.as_slice());
        if expected.len() == wire::XUDT_VAULT_SIZE {
            expected[wire::PAYOUT_UNITS_OFFSET..]
                .copy_from_slice(&payout.units.unwrap_or(0).to_le_bytes());
        }
        for journal in &self.journal {
            Journal::open(journal, anchor.clone())?
                .validate(l1)?
                .context("no attested L1 anchor in guardian journal")?;
        }
        Ok(AuthorizedPayout {
            bridge: bridge.clone(),
            proof,
            vault_point: point,
            nonce,
            withdrawal_id,
            recipient: (recipient, recipient_data),
            vault: (vault, Bytes::from(expected)),
            units: payout.units,
        })
    }

    fn verify_proof(&self) -> Result<Proof> {
        let output = Command::new(&self.proof_verifier)
            .arg("-json")
            .arg("-genesis")
            .arg(&self.comet_genesis)
            .arg("-proof")
            .arg(&self.proof)
            .output()
            .context("could not start the execution-proof verifier")?;
        ensure!(
            output.status.success(),
            "execution proof rejected: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        let proof: Proof = serde_json::from_slice(&output.stdout)?;
        ensure!(
            proof.transaction.as_bytes().len() <= MAX_TX_BYTES,
            "proved transaction exceeds size limit"
        );
        Ok(proof)
    }

    fn withdrawal(&self, nonce: u64) -> Result<Withdrawal> {
        let sidechain = self
            .sidechain_rpc
            .as_deref()
            .context("legacy receipts require --sidechain-rpc")?;
        let record = comet_rpc::Rpc::new(sidechain)?
            .query("/withdrawal", &format!("0x{nonce:016x}"), None)?
            .context("sidechain did not return a withdrawal")?;
        Ok(serde_json::from_slice(&record)?)
    }
}

impl AuthorizedPayout {
    fn check(&self, tx: &TransactionView) -> Result<()> {
        ensure!(
            tx.input_pts_iter()
                .filter(|input| input == &self.vault_point)
                .count()
                == 1,
            "transaction must spend the specified vault exactly once"
        );
        let output_data = tx.outputs_data().get(0).context("missing payout data")?;
        ensure!(
            tx.output(0).as_ref() == Some(&self.recipient.0)
                && output_data.raw_data() == self.recipient.1,
            "L1 recipient, capacity, asset or amount differs from the certified burn"
        );
        let mut vaults = tx
            .outputs_with_data_iter()
            .filter(|(output, _)| is_vault(&self.bridge, output));
        let (output, data) = vaults
            .next()
            .context("transaction must recreate the vault")?;
        ensure!(
            vaults.next().is_none() && output.lock() == self.vault.0.lock(),
            "release must preserve one vault under the current custody lock"
        );
        ensure!(
            output.capacity() == self.vault.0.capacity(),
            "L1 vault capacity differs from the authorized release"
        );
        ensure!(
            data == self.vault.1,
            "L1 vault output does not authorize the certified burn"
        );
        Ok(())
    }

    fn receipt(&self, tx: &TransactionView) -> serde_json::Value {
        json!({
            "l1_transaction_hash": format!("{:#x}", tx.hash()),
            "sidechain_transaction_hash": self.proof.ckb_tx_hash,
            "sidechain_height": self.proof.height,
            "nonce": self.nonce,
            "withdrawal_id": format!("{:#x}", self.withdrawal_id),
        })
    }
}
