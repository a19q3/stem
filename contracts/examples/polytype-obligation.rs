//! Test policy. A later Type binds part of an existing asset to a release authority.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_lock_hash, load_script},
};
use stem_contracts::native_type;
use stem_stake::Hash32;

entry!(main);
default_alloc!();

fn constrained_units(source: Source, asset: &Hash32) -> Option<u128> {
    let mut total = 0u128;
    for index in 0.. {
        match native_type::amount(source, index, asset) {
            Ok(units) if native_type::contains(source, index, asset) == Some(true) => {
                total = total.checked_add(units)?;
            }
            Err(SysError::IndexOutOfBound) => return Some(total),
            _ => return None,
        }
    }
    unreachable!()
}

fn main() -> i8 {
    let Ok(script) = load_script() else { return 61 };
    let args = script.args().raw_data();
    let [asset, authority] = args.as_chunks::<32>().0 else { return 61 };
    if args.len() != 64 {
        return 61;
    }
    let Some(input) = constrained_units(Source::GroupInput, asset) else { return 62 };
    let Some(output) = constrained_units(Source::GroupOutput, asset) else { return 63 };
    if output >= input {
        return 0;
    }
    for index in 0.. {
        match load_cell_lock_hash(index, Source::Input) {
            Ok(lock) if lock == *authority => return 0,
            Ok(_) => {}
            Err(_) => return 64,
        }
    }
    unreachable!()
}
