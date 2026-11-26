//! One CKB Type Script dispatches all virtual Type Groups of a deployment.

#![no_std]
#![no_main]

use alloc::vec::Vec;

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        encode_hex, load_cell_type_hash, load_header, load_script, load_script_hash, spawn_cell,
    },
    syscalls::wait,
};
use stem_bridge_wire::metatype::{
    Invocation, MAX_ADAPTER_TABLE_BYTES, MAX_CELLS_PER_SIDE, MAX_TRANSACTION_CELLS_PER_SIDE,
    MetatypeAdapterTable, MetatypeDeployment, custody::Boundary, project, witness::GroupWitnesses,
};
use stem_contracts::metatype::{ManifestResolver, bounded_data, witness_data};
use stem_contracts::metatype_custody::require_gate;
use stem_stake::Hash32;

entry!(main);
default_alloc!();

const MAX_LIVE_DATA: usize = 128 * 1024;

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
    let (input_count, inputs) = cells(Source::Input, &own_type, &mut remaining, &mut manifests)?;
    let (_, outputs) = cells(Source::Output, &own_type, &mut remaining, &mut manifests)?;
    let inputs = borrowed(&inputs);
    let outputs = borrowed(&outputs);
    let boundary = match table.custody {
        None => Boundary::Standalone,
        Some(custody) => {
            let boundary = Boundary::for_cardinality(inputs.len(), outputs.len()).ok_or(48)?;
            if inputs
                .iter()
                .chain(&outputs)
                .any(|(_, data)| custody.data(data).is_none())
            {
                return Err(48);
            }
            if boundary == Boundary::Transfer {
                if inputs[0].1 != outputs[0].1 {
                    return Err(48);
                }
            } else {
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

fn cells(
    source: Source,
    own_type: &Hash32,
    remaining: &mut usize,
    manifests: &mut ManifestResolver,
) -> Result<(usize, Vec<(u16, Vec<u8>)>), i8> {
    let mut matched = Vec::new();
    for index in 0..=MAX_TRANSACTION_CELLS_PER_SIDE {
        match load_cell_type_hash(index, source) {
            Ok(ty) if index < MAX_TRANSACTION_CELLS_PER_SIDE => {
                if ty.as_ref() == Some(own_type) {
                    if matched.len() == MAX_CELLS_PER_SIDE {
                        return Err(43);
                    }
                    let data = manifests.load(index as u16, source, *remaining).ok_or(43)?;
                    *remaining -= data.len();
                    matched.push((index as u16, data));
                }
            }
            Err(SysError::IndexOutOfBound) => return Ok((index, matched)),
            _ => return Err(43),
        }
    }
    unreachable!("bounded Cell enumeration")
}

fn borrowed(cells: &[(u16, Vec<u8>)]) -> Vec<(u16, &[u8])> {
    cells
        .iter()
        .map(|(index, data)| (*index, data.as_slice()))
        .collect()
}
