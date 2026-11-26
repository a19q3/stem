//! Metatype adapter for the native append-only revision constraint.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry};
use stem_bridge_wire::{
    intercell::Identity,
    metatype::{LogicalScript, custody::Boundary},
    object::{revision, valid_append_only_transition},
};
use stem_contracts::metatype::{CellData, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        let object: &Identity = group.script.args().try_into().ok()?;
        if *object == [0; 32] {
            return None;
        }
        let mut read = |indices: &[u16], source| match indices {
            [index] => Some(Some(manifests.manifest_data(*index, source)?)),
            [] => Some(None),
            _ => None,
        };
        let before = read(group.input_indices.as_slice(), Source::Input)?;
        let after = read(group.output_indices.as_slice(), Source::Output)?;
        let state = |cell| group_state(cell, group.script, object);
        match (invocation.boundary, before.as_ref(), after.as_ref()) {
            (Boundary::Standalone | Boundary::Transfer, Some(before), Some(after)) => {
                valid_append_only_transition(state(before)?, state(after)?).then_some(())
            }
            (Boundary::Standalone, None, Some(after)) => {
                (revision(state(after)?) == Some(0)).then_some(())
            }
            (Boundary::Import, None, Some(after)) => revision(state(after)?).map(|_| ()),
            (Boundary::Return, Some(before), None) => revision(state(before)?).map(|_| ()),
            _ => None,
        }
    })
}

fn group_state<'a>(
    cell: &'a CellData,
    script: LogicalScript<'_>,
    object: &Identity,
) -> Option<&'a [u8]> {
    let manifest = cell.view()?;
    if !manifest.state_for(script)?.is_empty() {
        return None;
    }
    manifest
        .entries()
        .iter()
        .find(|entry| &entry.script.id() == object)
        .map(|entry| entry.state)
}
