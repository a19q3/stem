#![no_std]
#![no_main]

use stem_bridge_wire::{self as wire, Gate};
use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_type_hash, load_script},
};

entry!(main);
default_alloc!();

fn main() -> i8 {
    match verify() {
        Ok(true) => 0,
        Ok(false) => 10,
        Err(_) => 11,
    }
}

fn verify() -> Result<bool, SysError> {
    let args = load_script()?.args().raw_data();
    let Some((vault, role)) = wire::gate(&args) else {
        return Ok(false);
    };
    if let Gate::Deposit { recipient, .. } = role {
        let Ok(recipient) = Script::from_slice(recipient) else {
            return Ok(false);
        };
        if !ScriptHashType::verify_value(u8::from(recipient.hash_type())) {
            return Ok(false);
        }
    }
    for index in 0.. {
        match load_cell_type_hash(index, Source::Input) {
            Ok(Some(hash)) if &hash == vault => return Ok(true),
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}
