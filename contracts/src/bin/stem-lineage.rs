//! Stable Type Script marker for the network's authenticated Lineage registry.
#![no_main]
#![no_std]

use core::arch::asm;

use ckb_std::{default_alloc, entry, high_level::load_script};

entry!(main);
default_alloc!();

fn main() -> i8 {
    let Ok(script) = load_script() else {
        return 10;
    };
    if script.args().raw_data().len() != 32 || u8::from(script.hash_type()) != 0 {
        return 11;
    }
    let mut group = [0u8; 9];
    let mut length = 9u64;
    let mut result = group.as_mut_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") result, in("a1") &mut length, in("a2") 0u64,
            in("a3") 0u64, in("a4") 0u64, in("a5") 0u64, in("a7") 2114u64,
        );
    }
    if result != 0 || length != 9 || group[0] != 1 {
        return 12;
    }
    let inputs = u32::from_le_bytes(group[1..5].try_into().unwrap());
    let outputs = u32::from_le_bytes(group[5..].try_into().unwrap());
    if inputs > 1 || outputs > 1 || inputs + outputs == 0 {
        return 13;
    }
    0
}
