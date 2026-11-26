//! Example receipt issuer that requires two independent CKB input Lock groups.
#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_capacity, load_cell_lock_hash, load_script},
};
use stem_bridge_wire::object::permit::{Permit, SIZE};
use stem_contracts::metatype::bounded_data;

entry!(main);
default_alloc!();

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let args = script.args().raw_data();
    let (keys, tail) = args.as_chunks::<32>();
    let [first, second] = keys else { return None };
    if !tail.is_empty()
        || *first == [0; 32]
        || first >= second
        || load_cell_capacity(1, Source::GroupOutput) != Err(SysError::IndexOutOfBound)
    {
        return None;
    }
    // Spending without reissuing revokes the permit and needs both approvals too.
    match bounded_data(0, Source::GroupOutput, SIZE) {
        Ok(raw) => {
            Permit::decode(&raw)?;
        }
        Err(SysError::IndexOutOfBound) => {}
        Err(_) => return None,
    }
    let (mut a, mut b) = (false, false);
    for index in 0.. {
        let lock = load_cell_lock_hash(index, Source::Input).ok()?;
        a |= lock == *first;
        b |= lock == *second;
        if a && b {
            return Some(());
        }
    }
    None
}

fn main() -> i8 {
    if verify().is_some() { 0 } else { 121 }
}
