//! A portable UDT adapter using Metatype's complete absolute Cell groups.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry};
use stem_bridge_wire::metatype::{
    LogicalScript, MetatypeManifest, custody::Boundary, fungible::Udt,
};
use stem_contracts::metatype::{authorized, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        let udt = Udt::decode(group.script.args(), &group.script.id())?;
        let policy = match udt.policy.as_deref() {
            Some(raw) => Some(LogicalScript::decode(raw)?),
            None => None,
        };
        if invocation.boundary != Boundary::Standalone && policy.is_none() {
            return None;
        }
        let amount = |manifest: &MetatypeManifest<'_>, _| {
            if let Some(policy) = policy {
                if !manifest.state_for(policy)?.is_empty() {
                    return None;
                }
            }
            stem_bridge_wire::amount(manifest.state_for(group.script)?)
        };
        let input = manifests.total(&group.input_indices, Source::Input, amount)?;
        let output = manifests.total(&group.output_indices, Source::Output, amount)?;
        let valid = match invocation.boundary {
            Boundary::Standalone => input == output || (output > input && authorized(udt.issuer)),
            Boundary::Transfer => input == output,
            Boundary::Import => input == 0 && output != 0,
            Boundary::Return => input > output,
        };
        valid.then_some(())
    })
}
