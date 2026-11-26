//! Portable adapter for the Stem immutable object identity profile.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry};
use stem_bridge_wire::{
    metatype::{LogicalScript, MetatypeManifest, custody::Boundary},
    spore::plain_spore,
};
use stem_contracts::metatype::{manifest_data, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation| {
        let group = invocation.group;
        let (args, tail) = group.script.args().as_chunks::<32>();
        let [_, native_authority, _, policy_code] = args else {
            return None;
        };
        if !tail.is_empty()
            || args.contains(&[0; 32])
            || Boundary::for_cardinality(group.input_indices.len(), group.output_indices.len())?
                != invocation.boundary
        {
            return None;
        }
        let policy_args = [group.script.id(), *native_authority];
        let policy = LogicalScript::encode(policy_code, 0, policy_args.as_flattened())?;
        let policy = LogicalScript::decode(&policy)?;
        for (source, indices) in [
            (Source::Input, group.input_indices),
            (Source::Output, group.output_indices),
        ] {
            for index in indices {
                let raw = manifest_data(index, source)?;
                let manifest = MetatypeManifest::decode(&raw)?;
                if manifest.entries().len() != 2
                    || !manifest.state_for(policy)?.is_empty()
                    || !plain_spore(manifest.state_for(group.script)?)
                {
                    return None;
                }
            }
        }
        // For import/return the dispatcher verified the bridge route. Its
        // independent mailbox Lock and Type bind every byte of the transition.
        Some(())
    })
}
