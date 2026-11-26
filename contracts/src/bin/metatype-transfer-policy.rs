//! An independent transfer constraint over the actual asset state in each Cell.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry, high_level::load_cell_lock_hash};
use stem_bridge_wire::metatype::MetatypeManifest;
use stem_contracts::metatype::{authorized, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        let (args, rest) = group.script.args().as_chunks::<32>();
        let [asset, authority, allowed_lock, _policy_id] = args else {
            return None;
        };
        if !rest.is_empty() || args.contains(&[0; 32]) {
            return None;
        }
        let amount = |manifest: &MetatypeManifest<'_>| {
            if !manifest.state_for(group.script)?.is_empty() {
                return None;
            }
            let entry = manifest
                .entries()
                .iter()
                .find(|entry| &entry.script.id() == asset)?;
            stem_bridge_wire::amount(entry.state)
        };
        let input = manifests.total(&group.input_indices, Source::Input, |manifest, _| {
            amount(manifest)
        })?;
        let output =
            manifests.total(&group.output_indices, Source::Output, |manifest, index| {
                if &load_cell_lock_hash(usize::from(index), Source::Output).ok()? != allowed_lock {
                    return None;
                }
                amount(manifest)
            })?;
        (input == output || (input == 0 && authorized(authority))).then_some(())
    })
}
