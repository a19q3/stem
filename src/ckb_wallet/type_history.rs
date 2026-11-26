//! Portable witnessed history of a unique CKB Type, pinned by its birth, end and anchor.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{JsonBytes, OutPoint as JsonPoint, TransactionAndWitnessProof};
use ckb_types::{
    H256,
    bytes::Bytes,
    core::{HeaderView, TransactionView},
    packed::{self, CellOutput, OutPoint, RawTransaction},
    prelude::*,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::metatype_history::{
    History, MAX_HEADERS, MAX_RAW_BYTES, MAX_RAW_TRANSACTIONS, resolve_dependencies,
};
use crate::{
    bridge_cli::{print_json, read_json_limited},
    l1::Rpc,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    anchor: H256,
    type_hash: H256,
    origin: JsonPoint,
    end_transaction: H256,
    #[serde(default)]
    required_states: Vec<RequiredState>,
    #[serde(default)]
    required_dependencies: Vec<RequiredDependency>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequiredState {
    lock_hash: Option<H256>,
    data_hash: Option<H256>,
}

/// A dependency's data commitment and, optionally, the Type of its issuing registry.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
struct RequiredDependency {
    type_hash: Option<H256>,
    data_hash: H256,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Step {
    height: u64,
    transaction: JsonBytes,
    proof: TransactionAndWitnessProof,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    headers: Vec<JsonBytes>,
    steps: Vec<Step>,
    references: Vec<JsonBytes>,
}

// The request and bundle travel separately. Only the caller supplies the trusted anchor,
// the expected Type hash and the two endpoints.
pub(super) fn prove(path: &Path, url: &str) -> Result<()> {
    let request: Request = read_json_limited(path, 128 * 1024)?;
    let mut history = History::at_anchor(Rpc::new(url)?, request.anchor.clone())?;
    let mut next = request.end_transaction.pack();
    let mut steps = Vec::new();
    let mut path_hashes = HashSet::new();
    loop {
        ensure!(
            path_hashes.insert(next.clone()),
            "repeated Type history transaction"
        );
        let (tx, height, proof) = history.witnessed(&next)?;
        let (parent, _) = transition(&tx, &request.type_hash, &[], |point| {
            history.referenced(point)
        })?;
        steps.push(Step {
            height,
            transaction: JsonBytes::from_bytes(tx.data().as_bytes()),
            proof,
        });
        match parent {
            Some(point) => next = point.tx_hash(),
            None => break,
        }
    }
    steps.reverse();
    let mut external: Vec<_> = history
        .fetched()
        .filter(|tx| !path_hashes.contains(&tx.hash()))
        .collect();
    external.sort_by_key(|tx| tx.hash());
    let bundle = Bundle {
        headers: history
            .headers
            .iter()
            .map(|h| JsonBytes::from_bytes(h.data().as_bytes()))
            .collect(),
        steps,
        references: external
            .into_iter()
            .map(|tx| JsonBytes::from_bytes(tx.data().raw().as_bytes()))
            .collect(),
    };
    validate(&request, &bundle)?;
    history.check_anchor()?;
    print_json(&bundle)
}

pub(super) fn verify(path: &Path, proof: &Path) -> Result<()> {
    let request: Request = read_json_limited(path, 128 * 1024)?;
    // Hex packing of 32 MiB, plus bounded headers and transaction-and-witness Merkle paths.
    let bundle: Bundle = read_json_limited(proof, 80 * 1024 * 1024)?;
    print_json(&validate(&request, &bundle)?)
}

fn transition(
    tx: &TransactionView,
    expected: &H256,
    required: &[RequiredDependency],
    mut resolve: impl FnMut(&OutPoint) -> Result<(CellOutput, Bytes)>,
) -> Result<(Option<OutPoint>, Option<OutPoint>)> {
    let matches = |cell: CellOutput| {
        cell.type_()
            .to_opt()
            .is_some_and(|script| script.calc_script_hash().as_slice() == expected.as_bytes())
    };
    let mut previous = None;
    for input in tx.inputs() {
        let point = input.previous_output();
        if point.is_null() && tx.is_cellbase() {
            continue;
        }
        if matches(resolve(&point)?.0) {
            ensure!(
                previous.replace(point).is_none(),
                "multiple inputs carry the pinned Type"
            );
        }
    }
    ensure!(
        tx.outputs().len() == tx.outputs_data().len(),
        "CKB output data count differs"
    );
    let mut next = None;
    for (index, cell) in tx.outputs().into_iter().enumerate() {
        if matches(cell) {
            ensure!(
                next.replace(OutPoint::new(tx.hash(), index.try_into()?))
                    .is_none(),
                "multiple outputs carry the pinned Type"
            );
        }
    }
    // Authenticate every dependency and DepGroup member by the exact OutPoint in the transaction.
    let deps = resolve_dependencies(tx, resolve)?;
    if !required.is_empty() {
        let mut missing: HashSet<_> = required.iter().cloned().collect();
        for (cell, data) in deps {
            let data_hash = H256::from(ckb_hash::blake2b_256(&data));
            missing.remove(&RequiredDependency {
                type_hash: None,
                data_hash: data_hash.clone(),
            });
            if let Some(script) = cell.type_().to_opt() {
                missing.remove(&RequiredDependency {
                    type_hash: Some(script.calc_script_hash().unpack()),
                    data_hash,
                });
            }
            if missing.is_empty() {
                break;
            }
        }
        ensure!(
            missing.is_empty(),
            "required CKB dependency absent from Type transition"
        );
    }
    Ok((previous, next))
}

fn validate(request: &Request, bundle: &Bundle) -> Result<Value> {
    ensure!(
        !bundle.headers.is_empty() && bundle.headers.len() <= MAX_HEADERS as usize + 1,
        "invalid Type history Header count"
    );
    ensure!(
        !bundle.steps.is_empty()
            && bundle.steps.len() + bundle.references.len() <= MAX_RAW_TRANSACTIONS,
        "invalid Type history transaction count"
    );
    let bytes: usize = bundle
        .steps
        .iter()
        .map(|s| s.transaction.as_bytes().len())
        .chain(bundle.references.iter().map(|r| r.as_bytes().len()))
        .sum();
    ensure!(bytes <= MAX_RAW_BYTES, "Type history exceeds byte limit");

    let headers = bundle
        .headers
        .iter()
        .map(|raw| Ok(packed::Header::from_slice(raw.as_bytes())?.into_view()))
        .collect::<Result<Vec<HeaderView>>>()?;
    ensure!(
        headers[0].hash().as_slice() == request.anchor.as_bytes(),
        "wrong trusted CKB anchor"
    );
    for pair in headers.windows(2) {
        let (child, parent) = (&pair[0], &pair[1]);
        ensure!(
            child.parent_hash() == parent.hash()
                && parent.number().checked_add(1) == Some(child.number()),
            "disconnected CKB Header chain"
        );
    }
    ensure!(
        headers.last().map(HeaderView::number) == Some(bundle.steps[0].height),
        "Header path must end at the birth height"
    );

    let mut sources = HashMap::new();
    let mut unused = HashSet::new();
    for bytes in &bundle.references {
        let raw = RawTransaction::from_slice(bytes.as_bytes())?;
        let hash = raw.calc_tx_hash();
        ensure!(
            unused.insert(hash.clone()),
            "duplicate Type history reference"
        );
        sources.insert(hash, raw);
    }
    let origin: OutPoint = request.origin.clone().into();
    ensure!(
        request.required_states.len() <= bundle.steps.len().saturating_sub(2)
            && request
                .required_states
                .iter()
                .all(|s| s.lock_hash.is_some() || s.data_hash.is_some()),
        "invalid intermediate state requirements"
    );
    ensure!(
        request.required_dependencies.len() <= super::metatype_history::MAX_DEPS
            && request
                .required_dependencies
                .iter()
                .collect::<HashSet<_>>()
                .len()
                == request.required_dependencies.len(),
        "invalid Type history dependency requirements"
    );
    let mut visits = Vec::new();
    let mut head = None;
    let mut trace = Vec::new();
    let mut prior_height = 0;
    for (index, step) in bundle.steps.iter().enumerate() {
        ensure!(
            step.height >= prior_height,
            "reversed CKB transaction heights"
        );
        prior_height = step.height;
        let depth = headers[0]
            .number()
            .checked_sub(step.height)
            .context("transaction after anchor")?;
        let header = headers
            .get(usize::try_from(depth)?)
            .context("missing CKB Header")?;
        let packed = packed::Transaction::from_slice(step.transaction.as_bytes())?;
        let tx =
            stem::l1_proof::verify_packed_transaction(&header.hash(), header, packed, &step.proof)?;
        let (previous, next) = transition(
            &tx,
            &request.type_hash,
            &request.required_dependencies,
            |point| {
                let hash = point.tx_hash();
                let raw = sources
                    .get(&hash)
                    .context("missing referenced CKB transaction")?;
                unused.remove(&hash);
                let i = u32::from(point.index()) as usize;
                Ok((
                    raw.outputs().get(i).context("referenced output missing")?,
                    raw.outputs_data()
                        .get(i)
                        .context("referenced Cell data missing")?
                        .raw_data(),
                ))
            },
        )?;
        if index == 0 {
            ensure!(
                previous.is_none() && next.as_ref() == Some(&origin),
                "wrong CKB Type birth"
            );
        } else {
            ensure!(
                head.is_some() && previous == head,
                "disconnected CKB Type history"
            );
        }
        if index + 1 == bundle.steps.len() {
            ensure!(
                tx.hash().as_slice() == request.end_transaction.as_bytes(),
                "wrong final transaction"
            );
        }
        let state = if let Some(point) = &next {
            let (cell, data) = super::metatype_history::output(&tx, point)?;
            let lock_hash: H256 = cell.lock().calc_script_hash().unpack();
            let data_hash = H256::from(ckb_hash::blake2b_256(&data));
            if index > 0
                && index + 1 < bundle.steps.len()
                && let Some(required) = request.required_states.get(visits.len())
                && required
                    .lock_hash
                    .as_ref()
                    .is_none_or(|hash| hash == &lock_hash)
                && required
                    .data_hash
                    .as_ref()
                    .is_none_or(|hash| hash == &data_hash)
            {
                visits.push(JsonPoint::from(point.clone()));
            }
            Some(json!({"out_point": JsonPoint::from(point.clone()),
                "lock_hash": lock_hash, "data_hash": data_hash, "data_bytes": data.len()}))
        } else {
            None
        };
        trace.push(
            json!({"height": step.height, "transaction": H256::from_slice(tx.hash().as_slice())?,
            "output": state}),
        );
        head = next;
        ensure!(
            sources.insert(tx.hash(), tx.data().raw()).is_none(),
            "duplicate Type history transaction"
        );
    }
    ensure!(
        visits.len() == request.required_states.len(),
        "required intermediate state absent or out of order"
    );
    ensure!(unused.is_empty(), "unused CKB Type history references");
    let mut result = json!({"anchor":request.anchor, "type_hash":request.type_hash, "origin":request.origin,
        "end_transaction":request.end_transaction, "ended_in_burn":head.is_none(), "matched_states":visits, "transitions":trace});
    if !request.required_dependencies.is_empty() {
        result["required_dependencies_per_transition"] = json!(request.required_dependencies);
    }
    Ok(result)
}
