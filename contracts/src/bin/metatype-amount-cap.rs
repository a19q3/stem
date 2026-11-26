//! Portable adapter for the native per-Cell fungible principal ceiling.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry};
use stem_bridge_wire::{
    amount,
    metatype::{custody::Profile, fungible::AmountCap},
};
use stem_contracts::metatype::with_invocation;

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        let cap = AmountCap::decode(group.script.args())?;
        for (source, indices) in [
            (Source::Input, group.input_indices),
            (Source::Output, group.output_indices),
        ] {
            for index in indices {
                let raw = manifests.manifest_data(index, source)?;
                let manifest = raw.view()?;
                let asset = manifest
                    .entries()
                    .iter()
                    .find(|entry| &entry.script.id() == cap.asset)?;
                let units = amount(asset.state)?;
                if !manifest.state_for(group.script)?.is_empty()
                    || units > cap.ceiling
                    || (units == 0 && invocation.profile != Some(Profile::MultiAsset))
                {
                    return None;
                }
            }
        }
        Some(())
    })
}
