//! Verify the same object royalty in a CKB virtual Type Group.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    high_level::{load_cell_lock_hash, load_cell_type_hash},
};
use stem_bridge_wire::metatype::{
    custody::{Boundary, Profile},
    royalty::{Royalty, payment_tag},
};
use stem_contracts::{metatype::with_invocation, royalty::paid};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        let rule = Royalty::decode(group.script.args())?;
        let mut owner = |source, indices: &[u16]| match indices {
            [] => Some(None),
            [index] => {
                let raw = manifests.manifest_data(*index, source)?;
                let manifest = raw.view()?;
                (manifest.state_for(group.script)?.is_empty()
                    && manifest
                        .entries()
                        .iter()
                        .any(|entry| entry.script.id() == *rule.object))
                .then_some(())?;
                Some(Some(load_cell_lock_hash(usize::from(*index), source).ok()?))
            }
            _ => None,
        };
        let before = owner(Source::Input, &group.input_indices)?;
        let after = owner(Source::Output, &group.output_indices)?;
        match (invocation.boundary, before, after) {
            (Boundary::Standalone | Boundary::Transfer, Some(before), Some(after)) => {
                if before == after {
                    return Some(());
                }
                let tag = payment_tag(Some(invocation.deployment_type), &group.script.id());
                paid(rule.ckb_payee, rule.amount, &tag, |index| {
                    Some(load_cell_type_hash(index, Source::Output).ok()?.is_none())
                })
            }
            (Boundary::Standalone, None, Some(_)) => Some(()),
            (Boundary::Import, None, Some(_)) | (Boundary::Return, Some(_), None)
                if matches!(
                    invocation.profile,
                    Some(Profile::Immutable | Profile::Revisable)
                ) =>
            {
                Some(())
            }
            _ => None,
        }
    })
}
