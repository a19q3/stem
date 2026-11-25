#![no_std]
#![no_main]

use core::arch::asm;

use ckb_hash::blake2b_256;
use ckb_std::{default_alloc, entry, high_level::load_script};

entry!(main);
default_alloc!();

const LOAD_GROUP_SEAL: u64 = 2118;
const LOAD_GROUP_SIGNING_DIGEST: u64 = 2119;
const SECP256K1_RECOVER: u64 = 2121;

fn load_exact<const N: usize>(number: u64) -> Option<[u8; N]> {
    let mut data = [0; N];
    let mut length = N as u64;
    let mut result = data.as_mut_ptr() as u64;
    // Stem buffer ABI, with a4=0 selecting the current Script Group.
    unsafe {
        asm!(
            "ecall",
            inout("a0") result,
            in("a1") &mut length,
            in("a2") 0u64,
            in("a3") 0u64,
            in("a4") 0u64,
            in("a5") 0u64,
            in("a7") number,
        );
    }
    (result == 0 && length == N as u64).then_some(data)
}

fn main() -> i8 {
    let Ok(script) = load_script() else { return 10 };
    let args = script.args().raw_data();
    if args.len() != 20 {
        return 11;
    }
    let Some(seal) = load_exact::<65>(LOAD_GROUP_SEAL) else {
        return 12;
    };
    let Some(digest) = load_exact::<32>(LOAD_GROUP_SIGNING_DIGEST) else {
        return 13;
    };
    let mut key = [0u8; 33];
    let mut result = seal.as_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") result,
            in("a1") digest.as_ptr(),
            in("a2") key.as_mut_ptr(),
            in("a7") SECP256K1_RECOVER,
        );
    }
    if result != 0 {
        return 17;
    }
    if blake2b_256(&key)[..20] != args[..] {
        return 18;
    }
    0
}
