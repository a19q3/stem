#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
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
    let script = load_script()?;
    let args = script.args().raw_data();
    let expected: [u8; 32] = match args.as_ref().try_into() {
        Ok(hash) => hash,
        Err(_) => return Ok(false),
    };
    // Every Cell in this lock group is native CKB. Releasing the deposit also
    // spends the unique vault, which executes its multisig lock and vault type.
    for index in 0.. {
        match load_cell_type_hash(index, Source::GroupInput) {
            Ok(None) => {}
            Ok(Some(_)) => return Ok(false),
            Err(SysError::IndexOutOfBound) => break,
            Err(error) => return Err(error),
        }
    }
    for index in 0.. {
        match load_cell_type_hash(index, Source::Input) {
            Ok(Some(hash)) if hash == expected => return Ok(true),
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => return Ok(false),
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}
