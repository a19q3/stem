//! Charge a royalty on a native object's Lock change.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_lock_hash, load_script, load_script_hash},
};
use stem_bridge_wire::metatype::royalty::{Royalty, payment_tag};
use stem_contracts::{native_type, royalty::paid};
use stem_stake::Hash32;

entry!(main);
default_alloc!();

fn owner(source: Source, object: &Hash32) -> Option<Option<Hash32>> {
    match load_cell_lock_hash(0, source) {
        Ok(lock)
            if native_type::contains(source, 0, object) == Some(true)
                && load_cell_lock_hash(1, source) == Err(SysError::IndexOutOfBound) =>
        {
            Some(Some(lock))
        }
        Err(SysError::IndexOutOfBound) => Some(None),
        _ => None,
    }
}

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let args = script.args().raw_data();
    let rule = Royalty::decode(&args)?;
    let before = owner(Source::GroupInput, rule.object)?;
    let after = owner(Source::GroupOutput, rule.object)??;
    if before.is_some_and(|before| before != after) {
        let tag = payment_tag(None, &load_script_hash().ok()?);
        paid(
            rule.stem_payee,
            rule.amount,
            &tag,
            native_type::output_is_untyped,
        )?;
    }
    Some(())
}

fn main() -> i8 {
    if verify().is_some() { 0 } else { 60 }
}
