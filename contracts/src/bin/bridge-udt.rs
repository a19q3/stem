#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_data, load_script},
};

entry!(main);
default_alloc!();

fn issued() -> Option<u128> {
    let mut data = [0; 16];
    let mut length = 16u64;
    let mut result = data.as_mut_ptr() as u64;
    unsafe {
        asm!("ecall", inout("a0") result, in("a1") &mut length,
             in("a2") 0u64, in("a3") 0u64, in("a4") 0u64,
             in("a5") 0u64, in("a7") 2120u64);
    }
    (result == 0 && length == 16).then_some(u128::from_le_bytes(data))
}

fn sum(source: Source) -> Option<u128> {
    let mut total = 0u128;
    for index in 0.. {
        match load_cell_data(index, source) {
            Ok(data) => {
                let amount = u128::from_le_bytes(data.as_slice().try_into().ok()?);
                if amount == 0 {
                    return None;
                }
                total = total.checked_add(amount)?;
            }
            Err(SysError::IndexOutOfBound) => return Some(total),
            Err(_) => return None,
        }
    }
    unreachable!()
}

fn main() -> i8 {
    // Args commit the complete original L1 Type Script hash.
    if load_script()
        .ok()
        .is_none_or(|script| script.args().raw_data().len() != 32)
    {
        return 10;
    }
    let Some(input) = sum(Source::GroupInput) else {
        return 11;
    };
    let Some(output) = sum(Source::GroupOutput) else {
        return 12;
    };
    let Some(grant) = issued() else { return 13 };
    if input.checked_add(grant) != Some(output) {
        return 14;
    }
    0
}
