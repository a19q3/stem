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
        encode_hex, load_cell_data_hash, load_cell_type_hash, load_script, load_script_hash,
        spawn_cell,
    },
    syscalls::wait,
};
use stem_bridge_wire::metatype::{
    Invocation, MAX_ADAPTER_TABLE_BYTES, MAX_CELLS_PER_SIDE, MAX_MANIFEST_BYTES,
    MAX_TRANSACTION_CELLS_PER_SIDE, MetatypeAdapterTable, MetatypeDeployment, project,
};
use stem_contracts::metatype::bounded_data;
use stem_stake::Hash32;

entry!(main);
default_alloc!();

const MAX_DEPS: usize = 128;
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
    let mut available = Vec::new();
    for index in 0..=MAX_DEPS {
        match load_cell_data_hash(index, Source::CellDep) {
            Ok(hash) if index < MAX_DEPS => available.push(hash),
            Err(SysError::IndexOutOfBound) => break,
            _ => return Err(42),
        }
    }
    let table_index = available
        .iter()
        .position(|hash| hash == &deployment.adapter_table_data_hash)
        .ok_or(42)?;
    let raw_table =
        bounded_data(table_index, Source::CellDep, MAX_ADAPTER_TABLE_BYTES).map_err(|_| 42)?;
    let table = MetatypeAdapterTable::decode(&raw_table).ok_or(42)?;

    let mut remaining = MAX_LIVE_DATA;
    let inputs = cells(Source::Input, &own_type, &mut remaining)?;
    let outputs = cells(Source::Output, &own_type, &mut remaining)?;
    let inputs = borrowed(&inputs);
    let outputs = borrowed(&outputs);
    let groups = project(&inputs, &outputs).ok_or(43)?;
    for group in groups {
        let adapter = table.adapter(group.script).ok_or(44)?;
        if adapter == &own_code || !available.contains(adapter) {
            return Err(44);
        }
        let argument = encode_hex(
            &Invocation {
                deployment_type: &own_type,
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
) -> Result<Vec<(u16, Vec<u8>)>, i8> {
    let mut matched = Vec::new();
    for index in 0..=MAX_TRANSACTION_CELLS_PER_SIDE {
        match load_cell_type_hash(index, source) {
            Ok(ty) if index < MAX_TRANSACTION_CELLS_PER_SIDE => {
                if ty.as_ref() == Some(own_type) {
                    if matched.len() == MAX_CELLS_PER_SIDE {
                        return Err(43);
                    }
                    let data = bounded_data(index, source, MAX_MANIFEST_BYTES.min(*remaining))
                        .map_err(|_| 43)?;
                    *remaining -= data.len();
                    matched.push((index as u16, data));
                }
            }
            Err(SysError::IndexOutOfBound) => return Ok(matched),
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
