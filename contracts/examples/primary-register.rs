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
    finality::NATIVE_HEADER_BYTES,
    jolteon::{Committee, MAX_MEMBERS},
};

entry!(main);
default_alloc!();

const REGISTRY_BYTES: usize = 32 * 1024;
const MAX_RECORDS: usize = 8;
const REGISTER: u8 = 0;
const HANDOFF: u8 = 1;

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

struct Authority<'a> {
    genesis: Hash32,
    checkpoint: Hash32,
    epoch: u64,
    height: u64,
    anchor: Hash32,
    roster: &'a [[u8; 40]],
}

fn digest(parts: &[&[u8]]) -> Hash32 {
    let mut hasher = new_blake2b();
    for part in parts {
        hasher.update(part);
    }
    let mut hash = [0; 32];
    hasher.finalize(&mut hash);
    hash
}

fn roster(raw: &[u8]) -> Option<(&[[u8; 40]], &[u8])> {
    let (&count, raw) = raw.split_first()?;
    let count = usize::from(count);
    if !(1..=MAX_MEMBERS).contains(&count) {
        return None;
    }
    let (members, tail) = raw.split_at_checked(count * 40)?;
    Some((members.as_chunks::<40>().0, tail))
}

fn members(roster: &[[u8; 40]]) -> Vec<Member> {
    roster
        .iter()
        .map(|member| Member {
            key: member[..32].try_into().expect("fixed member key"),
            power: u64::from_le_bytes(member[32..].try_into().expect("fixed member power")),
        })
        .collect()
}

fn identity(raw: &[u8]) -> Option<(u8, &Hash32, &[u8])> {
    let (&kind, raw) = raw.split_first()?;
    let (network, raw) = raw.split_first_chunk::<32>()?;
    Some((kind, network, raw))
}

fn prior_authority(kind: u8, raw: &[u8]) -> Option<Authority<'_>> {
    match kind {
        REGISTER => {
            let (statement, raw) = raw.split_at_checked(128)?;
            let ([_deployment, genesis, _manager, checkpoint], []) = statement.as_chunks::<32>() else {
                return None;
            };
            Some(Authority {
                genesis: *genesis,
                checkpoint: *checkpoint,
                epoch: 0,
                height: 1,
                anchor: *genesis,
                roster: roster(raw)?.0,
            })
        }
        HANDOFF => successor(raw).map(|(authority, _)| authority),
        _ => None,
    }
}

fn successor(raw: &[u8]) -> Option<(Authority<'_>, &[u8])> {
    let (genesis, raw) = raw.split_first_chunk::<32>()?;
    let (epoch, raw) = raw.split_first_chunk::<8>()?;
    let (height, raw) = raw.split_first_chunk::<8>()?;
    let (anchor, raw) = raw.split_first_chunk::<32>()?;
    let (checkpoint, raw) = raw.split_first_chunk::<32>()?;
    let (roster, tail) = roster(raw)?;
    Some((
        Authority {
            genesis: *genesis,
            checkpoint: *checkpoint,
            epoch: u64::from_le_bytes(*epoch),
            height: u64::from_le_bytes(*height),
            anchor: *anchor,
            roster,
        },
        tail,
    ))
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
    let (kind, network, body) = identity(new)?;
    if !tail.is_empty() || network == primary {
        return None;
    }
    let mut old = old.as_slice();
    let mut count = 0;
    let mut previous = None;
    while !old.is_empty() {
        let (prior, rest) = record(old)?;
        if count + 1 >= MAX_RECORDS {
            return None;
        }
        let (kind, prior_network, body) = identity(prior)?;
        if prior_network == network {
            previous = Some(prior_authority(kind, body)?);
        }
        count += 1;
        old = rest;
    }
    match (kind, previous) {
        (REGISTER, None) => verify_registration(domain, registrar, new.get(1..)?),
        (HANDOFF, Some(previous)) => verify_handoff(network, &previous, body),
        _ => None,
    }
}

fn verify_registration(domain: &[u8], registrar: &Hash32, raw: &[u8]) -> Option<()> {
    let (statement, rest) = raw.split_at_checked(160)?;
    let ([network, deployment, genesis, manager, checkpoint], []) = statement.as_chunks::<32>() else {
        return None;
    };
    if *deployment == [0; 32] {
        return None;
    }
    let (roster, rest) = roster(rest)?;
    let (registrar_signature, rest) = rest.split_first_chunk::<64>()?;
    let (manager_signature, proof) = rest.split_first_chunk::<64>()?;
    let hash = digest(&[b"Stem Primary native registration v1\0", &domain[..64], statement, roster.as_flattened()]);
    VerifyingKey::from_bytes(registrar).ok()?.verify_strict(&hash, &Signature::from_bytes(registrar_signature)).ok()?;
    VerifyingKey::from_bytes(manager).ok()?.verify_strict(&hash, &Signature::from_bytes(manager_signature)).ok()?;
    let finalized = Committee::new(*network, *genesis, 0, &members(roster)).ok()?.verify_finality(proof).ok()?;
    (finalized.height() == 1 && finalized.id() == *checkpoint && finalized.as_bytes()[16..48] == *genesis).then_some(())
}

fn verify_handoff(network: &Hash32, previous: &Authority<'_>, raw: &[u8]) -> Option<()> {
    let (next, evidence) = successor(raw)?;
    if next.genesis != previous.genesis || next.epoch <= previous.epoch || next.height <= previous.height {
        return None;
    }
    let (checkpoint_header, evidence) = evidence.split_first_chunk::<NATIVE_HEADER_BYTES>()?;
    if digest(&[b"Stem Header\0", network, checkpoint_header]) != previous.checkpoint {
        return None;
    }
    let (frontier, evidence) = record(evidence)?;
    let (&suffix_size, evidence) = evidence.split_first()?;
    let (suffix, evidence) = evidence.split_at_checked(usize::from(suffix_size) * 32)?;
    let (handoff, finalized_proof) = record(evidence)?;
    let before = Committee::at_anchor(*network, previous.genesis, previous.epoch, previous.anchor, &members(previous.roster)).ok()?;
    let (change, boundary, _) = before.verify_handoff(handoff).ok()?;
    boundary.verify_extension(checkpoint_header, frontier, suffix.as_chunks::<32>().0).ok()?;
    let roster = members(next.roster);
    if change.epoch != next.epoch || change.members != roster || boundary.id() != next.anchor {
        return None;
    }
    let after = Committee::at_anchor(*network, next.genesis, next.epoch, next.anchor, &roster).ok()?;
    let finalized = after.verify_finality(finalized_proof).ok()?;
    (finalized.height() == change.activation_height
        && finalized.height() == next.height
        && finalized.id() == next.checkpoint
        && finalized.as_bytes()[16..48] == next.anchor)
        .then_some(())
}
