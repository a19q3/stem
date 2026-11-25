#![no_std]
#![no_main]

use alloc::vec::Vec;

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data, load_cell_lock, load_cell_lock_hash,
        load_cell_occupied_capacity, load_cell_type, load_cell_type_hash, load_input_since,
        load_script, load_script_hash, load_witness_args,
    },
    type_id,
};
use stem_stake::{
    Attestation, DoubleVote, Hash32, MAX_RECORDS, Member, Registry, SignedVote, StakeData,
    l1::{Action, ChangeContext, GuardianProof, Policy, StakeCell, verify_rotation},
};

entry!(main);
default_alloc!();

const ROTATE: u8 = 7;
const FINALIZE_SLASH: u8 = 8;

#[derive(Debug)]
enum Error {
    Syscall = 20,
    Encoding = 21,
    Registry = 22,
    Stake = 23,
    Capacity = 24,
    Signature = 25,
}

impl From<SysError> for Error {
    fn from(_: SysError) -> Self {
        Self::Syscall
    }
}

impl From<stem_stake::Error> for Error {
    fn from(error: stem_stake::Error) -> Self {
        match error {
            stem_stake::Error::Authority
            | stem_stake::Error::Signature
            | stem_stake::Error::Quorum => Self::Signature,
            _ => Self::Stake,
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
        return Err(Error::Encoding);
    }
    let args = script.args().raw_data();
    match args.len() {
        33 if args[32] == 0 => verify_lock(&script, args[..32].try_into().unwrap()),
        64 => verify_stake_type(
            &script,
            args[..32].try_into().unwrap(),
            args[32..].try_into().unwrap(),
        ),
        121..=170 => verify_registry(&script, &args),
        _ => Err(Error::Encoding),
    }
}

fn verify_registry(script: &Script, args: &[u8]) -> Result<(), Error> {
    type_id::check_type_id(0, 32)?;
    let registry_id = load_script_hash()?;
    let policy = Policy::decode(registry_id, args)?;
    let lock = registry_lock(script, &registry_id);
    if load_cell_lock(0, Source::GroupOutput)? != lock
        || load_cell_type_hash(0, Source::GroupOutput)? != Some(registry_id)
    {
        return Err(Error::Registry);
    }
    let new = Registry::decode(&load_cell_data(0, Source::GroupOutput)?)?;
    if new.network_id() != policy.network_id {
        return Err(Error::Registry);
    }
    let Some(input) = find_type(&registry_id, Source::Input)? else {
        return if new.sequence() == 0 && new.records().is_empty() && new.retired_root() == [0; 32] {
            Ok(())
        } else {
            Err(Error::Registry)
        };
    };
    let old = Registry::decode(&load_cell_data(input, Source::Input)?)?;
    if load_cell_lock(input, Source::Input)? != lock
        || load_cell_capacity(0, Source::GroupOutput)? < load_cell_capacity(input, Source::Input)?
    {
        return Err(Error::Registry);
    }
    let witness = witness(input)?;
    let (&operation, payload) = witness.split_first().ok_or(Error::Encoding)?;
    if operation == ROTATE {
        let (old_keys, old_votes, rest) = read_proof(payload)?;
        let (new_keys, new_votes, rest) = read_proof(rest)?;
        if !rest.is_empty() {
            return Err(Error::Encoding);
        }
        return Ok(verify_rotation(
            &policy,
            &old,
            &new,
            &GuardianProof {
                committee: &old_keys,
                votes: &old_votes,
            },
            &GuardianProof {
                committee: &new_keys,
                votes: &new_votes,
            },
        )?);
    }

    let (validator, payload) = payload.split_at_checked(32).ok_or(Error::Encoding)?;
    let validator: Hash32 = validator.try_into().unwrap();
    let mut stake_args = [0u8; 64];
    stake_args[..32].copy_from_slice(&registry_id);
    stake_args[32..].copy_from_slice(&validator);
    let stake_type = with_args(script, &stake_args);
    let stake_input = find_type(
        stake_type.calc_script_hash().as_slice().try_into().unwrap(),
        Source::Input,
    )?;
    let stake_output = find_type(
        stake_type.calc_script_hash().as_slice().try_into().unwrap(),
        Source::Output,
    )?;
    let before = read_stake(&old, &validator, &lock, stake_input, Source::Input)?;
    let after = read_stake(&new, &validator, &lock, stake_output, Source::Output)?;
    let owner = before.or(after).ok_or(Error::Stake)?.1.owner;
    let owner_authorized = match operation {
        0 | 1 | 2 | 5 => owner != lock.calc_script_hash().as_slice() && inputs_authorize(&owner)?,
        _ => false,
    };
    let context = ChangeContext {
        policy: &policy,
        old_registry: &old,
        new_registry: &new,
        validator,
        old: before,
        new: after,
        owner_authorized,
        since: stake_input
            .map(|index| load_input_since(index, Source::Input))
            .transpose()?
            .unwrap_or(0),
        fee: if operation == FINALIZE_SLASH {
            fee()?
        } else {
            0
        },
    };
    match operation {
        0 => {
            let (signature, path) = payload.split_at_checked(64).ok_or(Error::Encoding)?;
            context.verify(Action::Register(signature.try_into().unwrap(), path))?;
        }
        1 if payload.is_empty() => context.verify(Action::Increase)?,
        2 if payload.is_empty() => context.verify(Action::RequestExit)?,
        3 | 4 => {
            let (keys, votes, rest) = read_proof(payload)?;
            if !rest.is_empty() {
                return Err(Error::Encoding);
            }
            let proof = GuardianProof {
                committee: &keys,
                votes: &votes,
            };
            let action = if operation == 3 {
                Action::Activate(proof)
            } else {
                Action::Retire(proof)
            };
            context.verify(action)?;
        }
        5 => context.verify(Action::Withdraw(payload))?,
        6 if payload.len() == 2 * SignedVote::SIZE => {
            context.verify(Action::Freeze(DoubleVote {
                first: SignedVote::decode(&payload[..SignedVote::SIZE])?,
                second: SignedVote::decode(&payload[SignedVote::SIZE..])?,
            }))?
        }
        FINALIZE_SLASH if payload.is_empty() => context.verify(Action::FinalizeSlash)?,
        _ => return Err(Error::Encoding),
    }
    Ok(())
}

fn with_args(script: &Script, args: &[u8]) -> Script {
    script.clone().as_builder().args(args.pack()).build()
}

fn registry_lock(script: &Script, registry_id: &Hash32) -> Script {
    let mut args = [0u8; 33];
    args[..32].copy_from_slice(registry_id);
    with_args(script, &args)
}

fn find_type(hash: &Hash32, source: Source) -> Result<Option<usize>, Error> {
    let mut result = None;
    for index in 0.. {
        match load_cell_type_hash(index, source) {
            Ok(Some(candidate)) if candidate == *hash => {
                if result.replace(index).is_some() {
                    return Err(Error::Stake);
                }
            }
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(result)
}

fn read_stake(
    registry: &Registry,
    key: &Hash32,
    lock: &Script,
    index: Option<usize>,
    source: Source,
) -> Result<Option<(StakeData, StakeCell)>, Error> {
    let Some(index) = index else {
        return Ok(None);
    };
    let records = registry.records();
    let record = records[records
        .binary_search_by_key(key, |record| record.validator_pubkey)
        .map_err(|_| Error::Stake)?];
    if load_cell_lock(index, source)? != *lock {
        return Err(Error::Stake);
    }
    let occupied = load_cell_occupied_capacity(index, source)?;
    if occupied.checked_add(record.bonded_amount) != Some(load_cell_capacity(index, source)?) {
        return Err(Error::Capacity);
    }
    let data = load_cell_data(index, source)?;
    Ok(Some((record, StakeCell::verify_record(&record, &data)?)))
}

fn witness(input: usize) -> Result<Vec<u8>, Error> {
    Ok(load_witness_args(input, Source::Input)?
        .input_type()
        .to_opt()
        .ok_or(Error::Encoding)?
        .raw_data()
        .to_vec())
}

fn same_code(a: &Script, b: &Script) -> bool {
    a.code_hash() == b.code_hash() && a.hash_type() == b.hash_type()
}

fn verify_stake_type(script: &Script, registry: Hash32, key: Hash32) -> Result<(), Error> {
    let input = find_type(&registry, Source::Input)?.ok_or(Error::Registry)?;
    let output = find_type(&registry, Source::Output)?.ok_or(Error::Registry)?;
    for (index, source) in [(input, Source::Input), (output, Source::Output)] {
        let registry = load_cell_type(index, source)?.ok_or(Error::Registry)?;
        if !same_code(script, &registry) || !(121..=170).contains(&registry.args().raw_data().len())
        {
            return Err(Error::Registry);
        }
    }
    let witness = witness(input)?;
    if !matches!(witness.first().copied(), Some(0..=6 | FINALIZE_SLASH))
        || witness.get(1..33) != Some(&key)
    {
        return Err(Error::Stake);
    }
    Ok(())
}

fn verify_lock(script: &Script, registry: Hash32) -> Result<(), Error> {
    for index in 0.. {
        match load_cell_type(index, Source::GroupInput) {
            Ok(Some(kind)) => {
                let args = kind.args().raw_data();
                if !same_code(script, &kind)
                    || (kind.calc_script_hash().as_slice() != registry
                        && (args.len() != 64 || args[..32] != registry))
                    || load_cell_lock(index, Source::GroupInput)? != *script
                {
                    return Err(Error::Stake);
                }
            }
            Err(SysError::IndexOutOfBound) => break,
            Err(error) => return Err(error.into()),
            Ok(None) => return Err(Error::Stake),
        }
    }
    Ok(())
}

fn inputs_authorize(owner: &Hash32) -> Result<bool, Error> {
    for index in 0.. {
        match load_cell_lock_hash(index, Source::Input) {
            Ok(candidate) if candidate == *owner => return Ok(true),
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn sum_capacity(source: Source) -> Result<u64, Error> {
    let mut total = 0u64;
    for index in 0.. {
        match load_cell_capacity(index, source) {
            Ok(capacity) => total = total.checked_add(capacity).ok_or(Error::Capacity)?,
            Err(SysError::IndexOutOfBound) => return Ok(total),
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!()
}

fn fee() -> Result<u64, Error> {
    sum_capacity(Source::Input)?
        .checked_sub(sum_capacity(Source::Output)?)
        .ok_or(Error::Capacity)
}

type ProofData<'a> = (Vec<Member>, Vec<Attestation>, &'a [u8]);

fn read_proof(bytes: &[u8]) -> Result<ProofData<'_>, Error> {
    let (keys, tail) = read_items(bytes, 40)?;
    let (votes, rest) = read_items(tail, 66)?;
    let committee = keys
        .chunks_exact(40)
        .map(|key| Member {
            key: key[..32].try_into().unwrap(),
            power: u64::from_le_bytes(key[32..].try_into().unwrap()),
        })
        .collect();
    let votes = votes
        .chunks_exact(66)
        .map(|vote| Attestation {
            guardian: u16::from_le_bytes(vote[..2].try_into().unwrap()),
            signature: vote[2..].try_into().unwrap(),
        })
        .collect();
    Ok((committee, votes, rest))
}

fn read_items(bytes: &[u8], width: usize) -> Result<(&[u8], &[u8]), Error> {
    let (len, bytes) = bytes.split_at_checked(2).ok_or(Error::Encoding)?;
    let count = u16::from_le_bytes(len.try_into().unwrap()) as usize;
    if count > MAX_RECORDS {
        return Err(Error::Encoding);
    }
    bytes.split_at_checked(count * width).ok_or(Error::Encoding)
}
