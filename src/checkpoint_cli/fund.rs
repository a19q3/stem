use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{
    JsonBytes, OutPoint, Script as JsonScript, Transaction as JsonTransaction,
};
use ckb_types::{
    H256,
    bytes::Bytes,
    core::{Capacity, ScriptHashType, TransactionBuilder, TransactionView},
    packed::{Byte32, CellDep, CellInput, CellOutput, Script, WitnessArgs},
    prelude::*,
};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::json;
use stem_stake::jolteon::checkpoint::{self, FundingPolicy, Tip};

use crate::{
    bridge_cli::{print_json, read_json_limited},
    l1::Rpc,
};

#[derive(Subcommand)]
pub enum Command {
    /// Derive the CKB Lock that receives the PoW Security Fund payout.
    Lock {
        #[arg(long)]
        request: PathBuf,
    },
    /// Build a two-input CKB update that pays its fee directly from a fund Cell.
    Update {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        l1_rpc: String,
        /// Submit to the configured CKB node.
        #[arg(long)]
        submit: bool,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LockRequest {
    code_hash: H256,
    checkpoint_code_hash: H256,
    checkpoint_type_id: H256,
    network: H256,
    ckb_genesis: H256,
    fee_cap: u64,
    interval_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Update {
    pub checkpoint: OutPoint,
    pub checkpoint_code: OutPoint,
    pub fund: OutPoint,
    pub fund_code: OutPoint,
    pub fee: u64,
    pub witness: JsonBytes,
}

pub(super) struct Funded {
    pub transaction: TransactionView,
    pub tip: Tip,
    change: u64,
}

impl Command {
    pub(super) fn run(self) -> Result<()> {
        match self {
            Self::Lock { request } => lock(&request),
            Self::Update {
                request,
                l1_rpc,
                submit,
            } => Update::run(&request, &l1_rpc, submit),
        }
    }
}

fn lock(path: &Path) -> Result<()> {
    let request: LockRequest = read_json_limited(path, 4096)?;
    let policy = FundingPolicy {
        checkpoint_code: request.checkpoint_code_hash.as_bytes().try_into()?,
        checkpoint_type_id: request.checkpoint_type_id.as_bytes().try_into()?,
        network: request.network.as_bytes().try_into()?,
        ckb_genesis: request.ckb_genesis.as_bytes().try_into()?,
        fee_cap: request.fee_cap,
        interval_ms: request.interval_ms,
    };
    ensure!(
        request.code_hash != H256::default(),
        "invalid fee fund code hash"
    );
    let args = policy.encode();
    FundingPolicy::decode(&args)?;
    let lock = Script::new_builder()
        .code_hash(Byte32::from(&request.code_hash))
        .hash_type(ScriptHashType::Data2)
        .args(args.as_slice().pack())
        .build();
    print_json(&JsonScript::from(lock))
}

impl Update {
    fn run(path: &Path, l1_rpc: &str, submit: bool) -> Result<()> {
        let update: Self = read_json_limited(path, 160 * 1024)?;
        let rpc = Rpc::new(l1_rpc)?;
        let funded = update.build(&rpc)?;
        let hash = H256::from(funded.transaction.hash());
        let raw = funded.transaction.data();
        let json = JsonTransaction::from(raw.clone());
        if submit {
            let submitted: H256 = rpc.call("send_transaction", json!([json, "passthrough"]))?;
            ensure!(
                submitted == hash,
                "CKB returned a different transaction hash"
            );
        }
        print_json(&json!({
            "hash": hash, "json": json, "molecule": JsonBytes::from_bytes(raw.as_bytes()),
            "fee_shannons": update.fee, "fund_change_shannons": funded.change,
            "checkpoint_height": funded.tip.height(), "submitted": submit,
        }))
    }

    pub fn build(&self, rpc: &Rpc) -> Result<Funded> {
        let checkpoint = rpc.live_cell(&self.checkpoint)?;
        let fund = rpc.live_cell(&self.fund)?;
        let fund_output: CellOutput = fund.output.into();
        let fund_lock = fund_output.lock();
        ensure!(
            fund_output.type_().to_opt().is_none()
                && fund.data.is_some_and(|data| data.content.is_empty()),
            "checkpoint fund must be a plain Capacity Cell"
        );
        let policy = FundingPolicy::decode(&fund_lock.args().raw_data())?;
        ensure!(
            rpc.block_hash(0)?.as_bytes() == &policy.ckb_genesis,
            "fund is pinned to a different CKB genesis"
        );
        let checkpoint_output: CellOutput = checkpoint.output.into();
        let checkpoint_type = checkpoint_output
            .type_()
            .to_opt()
            .context("missing checkpoint Type Script")?;
        let checkpoint_policy = policy.verify_checkpoint_type(
            &checkpoint_type.code_hash().as_slice().try_into()?,
            &checkpoint_type.args().raw_data(),
        )?;
        verify_code(rpc, &self.checkpoint_code, &checkpoint_type)?;
        verify_code(rpc, &self.fund_code, &fund_lock)?;
        let previous = Tip::decode(
            checkpoint
                .data
                .context("missing checkpoint data")?
                .content
                .as_bytes(),
        )?;
        let next =
            checkpoint::verify(&checkpoint_policy, Some(&previous), self.witness.as_bytes())?;
        policy
            .verify_charge(&previous, &next, self.fee)
            .context("checkpoint does not satisfy the fund expense policy")?;
        let balance: u64 = fund_output.capacity().unpack();
        let change = balance
            .checked_sub(self.fee)
            .context("insufficient fund Capacity")?;
        let next_fund = fund_output.as_builder().capacity(change).build();
        ensure!(
            !next_fund.is_lack_of_capacity(Capacity::zero())?,
            "fund change is below L1 occupied Capacity"
        );
        let witness = WitnessArgs::new_builder()
            .input_type(Some(Bytes::copy_from_slice(self.witness.as_bytes())).pack())
            .build();
        let tx = TransactionBuilder::default()
            .input(CellInput::new(self.checkpoint.clone().into(), 0))
            .input(CellInput::new(self.fund.clone().into(), 0))
            .output(checkpoint_output)
            .output(next_fund)
            .output_data(next.encode().pack())
            .output_data(ckb_types::packed::Bytes::default())
            .cell_dep(
                CellDep::new_builder()
                    .out_point(self.checkpoint_code.clone())
                    .build(),
            )
            .cell_dep(
                CellDep::new_builder()
                    .out_point(self.fund_code.clone())
                    .build(),
            )
            .witness(witness.as_bytes().pack())
            .build();
        Ok(Funded {
            transaction: tx,
            tip: next,
            change,
        })
    }
}

fn verify_code(rpc: &Rpc, point: &OutPoint, script: &Script) -> Result<()> {
    let data = rpc.live_cell(point)?.data.context("missing L1 code data")?;
    ensure!(
        u8::from(script.hash_type()) == ScriptHashType::Data2 as u8
            && ckb_hash::blake2b_256(data.content.as_bytes()).as_slice()
                == script.code_hash().as_slice(),
        "code dependency does not match the pinned Data2 Script"
    );
    Ok(())
}
