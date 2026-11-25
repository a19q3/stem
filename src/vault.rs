use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use ckb_hash::blake2b_256;
use ckb_jsonrpc_types::{
    CellDep as JsonCellDep, CellWithStatus, JsonBytes, OutPoint as JsonOutPoint,
    Transaction as JsonTx,
};
use fern_bridge_wire as wire;
use ckb_types::{
    bytes::Bytes,
    core::{Capacity, ScriptHashType, TransactionBuilder, TransactionView},
    packed::{CellInput, CellOutput, OutPoint, Script, Transaction},
    prelude::*,
};
use clap::Args;
use serde::Deserialize;
use serde_json::json;

use crate::{
    bridge::Bridge,
    bridge_cli::{print_json, read_json_limited},
    genesis::MAX_TX_BYTES,
    l1::Rpc,
    read_genesis,
};

pub struct LiveVault {
    pub output: CellOutput,
    pub data: Bytes,
    pub asset: Option<Script>,
}

impl LiveVault {
    pub fn load(l1: &Rpc, bridge: &Bridge, point: &OutPoint) -> Result<Self> {
        ensure!(
            l1.block_hash(0)? == bridge.l1_genesis,
            "incorrect L1 genesis"
        );
        let (output, data) = committed_cell(l1, point).context("could not read live L1 vault")?;
        ensure!(
            is_vault(bridge, &output),
            "L1 input is not the configured vault"
        );
        let asset = match (data.first(), data.len()) {
            (Some(1), wire::NATIVE_VAULT_SIZE) if bridge.xudt.is_none() => None,
            (Some(2), wire::XUDT_VAULT_SIZE) => {
                let fields = wire::vault_asset(&data).context("invalid xUDT vault data")?;
                let configured = bridge.xudt.as_ref().context("xUDT bridge is disabled")?;
                let script: Script = configured.l1_type.clone().into();
                ensure!(
                    fields.gate_code == *configured.gate_code_hash.as_bytes()
                        && fields.type_hash == script.calc_script_hash().as_slice(),
                    "L1 vault has a different asset profile"
                );
                Some(script)
            }
            _ => anyhow::bail!("unexpected L1 vault format"),
        };
        let lock = output.lock();
        ensure!(
            data[1] <= 2
                && &data[10..42] == bridge.deposit_lock.code_hash.as_bytes()
                && lock.code_hash().as_slice() == &data[42..74]
                && u8::from(lock.hash_type()) == ScriptHashType::Type as u8
                && lock.args().raw_data().len() == 20,
            "L1 vault has a different deposit or custody profile"
        );
        Ok(Self {
            output,
            data,
            asset,
        })
    }

    pub fn nonce(&self) -> u64 {
        u64::from_le_bytes(self.data[2..10].try_into().expect("validated vault length"))
    }

    fn rotation_data(&self) -> Bytes {
        let mut data = self.data.to_vec();
        data[1] = 2;
        data[74..wire::NATIVE_VAULT_SIZE].fill(0);
        if self.asset.is_some() {
            data[wire::PAYOUT_UNITS_OFFSET..].fill(0);
        }
        data.into()
    }
}

pub fn read_out_point(path: &Path) -> Result<OutPoint> {
    Ok(read_json_limited::<JsonOutPoint>(path, 4096)?.into())
}

pub fn committed_cell(l1: &Rpc, point: &OutPoint) -> Result<(CellOutput, Bytes)> {
    let response: CellWithStatus = l1.call(
        "get_live_cell",
        json!([JsonOutPoint::from(point.clone()), true]),
    )?;
    ensure!(
        response.status == "live" && response.block_hash.is_some(),
        "L1 input must be committed and live"
    );
    let cell = response.cell.context("missing live L1 Cell")?;
    let data = cell.data.context("missing live L1 Cell data")?;
    Ok((
        cell.output.into(),
        Bytes::copy_from_slice(data.content.as_bytes()),
    ))
}

pub fn is_vault(bridge: &Bridge, output: &CellOutput) -> bool {
    output.type_().to_opt().is_some_and(|script| {
        script.calc_script_hash().as_slice() == bridge.vault_type_hash.as_bytes()
    })
}

pub fn fee_change(l1: &Rpc, point: &OutPoint, fee: u64) -> Result<CellOutput> {
    ensure!(fee > 0, "L1 fee must be positive");
    let (cell, data) = committed_cell(l1, point)?;
    ensure!(
        cell.type_().is_none() && data.is_empty(),
        "fee input must be a plain Cell"
    );
    let change = u64::from(cell.capacity())
        .checked_sub(fee)
        .context("fee exceeds the fee input capacity")?;
    Ok(cell.as_builder().capacity(change).build())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Multisig {
    require_first_n: u8,
    threshold: u8,
    pubkey_hashes: Vec<JsonBytes>,
}

impl Multisig {
    fn lock(self, current: &Script) -> Result<Script> {
        let count = u8::try_from(self.pubkey_hashes.len()).context("too many multisig members")?;
        ensure!(
            self.require_first_n <= self.threshold && self.threshold > 0 && self.threshold <= count,
            "invalid multisig threshold or first-signer count"
        );
        let mut distinct = HashSet::new();
        let mut prefix = vec![0, self.require_first_n, self.threshold, count];
        for member in &self.pubkey_hashes {
            let hash = member.as_bytes();
            ensure!(
                hash.len() == 20 && hash != [0; 20] && distinct.insert(hash),
                "invalid or duplicate multisig pubkey hash"
            );
            prefix.extend_from_slice(hash);
        }
        let digest = blake2b_256(prefix);
        Ok(current
            .clone()
            .as_builder()
            .args(digest[..20].pack())
            .build())
    }
}

#[derive(Args)]
pub struct RotationRequest {
    /// Trusted sidechain application genesis.
    #[arg(long)]
    genesis: PathBuf,
    /// RPC of the custodian's independently synchronized L1 node.
    #[arg(long)]
    l1_rpc: String,
    /// JSON OutPoint of the current vault.
    #[arg(long)]
    vault: PathBuf,
    /// JSON configuration of the current multisig.
    #[arg(long)]
    current_multisig: PathBuf,
    /// JSON configuration approved for the new multisig.
    #[arg(long)]
    new_multisig: PathBuf,
    /// JSON OutPoint of a separate plain Cell that pays the fee.
    #[arg(long)]
    fee_input: PathBuf,
    /// Exact fee in shannons.
    #[arg(long)]
    fee: u64,
    /// JSON array of the vault, custody lock and fee lock CellDeps.
    #[arg(long)]
    cell_deps: PathBuf,
}

#[derive(Args)]
pub struct CheckRotation {
    #[command(flatten)]
    request: RotationRequest,
    /// Exact unsigned L1 transaction to check.
    #[arg(long)]
    transaction: PathBuf,
}

impl RotationRequest {
    fn prepare(&self) -> Result<(TransactionView, u64)> {
        let spec = read_genesis(&self.genesis)?;
        spec.validate()?;
        let bridge = spec.bridge.as_ref().context("bridge is disabled")?;
        let l1 = Rpc::new(&self.l1_rpc)?;
        let point = read_out_point(&self.vault)?;
        let vault = LiveVault::load(&l1, bridge, &point)?;
        let current = vault.output.lock();
        ensure!(
            read_json_limited::<Multisig>(&self.current_multisig, MAX_TX_BYTES)?.lock(&current)?
                == current,
            "current multisig configuration does not match the live vault"
        );
        let next =
            read_json_limited::<Multisig>(&self.new_multisig, MAX_TX_BYTES)?.lock(&current)?;
        ensure!(next != current, "new multisig must change the vault lock");
        let fee_point = read_out_point(&self.fee_input)?;
        ensure!(fee_point != point, "fee input is the vault");
        let change = fee_change(&l1, &fee_point, self.fee)?;
        let deps = read_json_limited::<Vec<JsonCellDep>>(&self.cell_deps, MAX_TX_BYTES)?;
        ensure!(!deps.is_empty(), "rotation requires L1 CellDeps");
        let tx = TransactionBuilder::default()
            .cell_deps(deps.into_iter().map(Into::into))
            .input(CellInput::new(point, 0))
            .input(CellInput::new(fee_point, 0))
            .output(vault.output.clone().as_builder().lock(next).build())
            .output_data(vault.rotation_data())
            .output(change)
            .output_data(Bytes::new())
            .build();
        for (output, data) in tx.outputs_with_data_iter() {
            ensure!(
                !output.is_lack_of_capacity(Capacity::bytes(data.len())?)?,
                "rotation output has insufficient occupied capacity"
            );
        }
        Ok((tx, vault.nonce()))
    }

    pub fn run(self) -> Result<()> {
        let (tx, _) = self.prepare()?;
        print_json(&JsonTx::from(tx.data()))
    }
}

impl CheckRotation {
    pub fn run(self) -> Result<()> {
        let (expected, nonce) = self.request.prepare()?;
        let tx = Transaction::from(read_json_limited::<JsonTx>(
            &self.transaction,
            4 * MAX_TX_BYTES,
        )?);
        ensure!(
            tx.as_slice() == expected.data().as_slice(),
            "rotation differs from the reviewed custody migration"
        );
        print_json(&json!({
            "l1_transaction_hash": format!("{:#x}", expected.hash()),
            "next_withdrawal_nonce": nonce,
            "new_vault": JsonOutPoint::from(OutPoint::new(expected.hash(), 0)),
        }))
    }
}
