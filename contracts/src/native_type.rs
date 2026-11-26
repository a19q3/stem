//! Native Stem Type Script membership syscall.

use ckb_std::{ckb_constants::Source, error::SysError};
use core::arch::asm;
use stem_bridge_wire::metatype::{
    custody::positive_amount,
    fungible::{Amounts, MAX_DATA},
};
use stem_stake::Hash32;

use crate::metatype::bounded_data;

/// Compare a registered Spore handoff with the actual native input and complete output Cell.
pub fn matches_handoff(
    grant: &stem_bridge_wire::spore::Handoff<'_>,
    input: usize,
    output: usize,
) -> bool {
    use ckb_std::{
        ckb_constants::SYS_LOAD_CELL_BY_FIELD, ckb_types::prelude::Entity,
        high_level::load_input_out_point,
    };
    let Ok(source) = load_input_out_point(input, Source::Input) else {
        return false;
    };
    let mut digest = [0u8; 32];
    let mut size = 32u64;
    let mut status = digest.as_mut_ptr() as u64;
    // Native CellContentHash includes capacity, Lock, Type set and data.
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut size, in("a2") 0u64,
             in("a3") output as u64, in("a4") Source::Output as u64,
             in("a5") 7u64, in("a7") SYS_LOAD_CELL_BY_FIELD);
    }
    source.as_slice() == grant.source && (status, size) == (0, 32) && &digest == grant.output_hash
}

pub fn amount(source: Source, index: usize, asset: &Hash32) -> Result<u128, SysError> {
    let data = bounded_data(index, source, MAX_DATA)?;
    if let Some(amount) = positive_amount(&data) {
        return Ok(amount);
    }
    let amounts = Amounts::decode(&data).ok_or(SysError::Encoding)?;
    if !amounts
        .ids()
        .all(|id| contains(source, index, id) == Some(true))
    {
        return Err(SysError::Encoding);
    }
    amounts.amount(asset).ok_or(SysError::Encoding)
}

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

pub fn output_is_untyped(index: usize) -> Option<bool> {
    let mut size = 0u64;
    let mut status = 0u64;
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut size, in("a2") 0u64,
             in("a3") index as u64, in("a4") Source::Output as u64,
             in("a5") 0u64, in("a7") 2111u64);
    }
    // The caller resolved this output before asking for its first Type Script.
    match status {
        0 => Some(false),
        1 => Some(true),
        _ => None,
    }
}

/// Return the exact type-hash-prefixed native projection of one L1 record.
pub fn live_ckb_data<const N: usize>(hash: &Hash32) -> Option<[u8; N]> {
    let (data, length) = live_ckb_data_bounded(hash)?;
    (length == N).then_some(data)
}

/// Return one live record and its full length within the supplied buffer bound.
pub fn live_ckb_data_bounded<const N: usize>(hash: &Hash32) -> Option<([u8; N], usize)> {
    let mut found = None;
    for index in 0u64..4 {
        let mut data = [0; N];
        let mut len = N as u64;
        let mut status = data.as_mut_ptr() as u64;
        unsafe {
            asm!("ecall", inout("a0") status, in("a1") &mut len, in("a2") 0u64,
                in("a3") index, in("a4") 0u64, in("a5") 1u64, in("a7") 2130u64);
        }
        match status {
            1 => break,
            2 => continue,
            0 if data.get(..32)? != hash => continue,
            0 if (32..=N as u64).contains(&len) && found.is_none() => {
                found = Some((data, len as usize));
            }
            _ => return None,
        }
    }
    found
}
