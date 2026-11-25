//! Permissionless CKB checkpoint driven by consecutive Stem CometBFT commits.
#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data, load_cell_lock, load_cell_type_hash, load_script,
        load_script_hash, load_witness_args,
    },
    type_id,
};
use stem_stake::finality::{self, CheckpointPolicy, Tip};

entry!(main);
default_alloc!();

const LOCK_BYTES: usize = 32;

#[repr(i8)]
enum Error {
    Syscall = 40,
    Identity = 41,
    Shape = 42,
    Finality = 43,
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
        return Err(Error::Identity);
    }
    let args = script.args().raw_data();
    match args.len() {
        LOCK_BYTES => {
            // The Type Script checks the successor and finalized evidence.
            if load_cell_type_hash(0, Source::GroupInput)?
                != Some(args.as_ref().try_into().unwrap())
                || load_cell_type_hash(1, Source::GroupInput) != Err(SysError::IndexOutOfBound)
            {
                return Err(Error::Identity);
            }
            Ok(())
        }
        _ => verify_update(script, &args),
    }
}

fn verify_update(script: Script, args: &[u8]) -> Result<(), Error> {
    type_id::check_type_id(0, 32)?;
    let policy = CheckpointPolicy::decode(args).map_err(|_| Error::Shape)?;
    let lock = script
        .as_builder()
        .args(load_script_hash()?.as_slice().pack())
        .build();
    if load_cell_lock(0, Source::GroupOutput)? != lock {
        return Err(Error::Identity);
    }
    let next = Tip::decode(&load_cell_data(0, Source::GroupOutput)?).map_err(|_| Error::Shape)?;
    let (old, witness_source) = match load_cell_lock(0, Source::GroupInput) {
        Ok(previous_lock) => {
            if previous_lock != lock
                || load_cell_capacity(0, Source::GroupOutput)?
                    < load_cell_capacity(0, Source::GroupInput)?
            {
                return Err(Error::Identity);
            }
            (
                Some(
                    Tip::decode(&load_cell_data(0, Source::GroupInput)?)
                        .map_err(|_| Error::Shape)?,
                ),
                Source::GroupInput,
            )
        }
        Err(SysError::IndexOutOfBound) => (None, Source::Input),
        Err(err) => return Err(err.into()),
    };
    let witness = load_witness_args(0, witness_source)?;
    let proof = witness.input_type().to_opt().ok_or(Error::Shape)?;
    let tip = finality::verify(&policy.trust, old.as_ref(), &proof.raw_data())
        .map_err(|_| Error::Finality)?;
    if tip != next {
        return Err(Error::Finality);
    }
    Ok(())
}
