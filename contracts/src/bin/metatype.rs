//! One CKB Type Script dispatches all virtual Type Groups of a deployment.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, prelude::*},
    default_alloc, entry,
    high_level::{encode_hex, load_header, load_script, load_script_hash, spawn_cell},
    syscalls::wait,
};
use stem_bridge_wire::metatype::{
    Invocation, MAX_ADAPTER_TABLE_BYTES, MAX_LIVE_DATA, MetatypeAdapterTable, MetatypeDeployment,
    custody::Boundary, project, witness::GroupWitnesses,
};
use stem_contracts::metatype::{ManifestResolver, borrowed, bounded_data, witness_data};
use stem_contracts::metatype_custody::require_gate;
use stem_stake::Hash32;

entry!(main);
default_alloc!();

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 41)?;
    if u8::from(script.hash_type()) != ScriptHashType::Data2 as u8 {
        return Err(41);
    }
    let own_code: Hash32 = script.code_hash().unpack();
    let own_type = load_script_hash().map_err(|_| 41)?;
    let deployment = MetatypeDeployment::decode(&script.args().raw_data()).ok_or(41)?;
    let genesis = load_header(0, Source::HeaderDep).map_err(|_| 41)?;
    let height: u64 = genesis.raw().number().unpack();
    if height != 0 || genesis.calc_header_hash().as_slice() != deployment.ckb_genesis {
        return Err(41);
    }
    let mut manifests = ManifestResolver::default();
    let table_index = manifests
        .dependency_hashes()
        .map_err(|_| 42)?
        .iter()
        .position(|hash| hash == &deployment.adapter_table_data_hash)
        .ok_or(42)?;
    let raw_table =
        bounded_data(table_index, Source::CellDep, MAX_ADAPTER_TABLE_BYTES).map_err(|_| 42)?;
    let table = MetatypeAdapterTable::decode(&raw_table).ok_or(42)?;

    let mut remaining = MAX_LIVE_DATA;
    let (input_count, inputs) = manifests.cells(Source::Input, &own_type, &mut remaining)?;
    let (_, outputs) = manifests.cells(Source::Output, &own_type, &mut remaining)?;
    let inputs = borrowed(&inputs);
    let outputs = borrowed(&outputs);
    let boundary = match table.custody {
        None => Boundary::Standalone,
        Some(custody) => {
            let (boundary, _) = custody.transition(&inputs, &outputs).ok_or(48)?;
            if boundary != Boundary::Transfer {
                require_gate(&custody, &own_type, &deployment.ckb_genesis)?;
            }
            boundary
        }
    };
    let groups = project(&inputs, &outputs).ok_or(43)?;
    let raw_witness = witness_data(input_count).map_err(|_| 47)?;
    let witnesses = match raw_witness.as_deref() {
        Some(raw) => {
            let witnesses = GroupWitnesses::decode(raw).ok_or(47)?;
            if !witnesses.validate_groups(&own_type, &groups) {
                return Err(47);
            }
            witnesses
        }
        None => GroupWitnesses::default(),
    };
    let available = manifests.dependency_hashes().map_err(|_| 42)?;
    for group in groups {
        let adapter = table.adapter(group.script).ok_or(44)?;
        if adapter == &own_code || !available.contains(adapter) {
            return Err(44);
        }
        let argument = encode_hex(
            &Invocation {
                deployment_type: &own_type,
                boundary,
                witness: witnesses.for_group(&own_type, group.script.id()),
                group,
            }
            .encode(),
        );
        let child = spawn_cell(adapter, ScriptHashType::Data2, &[argument.as_c_str()], &[])
            .map_err(|_| 45)?;
        if wait(child).map_err(|_| 45)? != 0 {
            return Err(46);
        }
    }
    Ok(())
}
