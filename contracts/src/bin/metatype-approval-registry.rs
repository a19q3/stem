//! CKB Type ID registry for a rotating Metatype approval key.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::core::ScriptHashType,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_lock_hash, load_script},
    type_id::validate_type_id,
};
use stem_bridge_wire::metatype::rotating_approval::Authority;
use stem_contracts::metatype::{bounded_data, workflow_key};

entry!(main);
default_alloc!();

fn main() -> i8 {
    if verify().is_some() { 0 } else { 61 }
}

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let id = script.args().raw_data();
    if id.len() != 32 || u8::from(script.hash_type()) != ScriptHashType::Data2 as u8 {
        return None;
    }
    validate_type_id(&id).ok()?;
    let owner = load_cell_lock_hash(0, Source::GroupOutput).ok()?;
    let read = |source| Authority::decode(&bounded_data(0, source, Authority::SIZE).ok()?);
    let next = read(Source::GroupOutput)?;
    workflow_key(&next.public_key)?;
    match load_cell_lock_hash(0, Source::GroupInput) {
        Err(SysError::IndexOutOfBound) if next.generation == 0 => Some(()),
        Ok(before) if before == owner && next.is_successor(&read(Source::GroupInput)?) => Some(()),
        _ => None,
    }
}
