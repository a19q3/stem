//! Experimental append-only registry validated by native Stem execution.
#![no_std]
#![no_main]

use alloc::vec::Vec;
use ckb_hash::new_blake2b;
use ckb_std::{
    ckb_constants::Source,
    ckb_types::prelude::Entity,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_capacity, load_cell_lock_hash, load_input_out_point, load_script},
};
use ed25519_dalek::{Signature, VerifyingKey};
use stem_contracts::metatype::bounded_data;
use stem_stake::{
    Hash32, Member,
    jolteon::{Committee, MAX_MEMBERS},
};

entry!(main);
default_alloc!();

const REGISTRY_BYTES: usize = 32 * 1024;
const MAX_CHAINS: usize = 8;

fn main() -> i8 {
    if validate().is_some() { 0 } else { 121 }
}

fn native_network() -> Option<Hash32> {
    const LOAD_STEM_NETWORK: u64 = 2116;
    let mut network = [0; 32];
    let mut length = network.len() as u64;
    let mut status = network.as_mut_ptr() as u64;
    unsafe {
        core::arch::asm!("ecall", inout("a0") status, in("a1") &mut length,
            in("a2") 0u64, in("a7") LOAD_STEM_NETWORK);
    }
    (status == 0 && length == 32).then_some(network)
}

fn record(input: &[u8]) -> Option<(&[u8], &[u8])> {
    let (length, rest) = input.split_first_chunk::<2>()?;
    rest.split_at_checked(usize::from(u16::from_le_bytes(*length)))
}

fn validate() -> Option<()> {
    let args = load_script().ok()?.args().raw_data();
    let (domain, seed) = args.split_at_checked(96)?;
    let ([primary, _genesis, registrar], []) = domain.as_chunks::<32>() else { return None };
    if seed.len() != 36
        || native_network()? != *primary
        || load_cell_capacity(1, Source::GroupOutput) != Err(SysError::IndexOutOfBound)
    {
        return None;
    }
    let output = bounded_data(0, Source::GroupOutput, REGISTRY_BYTES).ok()?;
    let old = match load_cell_capacity(0, Source::GroupInput) {
        Err(SysError::IndexOutOfBound) => {
            if load_input_out_point(0, Source::Input).ok()?.as_slice() != seed {
                return None;
            }
            Vec::new()
        }
        Ok(_) => {
            if load_cell_capacity(1, Source::GroupInput) != Err(SysError::IndexOutOfBound)
                || load_cell_lock_hash(0, Source::GroupInput).ok()? != load_cell_lock_hash(0, Source::GroupOutput).ok()?
            {
                return None;
            }
            bounded_data(0, Source::GroupInput, REGISTRY_BYTES).ok()?
        }
        Err(_) => return None,
    };
    let (new, tail) = record(output.strip_prefix(old.as_slice())?)?;
    let (statement, rest) = new.split_at_checked(160)?;
    let ([network, deployment, genesis, manager, checkpoint], []) = statement.as_chunks::<32>() else {
        return None;
    };
    if !tail.is_empty() || network == primary || *deployment == [0; 32] {
        return None;
    }
    let mut old = old.as_slice();
    let mut count = 0;
    while !old.is_empty() {
        let (prior, rest) = record(old)?;
        if prior.get(..32)? == network || count + 1 >= MAX_CHAINS {
            return None;
        }
        count += 1;
        old = rest;
    }
    let (member_count, rest) = rest.split_first()?;
    let member_count = usize::from(*member_count);
    if !(1..=MAX_MEMBERS).contains(&member_count) {
        return None;
    }
    let (roster, rest) = rest.split_at_checked(member_count * 40)?;
    let (registrar_signature, rest) = rest.split_first_chunk::<64>()?;
    let (manager_signature, proof) = rest.split_first_chunk::<64>()?;
    let mut digest = new_blake2b();
    digest.update(b"Stem Primary native registration v1\0");
    digest.update(&domain[..64]);
    digest.update(statement);
    digest.update(roster);
    let mut hash = [0; 32];
    digest.finalize(&mut hash);
    VerifyingKey::from_bytes(registrar).ok()?.verify_strict(&hash, &Signature::from_bytes(registrar_signature)).ok()?;
    VerifyingKey::from_bytes(manager).ok()?.verify_strict(&hash, &Signature::from_bytes(manager_signature)).ok()?;

    let members = roster
        .as_chunks::<40>()
        .0
        .iter()
        .map(|member| Member {
            key: member[..32].try_into().expect("fixed member key"),
            power: u64::from_le_bytes(member[32..].try_into().expect("fixed member power")),
        })
        .collect::<Vec<_>>();
    let finalized = Committee::new(*network, *genesis, 0, &members).ok()?.verify_finality(proof).ok()?;
    (finalized.height() == 1 && finalized.id() == *checkpoint && finalized.as_bytes()[16..48] == *genesis).then_some(())
}
