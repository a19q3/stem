//! Native object approval using the quorum at Stem's committed CKB anchor.

#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_lock_hash, load_script, load_script_hash, load_tx_hash},
};
use stem_bridge_wire::metatype::rotating_approval::{Approval, Authority, MAX_PROOF_BYTES};
use stem_contracts::{metatype::verify_workflow_signature, native_type};

entry!(main);
default_alloc!();

fn load<const NUMBER: u64>(buffer: &mut [u8]) -> Option<usize> {
    let mut length = buffer.len() as u64;
    let mut status = buffer.as_mut_ptr() as u64;
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut length, in("a2") 0u64,
            in("a3") 0u64, in("a4") 0u64, in("a5") 0u64, in("a7") NUMBER);
    }
    (status == 0 && length <= buffer.len() as u64).then_some(length as usize)
}

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let args = script.args().raw_data();
    let approval = Approval::decode(&args)?;
    let check = |source| match load_cell_lock_hash(0, source) {
        Ok(_)
            if native_type::contains(source, 0, approval.object) == Some(true)
                && load_cell_lock_hash(1, source) == Err(SysError::IndexOutOfBound) =>
        {
            Some(())
        }
        Err(SysError::IndexOutOfBound) if source == Source::GroupInput => Some(()),
        _ => None,
    };
    check(Source::GroupInput)?;
    check(Source::GroupOutput)?;
    let (raw, length) = native_type::live_ckb_data_bounded::<{ 32 + Authority::MAX_RECORD_BYTES }>(
        approval.registry,
    )?;
    let authority = Authority::decode(&raw[32..length])?;
    if authority.frozen {
        return None;
    }
    let (mut network, mut proof) = ([0; 32], [0; MAX_PROOF_BYTES]);
    if load::<2116>(&mut network)? != network.len() {
        return None;
    }
    let length = load::<2117>(&mut proof)?;
    let digest = authority.digest(
        &network,
        None,
        &load_script_hash().ok()?,
        &load_tx_hash().ok()?,
    );
    authority
        .authorize(&proof[..length], |key, signature| {
            verify_workflow_signature(key, &digest, signature)
        })
        .then_some(())
}

fn main() -> i8 {
    if verify().is_some() { 0 } else { 62 }
}
