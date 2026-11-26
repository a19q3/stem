//! Native object constraint backed by a resolved or authenticated rule record.

#![no_std]
#![no_main]

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

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let args = script.args().raw_data();
    let policy = ObjectPolicy::decode(&args)?;
    let limit = match policy.rule {
        RuleSource::DataHash(hash) => StateLimit::decode(
            &ManifestResolver::default().dependency_data(hash, StateLimit::ENCODED_BYTES)?,
        )?,
        RuleSource::LiveCkbType(hash) => StateLimit::decode(
            &native_type::live_ckb_data::<{ 32 + StateLimit::ENCODED_BYTES }>(hash)?[32..],
        )?,
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
