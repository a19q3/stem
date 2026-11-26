//! Recover a return proof from the actual witnessed CKB custody chain.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{JsonBytes, OutPoint as JsonPoint};
use ckb_types::{H256, core::TransactionView, packed::OutPoint, prelude::*};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use serde_json::json;
use stem_bridge_wire::{
    intercell::Identity,
    metatype::{
        witness::{GroupWitnesses, MAGIC},
        workflow::{State, Workflow, witness_proof},
    },
};
use stem_ledger::Cell;

use super::metatype_history::{History, Route, output};
use crate::bridge_cli::{print_json, read_json_limited};

const MAX_CUSTODY_TRANSACTIONS: usize = 64;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    escrow_cell: JsonBytes,
    workflow_script_hash: H256,
    import: JsonPoint,
    burn: JsonPoint,
    anchor: H256,
}

pub(super) fn recover(request: &Path, l1_rpc: &str) -> Result<()> {
    let request: Request = read_json_limited(request, 128 * 1024)?;
    let escrow = Cell::from_slice(request.escrow_cell.as_bytes())?;
    let rule: Identity = request.workflow_script_hash.0;
    let script = escrow
        .type_scripts()
        .find(|script| script.calc_script_hash().as_slice() == rule)
        .context("workflow Script is absent from the Stem escrow Cell")?;
    let args = script.args().raw_data();
    let workflow = Workflow::decode(&args).context("invalid workflow Script args")?;
    ensure!(
        escrow.lock().code_hash().as_slice() == workflow.escrow_code,
        "Stem Cell does not use the workflow's pinned escrow Lock code"
    );
    let route = Route::new(&escrow, workflow.network, workflow.object)?;
    let mut history = History::open(l1_rpc, request.anchor, &route.genesis)?;
    let import: OutPoint = request.import.into();
    let burn: OutPoint = request.burn.into();
    let (burn_tx, data) = route.burn(&mut history, &burn)?;
    let returned = route.returned(&data)?;
    ensure!(
        proof(&burn_tx, &route.gate.metatype, rule)?.is_empty(),
        "burn carries a workflow update"
    );

    let mut point = route
        .predecessor(&mut history, &burn_tx)?
        .context("burn has no Metatype input")?;
    let mut reverse = Vec::new();
    loop {
        ensure!(
            reverse.len() < MAX_CUSTODY_TRANSACTIONS,
            "CKB object history exceeds transaction limit"
        );
        let (tx, height) = history.included(&point.tx_hash())?;
        route.check_transaction(&tx)?;
        ensure!(
            route.unique_output(&tx)? == Some(u32::from(point.index()) as usize),
            "CKB history does not recreate the exact Metatype output"
        );
        let (_, data) = output(&tx, &point)?;
        let data = route.state(&mut history, &tx, data)?;
        let witness = proof(&tx, &route.gate.metatype, rule)?;
        let step = State::decode(&data, &rule).context("invalid CKB workflow state")?;
        ensure!(!step.is_initial(), "unopened workflow in CKB custody");
        reverse.push((point.clone(), height, step, witness, data));
        let previous = route.predecessor(&mut history, &tx)?;
        if point == import {
            ensure!(
                previous.is_none() && route.consumes_mailbox(&mut history, &tx)?,
                "named import does not introduce the pinned object through its mailbox"
            );
            break;
        }
        point = previous.context("CKB custody path did not reach the named import")?;
    }
    let mut steps = reverse.into_iter().rev();
    let (entry, height, mut state, witness, initial) =
        steps.next().context("empty CKB custody history")?;
    ensure!(
        initial == escrow.data() && witness.is_empty(),
        "CKB import does not match the exact escrowed state"
    );
    let mut trace =
        vec![json!({"out_point": JsonPoint::from(entry), "height": height, "step": false})];
    let mut proof_data = Vec::new();
    let mut last = initial;
    for (point, height, next, witness, data) in steps {
        ensure!(
            workflow.verify_successor(&rule, state, next, &witness, 1, verify_signature),
            "invalid witnessed workflow transition at {point:?}"
        );
        trace.push(json!({"out_point": JsonPoint::from(point), "height": height, "step": !witness.is_empty()}));
        proof_data.extend_from_slice(&witness);
        state = next;
        last = data;
    }
    ensure!(
        last.as_ref() == returned.state,
        "CKB burn does not commit the last witnessed state"
    );
    history.check_anchor()?;
    print_json(&json!({
        "proof_data": JsonBytes::from_vec(proof_data),
        "record": JsonBytes::from_vec(returned.record.to_vec()),
        "state": JsonBytes::from_vec(returned.state.to_vec()),
        "message_id": H256::from(returned.message_id),
        "recipient": H256::from(returned.recipient),
        "anchor": history.anchor,
        "burn": JsonPoint::from(burn),
        "trace": trace,
    }))
}

fn verify_signature(key: &Identity, digest: &Identity, signature: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(key).is_ok_and(|key| {
        key.verify_strict(digest, &Signature::from_bytes(signature))
            .is_ok()
    })
}

fn proof(tx: &TransactionView, deployment: &Identity, rule: Identity) -> Result<Vec<u8>> {
    let extra = tx
        .witnesses()
        .get(tx.inputs().len())
        .map(|raw| raw.raw_data());
    let group = match extra.as_deref() {
        Some(raw) if raw.starts_with(MAGIC) => GroupWitnesses::decode(raw)
            .context("invalid CKB group witness")?
            .for_group(deployment, rule),
        _ => None,
    };
    Ok(witness_proof(group)
        .context("invalid CKB workflow witness row")?
        .to_vec())
}
