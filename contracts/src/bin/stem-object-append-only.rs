//! Native Type Script binding an object's revisions to an append-only prefix.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_capacity, load_script},
};
use stem_bridge_wire::{
    intercell::Identity,
    metatype::MAX_STATE_BYTES,
    object::{revision, valid_append_only_transition},
};
use stem_contracts::{metatype::bounded_data, native_type};

entry!(main);
default_alloc!();

fn main() -> i8 {
    if verify().is_some() { 0 } else { 58 }
}

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let args = script.args().raw_data();
    let object: &Identity = args.as_ref().try_into().ok()?;
    if *object == [0; 32] {
        return None;
    }
    let read = |source| match bounded_data(0, source, MAX_STATE_BYTES) {
        Ok(data)
            if native_type::contains(source, 0, object) == Some(true)
                && load_cell_capacity(1, source) == Err(SysError::IndexOutOfBound) =>
        {
            Some(Some(data))
        }
        Err(SysError::IndexOutOfBound) => Some(None),
        _ => None,
    };
    let after = read(Source::GroupOutput)??;
    match read(Source::GroupInput)? {
        Some(before) if valid_append_only_transition(&before, &after) => Some(()),
        None if revision(&after) == Some(0) => Some(()),
        _ => None,
    }
}
