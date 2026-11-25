//! Owner-controlled Lock with an opt-in, Executor-only path for Lineage heads.
#![no_std]
#![no_main]

use core::arch::asm;

use ckb_hash::blake2b_256;
use ckb_std::{default_alloc, entry, high_level::load_script};

entry!(main);
default_alloc!();

fn load<const N: usize>(number: u64) -> (u64, [u8; N]) {
    let mut bytes = [0; N];
    let mut length = N as u64;
    let mut status = bytes.as_mut_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") status,
            in("a1") &mut length,
            in("a2") 0u64,
            in("a3") 0u64,
            in("a4") 0u64,
            in("a7") number,
        );
    }
    (
        if status == 0 && length != N as u64 {
            u64::MAX
        } else {
            status
        },
        bytes,
    )
}

fn main() -> i8 {
    let Ok(script) = load_script() else { return 10 };
    let args = script.args().raw_data();
    if args.len() != 21 || args[0] > 1 {
        return 11;
    }
    let digest = match load::<33>(2125) {
        (0, context) if context[0] == 1 && args[0] == 1 => return 0,
        (0, context) if context[0] == 0 && args[0] == 0 => {
            <[u8; 32]>::try_from(&context[1..]).unwrap()
        }
        (2, _) => {
            let (status, digest) = load::<32>(2119);
            if status != 0 {
                return 12;
            }
            digest
        }
        _ => return 12,
    };
    let (status, seal) = load::<65>(2118);
    if status != 0 {
        return 13;
    }
    let mut public_key = [0; 33];
    let mut status = seal.as_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") status,
            in("a1") digest.as_ptr(),
            in("a2") public_key.as_mut_ptr(),
            in("a7") 2121u64,
        );
    }
    if status != 0 || blake2b_256(public_key)[..20] != args[1..] {
        return 14;
    }
    0
}
