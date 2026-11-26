//! CKB Type ID registry for a rotating Metatype approval quorum and freeze state.

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
    let read = |source| bounded_data(0, source, Authority::MAX_RECORD_BYTES).ok();
    let record = read(Source::GroupOutput)?;
    let next = Authority::decode(&record)?;
    for key in next.keys() {
        let point = workflow_key(key)?.to_edwards();
        if point.compress().as_bytes() != key || !point.is_torsion_free() {
            return None;
        }
    }
    match load_cell_lock_hash(0, Source::GroupInput) {
        Err(SysError::IndexOutOfBound) if next.generation == 0 => Some(()),
        Ok(before) if before == owner => {
            let record = read(Source::GroupInput)?;
            next.is_successor(&Authority::decode(&record)?)
                .then_some(())
        }
        _ => None,
    }
}
