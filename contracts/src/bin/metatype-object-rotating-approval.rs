//! Portable object approval using the actual CKB registry dependency.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::prelude::Entity,
    default_alloc, entry,
    high_level::{load_header, load_tx_hash},
};
use stem_bridge_wire::metatype::{
    custody::Boundary,
    rotating_approval::{Approval, Authority, witness_signature},
};
use stem_contracts::metatype::{typed_dependency_data, verify_workflow_signature, with_invocation};

entry!(main);
default_alloc!();

fn main() -> i8 {
    with_invocation(|invocation, manifests| {
        let group = invocation.group;
        let approval = Approval::decode(group.script.args())?;
        let input = group.input_indices.as_slice();
        let output = group.output_indices.as_slice();
        if invocation.boundary == Boundary::Standalone {
            if !matches!((input, output), ([], [_]) | ([_], [_])) {
                return None;
            }
        } else if Boundary::for_cardinality(input.len(), output.len())? != invocation.boundary {
            return None;
        }
        for (indices, source) in [(input, Source::Input), (output, Source::Output)] {
            for &index in indices {
                let data = manifests.manifest_data(index, source)?;
                let manifest = data.view()?;
                if !manifest.state_for(group.script)?.is_empty()
                    || !manifest
                        .entries()
                        .iter()
                        .any(|entry| entry.script.id() == *approval.object)
                {
                    return None;
                }
            }
        }
        let authority =
            Authority::decode(&typed_dependency_data(approval.registry, Authority::SIZE)?)?;
        let network = load_header(0, Source::HeaderDep).ok()?.calc_header_hash();
        let digest = authority.digest(
            network.as_slice().try_into().ok()?,
            Some(invocation.deployment_type),
            &group.script.id(),
            &load_tx_hash().ok()?,
        );
        verify_workflow_signature(
            &authority.public_key,
            &digest,
            witness_signature(invocation.witness?)?,
        )
        .then_some(())
    })
}
