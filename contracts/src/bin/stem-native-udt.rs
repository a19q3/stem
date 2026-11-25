//! A native application UDT with explicit issuance authorization.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_data, load_cell_lock_hash, load_script},
};

entry!(main);
default_alloc!();

fn total(source: Source) -> Option<u128> {
    let mut amount = 0u128;
    for index in 0.. {
        match load_cell_data(index, source) {
            Ok(data) => {
                let units = u128::from_le_bytes(data.as_slice().try_into().ok()?);
                if units == 0 {
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
    let (keys, tail) = args.as_chunks::<32>();
    let [issuer, series] = keys else { return 91 };
    if !tail.is_empty() || *issuer == [0; 32] || *series == [0; 32] {
        return 91;
    }
    let Some(input) = total(Source::GroupInput) else {
        return 92;
    };
    let Some(output) = total(Source::GroupOutput) else {
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
            Ok(hash) if hash == *issuer => return 0,
            Ok(_) => {}
            Err(_) => return 94,
        }
    }
    unreachable!()
}
