//! Portable adapter for the native object state limit and its rule source.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry};
use stem_bridge_wire::metatype::{
    custody::Boundary,
    object_policy::{ObjectPolicy, RuleSource, StateLimit},
};
use stem_contracts::metatype::{typed_dependency_data, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        if Boundary::for_cardinality(group.input_indices.len(), group.output_indices.len())?
            != invocation.boundary
        {
            return None;
        }
        let policy = ObjectPolicy::decode(group.script.args())?;
        let raw = match policy.rule {
            RuleSource::DataHash(hash) => {
                manifests.dependency_data(hash, StateLimit::ENCODED_BYTES)?
            }
            RuleSource::LiveCkbType(hash) => {
                typed_dependency_data(hash, StateLimit::ENCODED_BYTES)?
            }
        };
        let limit = StateLimit::decode(&raw)?;
        for (source, indices) in [
            (Source::Input, group.input_indices),
            (Source::Output, group.output_indices),
        ] {
            for index in indices {
                let raw = manifests.manifest_data(index, source)?;
                let manifest = raw.view()?;
                let object = manifest
                    .entries()
                    .iter()
                    .find(|entry| &entry.script.id() == policy.object)?;
                if !manifest.state_for(group.script)?.is_empty() || !limit.permits(object.state) {
                    return None;
                }
            }
        }
        Some(())
    })
}
