//! Portable object identity, immutable content, and mandatory approval membership.

#![no_std]
#![no_main]

use alloc::vec::Vec;
use ckb_std::{
    ckb_constants::Source, ckb_types::prelude::*, default_alloc, entry,
    high_level::load_input_out_point,
};
use stem_bridge_wire::{
    metatype::{LogicalScript, MetatypeManifest},
    object::creation_id,
    spore::plain_spore,
};
use stem_contracts::metatype::{manifest_data, with_invocation};

entry!(main);
default_alloc!();

fn read(
    source: Source,
    index: u16,
    object: LogicalScript<'_>,
    approval: LogicalScript<'_>,
) -> Option<Vec<u8>> {
    let bytes = manifest_data(index, source)?;
    let manifest = MetatypeManifest::decode(&bytes)?;
    let state = manifest.state_for(object)?;
    if !plain_spore(state) || manifest.state_for(approval)? != [] {
        return None;
    }
    Some(state.to_vec())
}

fn main() -> i8 {
    with_invocation(|invocation| {
        let group = invocation.group;
        let (args, tail) = group.script.args().as_chunks::<32>();
        let [object_id, authority, approval_code] = args else {
            return None;
        };
        if !tail.is_empty() || args.contains(&[0; 32]) {
            return None;
        }
        let [output] = group.output_indices.as_slice() else {
            return None;
        };
        let approval_args = [group.script.id(), *authority];
        let approval = LogicalScript::encode(approval_code, 0, approval_args.as_flattened())?;
        let approval = LogicalScript::decode(&approval)?;
        let after = read(Source::Output, *output, group.script, approval)?;
        match group.input_indices.as_slice() {
            [] => {
                let seed = load_input_out_point(0, Source::Input).ok()?;
                (creation_id(
                    invocation.deployment_type,
                    seed.as_slice().try_into().ok()?,
                    u32::from(*output),
                ) == *object_id)
                    .then_some(())
            }
            [input] => {
                (read(Source::Input, *input, group.script, approval)? == after).then_some(())
            }
            _ => None,
        }
    })
}
