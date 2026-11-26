//! Recover the pinned route preimages needed to release an oversized object.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{JsonBytes, OutPoint as JsonPoint};
use ckb_types::{H256, core::ScriptHashType, packed::OutPoint, prelude::*};
use serde::Deserialize;
use serde_json::json;
use stem_bridge_wire::metatype::{
    LogicalScript, MAX_ADAPTER_TABLE_BYTES, MetatypeAdapterTable, MetatypeDeployment,
    object_policy::{ObjectPolicy, RouteCodes, RuleSource, StateLimit},
};
use stem_ledger::Cell;

use super::metatype_history::{History, Route, output};
use crate::bridge_cli::{print_json, read_json_limited};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    escrow_cell: JsonBytes,
    ceiling_script_hash: H256,
    stem_network: H256,
    burn: JsonPoint,
    anchor: H256,
}

pub(super) fn recover(request: &Path, l1_rpc: &str) -> Result<()> {
    let request: Request = read_json_limited(request, 128 * 1024)?;
    let escrow = Cell::from_slice(request.escrow_cell.as_bytes())?;
    let script = escrow
        .type_scripts()
        .find(|script| {
            script.calc_script_hash().as_slice() == request.ceiling_script_hash.as_bytes()
        })
        .context("ceiling Script is absent from the Stem escrow Cell")?;
    let logical = LogicalScript::decode(script.as_slice()).context("invalid ceiling Script")?;
    let policy = ObjectPolicy::decode(logical.args()).context("invalid ceiling Script args")?;
    let RuleSource::LiveCkbType {
        registry,
        route: commitment,
    } = policy.rule
    else {
        anyhow::bail!("ceiling Script must use a live CKB registry");
    };
    let route = Route::new(&escrow, &request.stem_network.0, policy.object)?;
    let mut history = History::open(l1_rpc, request.anchor, &route.genesis)?;
    let burn: OutPoint = request.burn.into();
    let (burn_tx, data) = route.burn(&mut history, &burn)?;
    let returned = route.returned(&data)?;
    let previous = route
        .predecessor(&mut history, &burn_tx)?
        .context("burn has no Metatype input")?;
    let (previous_tx, _) = history.included(&previous.tx_hash())?;
    route.check_transaction(&previous_tx)?;
    ensure!(
        route.unique_output(&previous_tx)? == Some(u32::from(previous.index()) as usize),
        "CKB history does not recreate the exact Metatype output"
    );
    let (input, state) = output(&previous_tx, &previous)?;
    let state = route.state(&mut history, &previous_tx, state)?;
    ensure!(
        state.as_ref() == returned.state,
        "returned state differs from the burned object"
    );

    let wrapper = input
        .type_()
        .to_opt()
        .context("burned object has no Metatype Script")?;
    ensure!(
        u8::from(wrapper.hash_type()) == ScriptHashType::Data2 as u8
            && wrapper.calc_script_hash().as_slice() == route.gate.metatype
            && wrapper.as_slice().len() == RouteCodes::WRAPPER_SIZE,
        "burned object has a different physical Metatype Script"
    );
    let deployment = MetatypeDeployment::decode(&wrapper.args().raw_data())
        .context("invalid physical Metatype deployment")?;
    ensure!(
        deployment.ckb_genesis == route.genesis,
        "Metatype deployment has a different genesis"
    );

    let deps = history.dependencies(&burn_tx)?;
    let data_hashes = deps
        .iter()
        .map(|(_, data)| ckb_hash::blake2b_256(data))
        .collect::<Vec<_>>();
    let table = deps
        .iter()
        .zip(&data_hashes)
        .find(|((_, data), hash)| {
            data.len() <= MAX_ADAPTER_TABLE_BYTES && **hash == deployment.adapter_table_data_hash
        })
        .map(|((_, data), _)| data)
        .context("pinned adapter table is absent from the burn dependencies")?;
    let adapters =
        MetatypeAdapterTable::decode(table).context("invalid burned deployment adapter table")?;
    let custody = route.gate.custody;
    ensure!(
        adapters.custody == Some(custody),
        "adapter table changes the escrow custody policy"
    );
    let adapter = adapters
        .adapter(logical)
        .context("ceiling adapter is absent from the table")?;
    let codes: [[u8; 32]; 5] = [
        escrow.lock().code_hash().unpack(),
        wrapper.code_hash().unpack(),
        custody.guard_code,
        custody.mailbox_code,
        *adapter,
    ];
    let codes = codes.as_flattened();
    ensure!(
        RouteCodes::authenticate(codes, commitment)
            .is_some_and(|route| route.matches_custody(&custody, policy.object)),
        "burned deployment differs from the ceiling's pinned custody code"
    );
    for hash in codes.as_chunks::<32>().0.iter().skip(1) {
        ensure!(
            data_hashes.contains(hash),
            "pinned CKB code is absent from the burn dependencies"
        );
    }

    let mut records = deps.iter().filter(|(cell, _)| {
        cell.type_()
            .to_opt()
            .is_some_and(|script| script.calc_script_hash().as_slice() == registry)
    });
    let (_, data) = records
        .next()
        .context("ceiling registry is absent from the burn dependencies")?;
    ensure!(
        records.next().is_none(),
        "burn names multiple ceiling registry dependencies"
    );
    StateLimit::decode(data).context("invalid ceiling at burn")?;
    let ceiling_at_burn = u16::from_le_bytes(data.as_ref().try_into()?);
    let proof = [codes, wrapper.as_slice(), table.as_ref()].concat();
    history.check_anchor()?;
    print_json(&json!({
        "proof_data": JsonBytes::from_vec(proof),
        "record": JsonBytes::from_vec(returned.record.to_vec()),
        "state": JsonBytes::from_vec(returned.state.to_vec()),
        "message_id": H256::from(returned.message_id),
        "recipient": H256::from(returned.recipient),
        "registry_type": H256::from(*registry),
        "ceiling_at_burn": ceiling_at_burn,
        "ceiling_script_hash": request.ceiling_script_hash,
        "anchor": history.anchor,
        "burn": JsonPoint::from(burn),
        "object_input": JsonPoint::from(previous),
    }))
}
