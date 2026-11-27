//! Lock a CKB fee fund to verified checkpoint transaction fees.
#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data, load_cell_lock_hash, load_cell_type, load_script,
        load_script_hash,
    },
};
use stem_stake::settlement::ckb::{FundingPolicy, Tip};

entry!(main);
default_alloc!();

#[repr(i8)]
enum Error {
    Syscall = 90,
    Encoding = 91,
    Shape = 92,
    Expense = 93,
}

impl From<SysError> for Error {
    fn from(_: SysError) -> Self {
        Self::Syscall
    }
}

fn main() -> i8 {
    verify().map_or_else(|error| error as i8, |_| 0)
}

fn verify() -> Result<(), Error> {
    let script = load_script()?;
    if u8::from(script.hash_type()) != ScriptHashType::Data2 as u8 {
        return Err(Error::Encoding);
    }
    let policy = FundingPolicy::decode(&script.args().raw_data()).map_err(|_| Error::Encoding)?;
    let fund = load_script_hash()?;
    let mut principal = 0u64;
    let mut inputs = 0usize;
    for index in 0.. {
        let capacity = match load_cell_capacity(index, Source::GroupInput) {
            Ok(capacity) => capacity,
            Err(SysError::IndexOutOfBound) => break,
            Err(error) => return Err(error.into()),
        };
        check_plain(index, Source::GroupInput)?;
        inputs += 1;
        principal = principal.checked_add(capacity).ok_or(Error::Expense)?;
    }
    let mut returned = 0u64;
    let mut outputs = 0usize;
    for index in 0.. {
        let lock = match load_cell_lock_hash(index, Source::Output) {
            Ok(lock) => lock,
            Err(SysError::IndexOutOfBound) => break,
            Err(error) => return Err(error.into()),
        };
        if lock == fund {
            check_plain(index, Source::Output)?;
            outputs += 1;
            returned = returned
                .checked_add(load_cell_capacity(index, Source::Output)?)
                .ok_or(Error::Expense)?;
        }
    }
    // Combine separately funded payouts without debiting their principal.
    if returned >= principal {
        return if inputs >= 2 && outputs == 1 {
            Ok(())
        } else {
            Err(Error::Shape)
        };
    }

    if load_cell_lock_hash(1, Source::Input)? != fund
        || load_cell_lock_hash(1, Source::Output)? != fund
        || load_cell_lock_hash(0, Source::Input)? == fund
        || load_cell_lock_hash(0, Source::Output)? == fund
        || load_cell_capacity(2, Source::Input) != Err(SysError::IndexOutOfBound)
        || load_cell_capacity(2, Source::Output) != Err(SysError::IndexOutOfBound)
        || load_cell_capacity(0, Source::Input)? != load_cell_capacity(0, Source::Output)?
    {
        return Err(Error::Shape);
    }
    let input = load_cell_type(0, Source::Input)?.ok_or(Error::Shape)?;
    if load_cell_type(0, Source::Output)?.as_ref() != Some(&input)
        || u8::from(input.hash_type()) != ScriptHashType::Data2 as u8
    {
        return Err(Error::Shape);
    }
    policy
        .verify_checkpoint_type(
            &input
                .code_hash()
                .as_slice()
                .try_into()
                .map_err(|_| Error::Encoding)?,
            &input.args().raw_data(),
        )
        .map_err(|_| Error::Shape)?;

    let previous = Tip::decode(&load_cell_data(0, Source::Input)?).map_err(|_| Error::Encoding)?;
    let next = Tip::decode(&load_cell_data(0, Source::Output)?).map_err(|_| Error::Encoding)?;
    policy
        .verify_charge(&previous, &next, principal - returned)
        .map_err(|_| Error::Expense)
}

fn check_plain(index: usize, source: Source) -> Result<(), Error> {
    if load_cell_type(index, source)?.is_some() || !load_cell_data(index, source)?.is_empty() {
        return Err(Error::Shape);
    }
    Ok(())
}
