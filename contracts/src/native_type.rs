//! Native Stem Type Script membership syscall.

use ckb_std::ckb_constants::Source;
use core::arch::asm;
use stem_stake::Hash32;

pub fn contains(source: Source, index: usize, hash: &Hash32) -> Option<bool> {
    let mut size = 0u64;
    let mut status = 0u64;
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut size, in("a2") 0u64,
             in("a3") index as u64, in("a4") source as u64,
             in("a5") hash.as_ptr() as u64, in("a7") 2113u64);
    }
    match status {
        0 => Some(true),
        2 => Some(false),
        _ => None,
    }
}
