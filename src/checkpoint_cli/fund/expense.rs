//! Independent receipts for ordinary CKB expenses paid by one lock.
//! A confirmed spend commits to each funding transaction through its input OutPoint.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{HeaderView, JsonBytes, TransactionAndWitnessProof};
use ckb_types::{H256, core, packed, prelude::*};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    bridge_cli::{print_json, read_json_limited},
    l1::Rpc,
};
const MAX_INPUTS: usize = 64;
const MAX_PROOF_FILE: usize = 16 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    header: HeaderView,
    inclusion: TransactionAndWitnessProof,
    transaction: JsonBytes,
    parents: Vec<JsonBytes>,
}

#[derive(Serialize)]
struct Receipt {
    id: H256,
    transaction: H256,
    block: H256,
    payer_lock: H256,
    total_input_shannons: u64,
    total_output_shannons: u64,
    returned_to_payer_shannons: u64,
    miner_fee_shannons: u64,
}

impl Evidence {
    fn verify(
        &self,
        genesis: &H256,
        block: &H256,
        expected: &H256,
        payer: &H256,
    ) -> Result<Receipt> {
        let header: core::HeaderView = self.header.clone().into();
        let tx = packed::Transaction::from_slice(self.transaction.as_bytes())?.into_view();
        ensure!(
            tx.hash().as_slice() == expected.as_bytes(),
            "unexpected expense transaction"
        );
        stem::l1_proof::verify_transaction(&block.into(), &header, &tx, &self.inclusion)?;
        ensure!(
            (1..=MAX_INPUTS).contains(&tx.inputs().len())
                && self.parents.len() <= tx.inputs().len(),
            "expense input count is out of range"
        );
        let mut parents = BTreeMap::new();
        for data in &self.parents {
            let parent = packed::RawTransaction::from_slice(data.as_bytes())?;
            ensure!(
                parents.insert(parent.calc_tx_hash(), parent).is_none(),
                "duplicate funding transaction"
            );
        }
        let mut used = BTreeSet::new();
        let mut inputs = BTreeSet::new();
        let mut total_input = 0u64;
        for input in tx.inputs() {
            let point = input.previous_output();
            ensure!(
                !point.is_null() && inputs.insert(point.clone()),
                "invalid expense input"
            );
            let parent = parents
                .get(&point.tx_hash())
                .context("missing committed funding transaction")?;
            let output = parent
                .outputs()
                .get(u32::from(point.index()) as usize)
                .context("missing funding output")?;
            ensure!(
                output.type_().to_opt().is_none()
                    && output.lock().calc_script_hash().as_slice() == payer.as_bytes(),
                "funding input must be plain CKB under the pinned payer lock"
            );
            used.insert(point.tx_hash());
            total_input = total_input
                .checked_add(u64::from(output.capacity()))
                .context("input capacity overflow")?;
        }
        ensure!(
            used.len() == parents.len(),
            "unreferenced funding transaction"
        );
        let mut total_output = 0u64;
        let mut change = 0u64;
        for output in tx.outputs() {
            let amount = u64::from(output.capacity());
            total_output = total_output
                .checked_add(amount)
                .context("output capacity overflow")?;
            if output.lock().calc_script_hash().as_slice() == payer.as_bytes() {
                change = change
                    .checked_add(amount)
                    .context("returned capacity overflow")?;
            }
        }
        let fee = total_input
            .checked_sub(total_output)
            .context("expense has negative miner fee")?;
        let mut hasher = ckb_hash::new_blake2b();
        hasher.update(b"Stem L1 operating expense\0");
        hasher.update(genesis.as_bytes());
        hasher.update(expected.as_bytes());
        let mut id = [0u8; 32];
        hasher.finalize(&mut id);
        Ok(Receipt {
            id: id.into(),
            transaction: expected.clone(),
            block: block.clone(),
            payer_lock: payer.clone(),
            total_input_shannons: total_input,
            total_output_shannons: total_output,
            returned_to_payer_shannons: change,
            miner_fee_shannons: fee,
        })
    }
}

pub fn prove(url: &str, genesis: &H256, block: &H256, expected: &H256, payer: &H256) -> Result<()> {
    let rpc = Rpc::new(url)?;
    ensure!(rpc.block_hash(0)? == *genesis, "CKB genesis differs");
    let (transaction, _, included) = rpc.committed(expected)?;
    ensure!(&included == block, "expense is outside the pinned block");
    let tx = packed::Transaction::from_slice(transaction.as_bytes())?;
    ensure!(
        (1..=MAX_INPUTS).contains(&tx.raw().inputs().len()),
        "expense input count is out of range"
    );
    let hashes: BTreeSet<_> = tx
        .raw()
        .inputs()
        .into_iter()
        .map(|input| input.previous_output().tx_hash())
        .collect();
    let mut parents = Vec::with_capacity(hashes.len());
    let mut size = transaction.as_bytes().len();
    for hash in hashes {
        let (data, _, _) = rpc.committed(&hash.unpack())?;
        let raw = packed::Transaction::from_slice(data.as_bytes())?
            .raw()
            .as_bytes();
        size = size
            .checked_add(raw.len())
            .context("expense evidence overflow")?;
        ensure!(
            size < (MAX_PROOF_FILE - 65_536) / 2,
            "expense evidence exceeds portable limit"
        );
        parents.push(JsonBytes::from_bytes(raw));
    }
    let evidence = Evidence {
        header: rpc.call("get_header", json!([block]))?,
        inclusion: rpc.call(
            "get_transaction_and_witness_proof",
            json!([[expected], block]),
        )?,
        transaction,
        parents,
    };
    let receipt = evidence.verify(genesis, block, expected, payer)?;
    print_json(&json!({"receipt": receipt,"evidence":evidence}))
}

pub fn verify(path: &Path, genesis: &H256, block: &H256, tx: &H256, payer: &H256) -> Result<()> {
    let evidence: Evidence = read_json_limited(path, MAX_PROOF_FILE)?;
    print_json(&serde_json::to_value(
        evidence.verify(genesis, block, tx, payer)?,
    )?)
}
