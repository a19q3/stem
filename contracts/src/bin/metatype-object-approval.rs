//! A detached approval independent of both CKB ownership and object identity.

#![no_std]
#![no_main]

use alloc::vec::Vec;
use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    high_level::{load_cell_lock_hash, load_tx_hash},
};
use ed25519_dalek::{Signature, VerifyingKey};
use stem_bridge_wire::metatype::{LogicalScript, MetatypeManifest};
use stem_contracts::metatype::{manifest_data, with_invocation};

entry!(main);
default_alloc!();

fn read(
    source: Source,
    index: u16,
    approval: LogicalScript<'_>,
    object: &[u8; 32],
) -> Option<Vec<u8>> {
    let bytes = manifest_data(index, source)?;
    let manifest = MetatypeManifest::decode(&bytes)?;
    if !manifest.state_for(approval)?.is_empty() {
        return None;
    }
    let entry = manifest
        .entries()
        .iter()
        .find(|entry| &entry.script.id() == object)?;
    Some(entry.state.to_vec())
}

fn main() -> i8 {
    with_invocation(|invocation| {
        let group = invocation.group;
        let (args, tail) = group.script.args().as_chunks::<32>();
        let [object, authority] = args else {
            return None;
        };
        if !tail.is_empty() || args.contains(&[0; 32]) {
            return None;
        }
        let producing = match group.input_indices.as_slice() {
            [] => true,
            [index] => {
                read(Source::Input, *index, group.script, object)?;
                false
            }
            _ => return None,
        };
        let [output] = group.output_indices.as_slice() else {
            return None;
        };
        let state = read(Source::Output, *output, group.script, object)?;
        let witness = invocation.witness?;
        let action = if producing { 1 } else { 2 };
        if witness.action != action
            || witness.message != load_cell_lock_hash(usize::from(*output), Source::Output).ok()?
            || witness.proof != ckb_hash::blake2b_256(&state)
        {
            return None;
        }
        let key = VerifyingKey::from_bytes(authority).ok()?;
        let signature = Signature::from_bytes(witness.seal.try_into().ok()?);
        let digest = witness.approval_digest(
            invocation.deployment_type,
            &group.script.id(),
            &load_tx_hash().ok()?,
        );
        key.verify_strict(&digest, &signature).ok()
    })
}
