//! Portable adapter preserving the original object's paired approval authority.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry};
use stem_bridge_wire::metatype::custody::Boundary;
use stem_contracts::metatype::{authorized, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        let (args, tail) = group.script.args().as_chunks::<32>();
        let [object, native_authority] = args else {
            return None;
        };
        if !tail.is_empty()
            || args.contains(&[0; 32])
            || Boundary::for_cardinality(group.input_indices.len(), group.output_indices.len())?
                != invocation.boundary
        {
            return None;
        }
        let mut authority = None;
        for (source, indices) in [
            (Source::Input, group.input_indices),
            (Source::Output, group.output_indices),
        ] {
            for index in indices {
                let raw = manifests.manifest_data(index, source)?;
                let manifest = raw.view()?;
                let identity = manifest
                    .entries()
                    .iter()
                    .find(|entry| &entry.script.id() == object)?;
                let (fields, rest) = identity.script.args().as_chunks::<32>();
                let [_, native, ckb, policy_code] = fields else {
                    return None;
                };
                if !rest.is_empty()
                    || fields.contains(&[0; 32])
                    || native != native_authority
                    || policy_code != group.script.code().0
                    || !manifest.state_for(group.script)?.is_empty()
                    || authority.is_some_and(|seen| seen != *ckb)
                {
                    return None;
                }
                authority = Some(*ckb);
            }
        }
        authorized(&authority?).then_some(())
    })
}
