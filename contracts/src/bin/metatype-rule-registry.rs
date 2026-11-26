//! CKB Type ID registry that can only increase an object's state byte ceiling.

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
use stem_bridge_wire::metatype::object_policy::StateLimit;
use stem_contracts::metatype::bounded_data;

entry!(main);
default_alloc!();

fn main() -> i8 {
    if verify().is_some() { 0 } else { 57 }
}

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let id = script.args().raw_data();
    if id.len() != 32 || u8::from(script.hash_type()) != ScriptHashType::Data2 as u8 {
        return None;
    }
    validate_type_id(&id).ok()?;
    let owner = load_cell_lock_hash(0, Source::GroupOutput).ok()?;
    let read_limit =
        |source| StateLimit::decode(&bounded_data(0, source, StateLimit::ENCODED_BYTES).ok()?);
    let next = read_limit(Source::GroupOutput)?;
    match load_cell_lock_hash(0, Source::GroupInput) {
        Err(SysError::IndexOutOfBound) => Some(()),
        Ok(before) if before == owner && next.increases(&read_limit(Source::GroupInput)?) => {
            Some(())
        }
        _ => None,
    }
}
