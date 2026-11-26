//! Native object constraint backed by a resolved or authenticated rule record.

#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_capacity, load_script},
};
use stem_bridge_wire::metatype::{
    MAX_STATE_BYTES,
    object_policy::{ObjectPolicy, RuleSource, StateLimit},
};
use stem_contracts::{
    metatype::{ManifestResolver, bounded_data},
    native_type,
};

entry!(main);
default_alloc!();

fn main() -> i8 {
    if verify().is_some() { 0 } else { 56 }
}

fn live_limit(hash: &[u8; 32]) -> Option<StateLimit> {
    let mut found = None;
    for index in 0u64..4 {
        let mut data = [0u8; 32 + StateLimit::ENCODED_BYTES];
        let mut len = data.len() as u64;
        let mut status = data.as_mut_ptr() as u64;
        // Only authenticated, typed, live observations expose this projection.
        unsafe {
            asm!("ecall", inout("a0") status, in("a1") &mut len, in("a2") 0u64,
                in("a3") index, in("a4") 0u64, in("a5") 1u64, in("a7") 2130u64);
        }
        match status {
            1 => break,
            2 => continue,
            0 if &data[..32] != hash => continue,
            0 if len == data.len() as u64 && found.is_none() => {
                found = Some(StateLimit::decode(&data[32..])?);
            }
            _ => return None,
        }
    }
    found
}

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let args = script.args().raw_data();
    let policy = ObjectPolicy::decode(&args)?;
    let limit = match policy.rule {
        RuleSource::DataHash(hash) => StateLimit::decode(
            &ManifestResolver::default().dependency_data(hash, StateLimit::ENCODED_BYTES)?,
        )?,
        RuleSource::LiveCkbType(hash) => live_limit(hash)?,
    };
    for source in [Source::GroupInput, Source::GroupOutput] {
        match bounded_data(0, source, MAX_STATE_BYTES) {
            Ok(data)
                if limit.permits(&data)
                    && native_type::contains(source, 0, policy.object) == Some(true)
                    && load_cell_capacity(1, source) == Err(SysError::IndexOutOfBound) =>
            {
                ()
            }
            Err(SysError::IndexOutOfBound) if source == Source::GroupInput => (),
            _ => return None,
        }
    }
    Some(())
}
