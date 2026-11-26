//! A portable UDT adapter using Metatype's complete absolute Cell groups.

#![no_std]
#![no_main]

use ckb_std::{ckb_constants::Source, default_alloc, entry};
use stem_contracts::metatype::{authorized, total, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation| {
        let group = invocation.group;
        let (args, rest) = group.script.args().as_chunks::<32>();
        let [issuer, series] = args else { return None };
        if !rest.is_empty() || *issuer == [0; 32] || *series == [0; 32] {
            return None;
        }
        let amount = |manifest: &stem_bridge_wire::metatype::MetatypeManifest<'_>, _| {
            stem_bridge_wire::amount(manifest.state_for(group.script)?)
        };
        let input = total(&group.input_indices, Source::Input, amount)?;
        let output = total(&group.output_indices, Source::Output, amount)?;
        (input == output || (output > input && authorized(issuer))).then_some(())
    })
}
