//! Separate transfer-approval Type Group for a native object.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_lock_hash, load_script},
};
use core::arch::asm;

entry!(main);
default_alloc!();

fn main() -> i8 {
    let Ok(script) = load_script() else {
        return 114;
    };
    let args = script.args().raw_data();
    let (fields, remaining) = args.as_chunks::<32>();
    let [object, authority] = fields else {
        return 114;
    };
    if !remaining.is_empty() || *object == [0; 32] || *authority == [0; 32] {
        return 114;
    }
    // An input-side policy must stay tied to its original object, even
    // if a transaction tries to move it to an unrelated Cell.
    for source in [Source::GroupInput, Source::GroupOutput] {
        if load_cell_lock_hash(1, source) != Err(SysError::IndexOutOfBound) {
            return 114;
        }
        let mut len = 0u64;
        let mut status = 0u64;
        unsafe {
            asm!("ecall", inout("a0") status, in("a1") &mut len, in("a2") 0u64,
                 in("a3") 0u64, in("a4") source as u64,
                 in("a5") object.as_ptr() as u64, in("a7") 2113u64);
        }
        if status != 0 && status != 1 {
            return 114;
        }
    }
    for index in 0.. {
        match load_cell_lock_hash(index, Source::Input) {
            Ok(hash) if &hash == authority => return 0,
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => return 115,
            Err(_) => return 114,
        }
    }
    unreachable!()
}
