//! Check a fresh, untyped payment output in the current transaction.

use ckb_std::{
    ckb_constants::Source,
    error::SysError,
    high_level::{load_cell_capacity, load_cell_lock_hash},
    syscalls::load_cell_data,
};
use stem_bridge_wire::metatype::MAX_TRANSACTION_CELLS_PER_SIDE;
use stem_stake::Hash32;

pub fn paid(
    payee: &Hash32,
    amount: u64,
    tag: &Hash32,
    untyped: impl Fn(usize) -> Option<bool>,
) -> Option<()> {
    let mut found = false;
    for index in 0..=MAX_TRANSACTION_CELLS_PER_SIDE {
        match load_cell_lock_hash(index, Source::Output) {
            Ok(lock) if index < MAX_TRANSACTION_CELLS_PER_SIDE => {
                if &lock != payee {
                    continue;
                }
            }
            Err(SysError::IndexOutOfBound) => return found.then_some(()),
            _ => return None,
        }
        let mut data = [0; 32];
        match load_cell_data(&mut data, 0, index, Source::Output) {
            Ok(32) if &data == tag => {
                if found
                    || !untyped(index)?
                    || load_cell_capacity(index, Source::Output).ok()? < amount
                {
                    return None;
                }
                found = true;
            }
            Ok(_) | Err(SysError::LengthNotEnough(_)) => (),
            Err(_) => return None,
        }
    }
    None
}
