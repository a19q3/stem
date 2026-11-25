//! Example Executor that atomically increments all selected counters.
#![no_std]
#![no_main]

use ckb_std::{default_alloc, entry};
use core::arch::asm;

entry!(main);
default_alloc!();

fn load<const N: usize>(number: u64, source: u64, index: u64) -> Option<[u8; N]> {
    let mut bytes = [0; N];
    let mut length = N as u64;
    let mut status = bytes.as_mut_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") status,
            in("a1") &mut length,
            in("a2") 0u64,
            in("a3") index,
            in("a4") source,
            in("a7") number,
        );
    }
    (status == 0 && length == N as u64).then_some(bytes)
}

fn main() -> i8 {
    let Some(context) = load::<52>(2126, 0, 0) else {
        return 10;
    };
    let Some(increment) = load::<8>(2127, 0, 0).map(u64::from_le_bytes) else {
        return 11;
    };
    if increment == 0 {
        return 12;
    }
    let count = u32::from_le_bytes(context[48..].try_into().unwrap());
    for index in 0..u64::from(count) {
        let Some(input) = load::<48>(2126, 1, index) else {
            return 13;
        };
        let Some(value) = load::<16>(2127, 1, index).map(u128::from_le_bytes) else {
            return 14;
        };
        let Some(value) = value.checked_add(u128::from(increment)) else {
            return 15;
        };
        let capacity = u64::from_le_bytes(input[40..].try_into().unwrap());
        // Keep the guest buffer alive until the host has copied it.
        let data = value.to_le_bytes();
        let mut status = data.as_ptr() as u64;
        unsafe {
            asm!(
                "ecall",
                inout("a0") status,
                in("a1") 16u64,
                in("a2") index,
                in("a3") capacity,
                in("a7") 2128u64,
            );
        }
        if status != 0 {
            return 16;
        }
    }
    0
}
