//! Fixed-supply UDT for the Lineage reserve example. Each Type Group sums
//! exactly one bound slot. Development genesis provides the initial balances.
#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_data, load_script},
};

entry!(main);
default_alloc!();

fn sum(source: Source, offset: usize) -> Option<u128> {
    let mut total = 0u128;
    for index in 0.. {
        match load_cell_data(index, source) {
            Ok(data) if data.len() == 65 => {
                let value = u128::from_le_bytes(data[offset..offset + 16].try_into().ok()?);
                total = total.checked_add(value)?;
            }
            Err(SysError::IndexOutOfBound) => return Some(total),
            _ => return None,
        }
    }
    unreachable!()
}

fn main() -> i8 {
    let Ok(script) = load_script() else { return 10 };
    let args = script.args().raw_data();
    // The two slots define the example's two fixed asset identities.
    if args.len() != 1 || args[0] > 1 {
        return 11;
    }
    let offset = usize::from(args[0]) * 16;
    match (
        sum(Source::GroupInput, offset),
        sum(Source::GroupOutput, offset),
    ) {
        (Some(input), Some(output)) if input == output => 0,
        _ => 12,
    }
}
