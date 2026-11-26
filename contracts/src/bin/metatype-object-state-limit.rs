//! Portable adapter for the native object state limit and its rule source.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    high_level::load_script,
};
use stem_bridge_wire::metatype::{
    Invocation, LogicalScript, MAX_ADAPTER_TABLE_BYTES, MetatypeAdapterTable, MetatypeDeployment,
    custody::{Boundary, EscrowArgs},
    object_policy::{ObjectPolicy, RouteCodes, RuleSource, StateLimit, valid_state},
};
use stem_contracts::{
    metatype::{CellData, ManifestResolver, typed_dependency_data, with_invocation},
    metatype_custody::require_gate,
};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = &invocation.group;
        if Boundary::for_cardinality(group.input_indices.len(), group.output_indices.len())?
            != invocation.boundary
        {
            return None;
        }
        let policy = ObjectPolicy::decode(group.script.args())?;
        let mut read = |indices: &[u16], source| match indices {
            [] => Some(None),
            [index] => Some(Some(manifests.manifest_data(*index, source)?)),
            _ => None,
        };
        let input = read(&group.input_indices, Source::Input)?;
        let output = read(&group.output_indices, Source::Output)?;
        let before = match input.as_ref() {
            Some(cell) => Some(state(cell, group.script, policy.object)?),
            None => None,
        };
        let after = match output.as_ref() {
            Some(cell) => Some(state(cell, group.script, policy.object)?),
            None => None,
        };
        match policy.rule {
            RuleSource::DataHash(hash) => {
                let raw = manifests.dependency_data(hash, StateLimit::ENCODED_BYTES)?;
                let limit = StateLimit::decode(&raw)?;
                (before.is_none_or(|data| limit.permits(data))
                    && after.is_none_or(|data| limit.permits(data)))
                .then_some(())
            }
            RuleSource::LiveCkbType { registry, route } => {
                let limit = StateLimit::decode(&typed_dependency_data(
                    registry,
                    StateLimit::ENCODED_BYTES,
                )?)?;
                match (invocation.boundary, after) {
                    (Boundary::Return, None) => Some(()),
                    (_, Some(after)) if limit.permits_successor(before, after) => Some(()),
                    (Boundary::Import, Some(_)) => {
                        authorized_import(&invocation, manifests, policy.object, route)
                    }
                    _ => None,
                }
            }
        }
    })
}

fn state<'a>(data: &'a CellData, rule: LogicalScript<'_>, object: &[u8; 32]) -> Option<&'a [u8]> {
    let manifest = data.view()?;
    if !manifest.state_for(rule)?.is_empty() {
        return None;
    }
    let state = manifest
        .entries()
        .iter()
        .find(|entry| &entry.script.id() == object)?
        .state;
    valid_state(state).then_some(state)
}

fn authorized_import(
    invocation: &Invocation<'_>,
    manifests: &mut ManifestResolver,
    object: &[u8; 32],
    commitment: &[u8; 32],
) -> Option<()> {
    let witness = invocation.witness?;
    if witness.action != 1 || !witness.message.is_empty() || !witness.seal.is_empty() {
        return None;
    }
    let codes = RouteCodes::authenticate(witness.proof, commitment)?;
    let wrapper = load_script().ok()?;
    if wrapper.code_hash().as_slice() != codes.dispatcher
        || u8::from(wrapper.hash_type()) != ScriptHashType::Data2 as u8
    {
        return None;
    }
    let deployment = MetatypeDeployment::decode(&wrapper.args().raw_data())?;
    let table =
        manifests.dependency_data(&deployment.adapter_table_data_hash, MAX_ADAPTER_TABLE_BYTES)?;
    let table = MetatypeAdapterTable::decode(&table)?;
    let custody = table.custody?;
    if !codes.matches_custody(&custody, object)
        || table.adapter(invocation.group.script) != Some(codes.adapter)
    {
        return None;
    }
    let (gate, mailbox) = require_gate(
        &custody,
        invocation.deployment_type,
        &deployment.ckb_genesis,
    )
    .ok()?;
    let escrow = EscrowArgs {
        ckb: &deployment.ckb_genesis,
        metatype: invocation.deployment_type,
        checkpoint_code: &mailbox.checkpoint_code,
        checkpoint_id: &mailbox.checkpoint_id,
        custody,
    };
    let lock = Script::new_builder()
        .code_hash(codes.escrow.pack())
        .hash_type(ScriptHashType::Data)
        .args(escrow.encode().as_slice().pack())
        .build();
    (lock.calc_script_hash().as_slice() == gate.stem_escrow).then_some(())
}
