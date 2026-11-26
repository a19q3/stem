//! Independent native Type Script limiting fungible units per Cell.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source, default_alloc, entry, error::SysError, high_level::load_script,
};
use stem_bridge_wire::metatype::fungible::AmountCap;
use stem_contracts::{metatype::bounded_data, native_type};

entry!(main);
default_alloc!();

fn main() -> i8 {
    let check = || {
        let script = load_script().ok()?;
        let args = script.args().raw_data();
        let cap = AmountCap::decode(&args)?;
        for source in [Source::GroupInput, Source::GroupOutput] {
            for index in 0.. {
                match bounded_data(index, source, 16) {
                    Ok(data)
                        if cap.permits(&data)
                            && native_type::contains(source, index, cap.asset) == Some(true) =>
                    {
                        ()
                    }
                    Err(SysError::IndexOutOfBound) => break,
                    _ => return None,
                }
            }
        }
        Some(())
    };
    if check().is_some() { 0 } else { 55 }
}
