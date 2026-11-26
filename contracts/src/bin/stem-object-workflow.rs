//! Native Type Script that verifies each step from the previous workflow state.

#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, prelude::Entity},
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_capacity, load_cell_lock, load_script, load_script_hash},
};
use stem_bridge_wire::metatype::{
    MAX_STATE_BYTES,
    workflow::{MAX_PROOF, STEPS, State, Workflow},
};
use stem_contracts::{
    metatype::{bounded_data, verify_workflow_signature, workflow_key},
    native_type,
};

entry!(main);
default_alloc!();

fn main() -> i8 {
    if verify().is_some() { 0 } else { 59 }
}

fn load<const NUMBER: u64>(buffer: &mut [u8]) -> (u64, u64) {
    let mut length = buffer.len() as u64;
    let mut status = buffer.as_mut_ptr() as u64;
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut length, in("a2") 0u64,
            in("a3") 0u64, in("a4") 0u64, in("a5") 0u64, in("a7") NUMBER);
    }
    (status, length)
}

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let args = script.args().raw_data();
    let workflow = Workflow::decode(&args)?;
    let id = load_script_hash().ok()?;
    let mut network = [0u8; 32];
    if load::<2116>(&mut network) != (0, 32) || &network != workflow.network {
        return None;
    }
    let mut proof = [0u8; MAX_PROOF];
    let proof = match load::<2117>(&mut proof) {
        (0, size) => proof.get(..size as usize)?,
        (2, _) => &[],
        _ => return None,
    };
    let read = |source| match bounded_data(0, source, MAX_STATE_BYTES) {
        Ok(data)
            if load_cell_capacity(1, source) == Err(SysError::IndexOutOfBound)
                && native_type::contains(source, 0, workflow.object) == Some(true) =>
        {
            Some(Some(State::decode(&data, &id)?))
        }
        Err(SysError::IndexOutOfBound) => Some(None),
        _ => None,
    };
    let after = read(Source::GroupOutput)??;
    if after.is_initial() {
        let lock = load_cell_lock(0, Source::GroupOutput).ok()?;
        if lock.code_hash().as_slice() == workflow.escrow_code
            && u8::from(lock.hash_type()) == ScriptHashType::Data as u8
        {
            return None;
        }
    }
    match read(Source::GroupInput)? {
        Some(before) => workflow
            .verify_successor(&id, before, after, proof, STEPS, verify_workflow_signature)
            .then_some(()),
        None => (after.is_initial()
            && proof.is_empty()
            && workflow
                .authorities
                .iter()
                .all(|key| workflow_key(key).is_some()))
        .then_some(()),
    }
}
