//! Type Script example that requires a specific authenticated CKB observation.
#![no_std]
#![no_main]

use alloc::vec;
use core::arch::asm;

use ckb_hash::blake2b_256;
use ckb_std::{default_alloc, entry, high_level::load_script};

entry!(main);
default_alloc!();

fn load_reference(index: u64, buffer: &mut [u8]) -> (u64, u64) {
    let mut size = buffer.len() as u64;
    let mut status = buffer.as_mut_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") status,
            in("a1") &mut size,
            in("a2") 0u64,
            in("a3") index,
            in("a4") 0u64,
            in("a5") 0u64,
            in("a7") 2130u64,
        );
    }
    (status, size)
}

fn main() -> i8 {
    let Ok(script) = load_script() else { return 10 };
    let args = script.args().raw_data();
    if args.len() != 33 || args[0] > 1 {
        return 11;
    }
    for index in 0..4 {
        let (status, size) = load_reference(index, &mut []);
        if status != 0 {
            break;
        }
        if !(1..=21_000).contains(&size) {
            return 12;
        }
        let mut record = vec![0; size as usize];
        if load_reference(index, &mut record) != (0, size) {
            return 12;
        }
        if args[0] == record[0] && blake2b_256(&record) == args[1..] {
            return 0;
        }
    }
    13
}
