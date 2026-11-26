//! Portable adapter for native immutable and revisable object identity.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry};
use stem_bridge_wire::{
    metatype::{LogicalScript, custody::Boundary},
    object::ObjectArgs,
};
use stem_contracts::metatype::{CellData, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        let object = ObjectArgs::decode(group.script.args())?;
        if Boundary::for_cardinality(group.input_indices.len(), group.output_indices.len())?
            != invocation.boundary
        {
            return None;
        }
        let policy_args = [group.script.id(), *object.native_authority];
        let policy = LogicalScript::encode(object.policy_code, 0, policy_args.as_flattened())?;
        let policy = LogicalScript::decode(&policy)?;
        let mut load = |index, source| manifests.manifest_data(index, source);
        let input = match group.input_indices.as_slice() {
            [index] => Some(load(*index, Source::Input)?),
            _ => None,
        };
        let output = match group.output_indices.as_slice() {
            [index] => Some(load(*index, Source::Output)?),
            _ => None,
        };
        match (&input, &output) {
            (Some(before), Some(after))
                if object.permits(
                    state(before, &object, group.script, policy)?,
                    state(after, &object, group.script, policy)?,
                ) =>
            {
                ()
            }
            (None, Some(boundary)) | (Some(boundary), None) => {
                state(boundary, &object, group.script, policy)?;
            }
            _ => return None,
        }
        // For import/return the dispatcher verified the bridge route. Its
        // independent mailbox Lock and Type bind every byte of the transition.
        Some(())
    })
}

fn state<'a>(
    cell: &'a CellData,
    object: &ObjectArgs<'_>,
    script: LogicalScript<'_>,
    policy: LogicalScript<'_>,
) -> Option<&'a [u8]> {
    let manifest = cell.view()?;
    if !manifest.state_for(policy)?.is_empty() {
        return None;
    }
    let raw = manifest.state_for(script)?;
    object.valid_state(raw).then_some(raw)
}
