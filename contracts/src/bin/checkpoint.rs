#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data, load_cell_lock, load_cell_type_hash,
        load_input_out_point, load_script, load_script_hash, load_witness_args,
    },
    type_id,
};
use stem_stake::{
    Hash32,
    checkpoint::{Checkpoint, Domain, Update, ZERO_OUT_POINT},
};

entry!(main);
default_alloc!();

#[repr(i8)]
enum Error {
    Syscall = 40,
    Identity = 41,
    Encoding = 42,
    History = 43,
    Quorum = 44,
}

impl From<SysError> for Error {
    fn from(_: SysError) -> Self {
        Self::Syscall
    }
}

impl From<stem_stake::Error> for Error {
    fn from(error: stem_stake::Error) -> Self {
        match error {
            stem_stake::Error::Anchor | stem_stake::Error::Sequence => Self::History,
            stem_stake::Error::Authority
            | stem_stake::Error::Quorum
            | stem_stake::Error::Signature => Self::Quorum,
            _ => Self::Encoding,
        }
    }
}

fn main() -> i8 {
    match verify() {
        Ok(()) => 0,
        Err(error) => error as i8,
    }
}

fn verify() -> Result<(), Error> {
    let script = load_script()?;
    if u8::from(script.hash_type()) != ScriptHashType::Data2 as u8 {
        return Err(Error::Identity);
    }
    let args = script.args().raw_data();
    match args.len() {
        33 if args[32] == 0 => verify_lock(args[..32].try_into().unwrap()),
        128 => verify_type(script, &args),
        _ => Err(Error::Encoding),
    }
}

fn verify_lock(checkpoint_type: Hash32) -> Result<(), Error> {
    for index in 0.. {
        match load_cell_type_hash(index, Source::GroupInput) {
            Ok(Some(actual)) if actual == checkpoint_type => {}
            Err(SysError::IndexOutOfBound) if index > 0 => return Ok(()),
            _ => return Err(Error::Identity),
        }
    }
    unreachable!()
}

fn verify_type(script: Script, args: &[u8]) -> Result<(), Error> {
    type_id::check_type_id(0, 32)?;
    let domain = Domain {
        network: args[32..64].try_into().unwrap(),
        ckb_genesis: args[64..96].try_into().unwrap(),
        checkpoint_type: load_script_hash()?,
    };
    let initial_generation = args[96..128].try_into().unwrap();
    if domain.network == [0; 32] || domain.ckb_genesis == [0; 32] {
        return Err(Error::Identity);
    }
    let mut lock_args = [0; 33];
    lock_args[..32].copy_from_slice(&domain.checkpoint_type);
    let lock = script
        .as_builder()
        .args(lock_args.as_slice().pack())
        .build();
    if load_cell_lock(0, Source::GroupOutput)? != lock {
        return Err(Error::Identity);
    }
    let next = Checkpoint::decode(domain.network, &load_cell_data(0, Source::GroupOutput)?)?;
    let (old, predecessor, source) = match load_input_out_point(0, Source::GroupInput) {
        Ok(point) => {
            if load_cell_lock(0, Source::GroupInput)? != lock
                || load_cell_capacity(0, Source::GroupOutput)?
                    < load_cell_capacity(0, Source::GroupInput)?
            {
                return Err(Error::Identity);
            }
            let old = load_cell_data(0, Source::GroupInput)?;
            (
                Some(Checkpoint::decode(domain.network, &old)?),
                point.as_slice().try_into().unwrap(),
                Source::GroupInput,
            )
        }
        Err(SysError::IndexOutOfBound) => (None, ZERO_OUT_POINT, Source::Input),
        Err(error) => return Err(error.into()),
    };
    let witness = load_witness_args(0, source)?;
    let proof = witness.input_type().to_opt().ok_or(Error::Encoding)?;
    let update = Update::decode(&proof.raw_data())?;
    Ok(update.verify(
        &domain,
        initial_generation,
        old.as_ref(),
        &next,
        &predecessor,
    )?)
}
