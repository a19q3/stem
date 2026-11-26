//! Enforce one individually approved workflow step per CKB edit.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry};
use stem_bridge_wire::metatype::{
    custody::Boundary,
    workflow::{ACTION, State, Workflow},
};
use stem_contracts::metatype::{verify_workflow_signature, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        let workflow = Workflow::decode(group.script.args())?;
        let id = group.script.id();
        let mut read = |indices: &[u16], source| match indices {
            [] => Some(None),
            [index] => {
                let cell = manifests.manifest_data(*index, source)?;
                let manifest = cell.view()?;
                if !manifest.state_for(group.script)?.is_empty() {
                    return None;
                }
                let entry = manifest
                    .entries()
                    .iter()
                    .find(|entry| &entry.script.id() == workflow.object)?;
                Some(Some(State::decode(entry.state, &id)?))
            }
            _ => None,
        };
        let before = read(group.input_indices.as_slice(), Source::Input)?;
        let after = read(group.output_indices.as_slice(), Source::Output)?;
        let proof = match invocation.witness {
            None => &[][..],
            Some(row)
                if row.action == ACTION
                    && row.message.is_empty()
                    && row.seal.is_empty()
                    && !row.proof.is_empty() =>
            {
                row.proof
            }
            _ => return None,
        };
        match (invocation.boundary, before, after) {
            (Boundary::Transfer, Some(before), Some(after)) => workflow
                .verify_successor(&id, before, after, proof, 1, verify_workflow_signature)
                .then_some(()),
            // The request is made on Stem, bounding the replay work at redemption.
            (Boundary::Import, None, Some(state)) if !state.is_initial() => {
                proof.is_empty().then_some(())
            }
            (Boundary::Return, Some(_), None) => proof.is_empty().then_some(()),
            _ => None,
        }
    })
}
