//! A native application UDT with explicit issuance authorization.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_lock_hash, load_script, load_script_hash},
};
use stem_bridge_wire::metatype::fungible::Udt;
use stem_contracts::native_type;
use stem_stake::Hash32;

entry!(main);
default_alloc!();

fn total(source: Source, asset: &Hash32, required: Option<&Hash32>) -> Option<u128> {
    let mut amount = 0u128;
    for index in 0.. {
        match native_type::amount(source, index, asset) {
            Ok(units) => {
                if required
                    .is_some_and(|hash| native_type::contains(source, index, hash) != Some(true))
                {
                    return None;
                }
                amount = amount.checked_add(units)?;
            }
            Err(SysError::IndexOutOfBound) => return Some(amount),
            Err(_) => return None,
        }
    }
    unreachable!()
}

fn main() -> i8 {
    let Ok(script) = load_script() else { return 90 };
    let args = script.args().raw_data();
    let Ok(asset) = load_script_hash() else {
        return 90;
    };
    let Some(udt) = Udt::decode(&args, &asset) else {
        return 91;
    };
    let policy = udt.policy.as_ref().map(ckb_hash::blake2b_256);
    let Some(input) = total(Source::GroupInput, &asset, policy.as_ref()) else {
        return 92;
    };
    let Some(output) = total(Source::GroupOutput, &asset, policy.as_ref()) else {
        return 92;
    };
    if input == output {
        return 0;
    }
    if input > output {
        return 93;
    }
    for index in 0.. {
        match load_cell_lock_hash(index, Source::Input) {
            Ok(hash) if hash == *udt.issuer => return 0,
            Ok(_) => {}
            Err(_) => return 94,
        }
    }
    unreachable!()
}
