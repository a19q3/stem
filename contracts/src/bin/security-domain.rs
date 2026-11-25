//! Paid, slashable CKB Live Cell attestations with separate service capital.
#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{
        core::ScriptHashType,
        packed::{Byte32, Script},
        prelude::*,
    },
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell, load_cell_capacity, load_cell_data, load_cell_data_hash, load_cell_lock,
        load_cell_lock_hash, load_cell_type, load_cell_type_hash, load_header, load_input,
        load_input_since, load_script, load_script_hash, load_transaction, load_witness_args,
    },
    type_id,
};
use stem_stake::{
    Hash32,
    security::{self, Certificate, Domain, Payee, Policy, Request},
};

entry!(main);
default_alloc!();

#[repr(i8)]
enum Error {
    Syscall = 94,
    Encoding = 95,
    Identity = 96,
    Backing = 97,
    Service = 98,
    Liability = 99,
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
    let id = load_script_hash()?;
    if u8::from(script.hash_type()) != ScriptHashType::Data2 as u8 {
        return Err(Error::Identity);
    }
    let args = script.args().raw_data();
    match args.len() {
        32 => {
            if load_cell_type_hash(0, Source::GroupInput)?
                .as_ref()
                .is_some_and(|hash| hash == &args[..])
                && absent(1, Source::GroupInput)
            {
                Ok(())
            } else {
                Err(Error::Identity)
            }
        }
        Policy::SIZE => verify_domain(
            &script,
            &id,
            Policy::decode(&args).map_err(|_| Error::Encoding)?,
        ),
        Request::SIZE => verify_request(
            &script,
            &id,
            Request::decode(&args).map_err(|_| Error::Encoding)?,
        ),
        68 => verify_receipt(&script, &args),
        _ => Err(Error::Encoding),
    }
}

fn verify_domain(script: &Script, id: &Hash32, policy: Policy) -> Result<(), Error> {
    type_id::validate_type_id(&policy.type_id)?;
    let lock = script
        .clone()
        .as_builder()
        .args(id.as_slice().pack())
        .build();
    if absent(0, Source::GroupInput) {
        let genesis = load_header(0, Source::HeaderDep)?;
        if genesis.calc_header_hash().as_slice() != policy.ckb_genesis
            || genesis.raw().number().unpack() != 0u64
            || load_cell_lock(0, Source::GroupOutput)? != lock
        {
            return Err(Error::Identity);
        }
        let data = load_cell_data(0, Source::GroupOutput)?;
        let domain = Domain::decode(&data).map_err(|_| Error::Encoding)?;
        if domain.retired || domain.positions.iter().any(|position| position.slashed) {
            return Err(Error::Liability);
        }
        let capacity = load_cell_capacity(0, Source::GroupOutput)?;
        let minimum = occupied(&lock, Some(script), data.len())?;
        let bonded = domain.bonded().map_err(|_| Error::Backing)?;
        return if capacity
            .checked_sub(minimum)
            .is_some_and(|value| value >= bonded)
        {
            Ok(())
        } else {
            Err(Error::Backing)
        };
    }

    if load_cell_type_hash(0, Source::Input)? != Some(*id)
        || load_cell_lock(0, Source::Input)? != lock
    {
        return Err(Error::Identity);
    }
    let old_data = load_cell_data(0, Source::Input)?;
    let mut domain = Domain::decode(&old_data).map_err(|_| Error::Encoding)?;
    let (operation, data) = operation(0)?;
    let capacity = load_cell_capacity(0, Source::Input)?;

    if operation == security::CLOSE {
        if !data.is_empty()
            || !domain.retired
            || !absent(0, Source::GroupOutput)
            || load_input_since(0, Source::Input)?
                != (security::RELATIVE_HEIGHT | policy.challenge_blocks)
        {
            return Err(Error::Liability);
        }
        let mut index = 0;
        for shareholder in domain.positions.iter().filter(|position| !position.slashed) {
            pay(index, &shareholder.beneficiary, shareholder.bond)?;
            index += 1;
        }
        let remainder = capacity
            .checked_sub(domain.bonded().map_err(|_| Error::Backing)?)
            .ok_or(Error::Backing)?;
        return pay(index, &policy.operator, remainder);
    }

    if load_cell_type_hash(0, Source::Output)? != Some(*id)
        || load_cell_lock(0, Source::Output)? != lock
    {
        return Err(Error::Identity);
    }
    let expected = match operation {
        security::SERVE => {
            verify_service(script, id, &policy, &domain, &data)?;
            capacity
        }
        security::RETIRE => {
            if !data.is_empty()
                || domain.retired
                || load_cell_lock(1, Source::Input)? != payee_lock(&policy.operator)
                || load_cell_type(1, Source::Input)?.is_some()
            {
                return Err(Error::Liability);
            }
            domain.retired = true;
            capacity
        }
        security::SLASH => {
            let index = domain
                .verify_double_report(id, &data)
                .map_err(|_| Error::Liability)?;
            let offender = &mut domain.positions[index];
            offender.slashed = true;
            pay(1, &policy.penalty, offender.bond)?;
            capacity.checked_sub(offender.bond).ok_or(Error::Backing)?
        }
        _ => return Err(Error::Encoding),
    };
    if load_cell_capacity(0, Source::Output)? != expected
        || load_cell_data(0, Source::Output)? != domain.encode()
    {
        return Err(Error::Backing);
    }
    Ok(())
}

fn verify_service(
    domain_script: &Script,
    domain_id: &Hash32,
    policy: &Policy,
    domain: &Domain,
    raw: &[u8],
) -> Result<(), Error> {
    let lock = load_cell_lock(1, Source::Input)?;
    if !same_program(domain_script, &lock)
        || load_cell_type(1, Source::Input)?.is_some()
        || !load_cell_data(1, Source::Input)?.is_empty()
    {
        return Err(Error::Identity);
    }
    let request = Request::decode(&lock.args().raw_data()).map_err(|_| Error::Encoding)?;
    if request.domain != *domain_id {
        return Err(Error::Identity);
    }
    let tx = load_transaction()?.raw();
    let first = tx.cell_deps().get(0).ok_or(Error::Service)?;
    if u8::from(first.dep_type()) != 0
        || first.out_point().as_slice() != request.subject
        || tx
            .inputs()
            .into_iter()
            .any(|input| input.previous_output().as_slice() == request.subject)
    {
        return Err(Error::Service);
    }
    let result = security::live_value(
        &request.subject,
        load_cell(0, Source::CellDep)?.as_slice(),
        &load_cell_data_hash(0, Source::CellDep)?,
    );
    let certificate =
        Certificate::decode(raw, domain.positions.len()).map_err(|_| Error::Encoding)?;
    if certificate.result != result {
        return Err(Error::Service);
    }
    let outpoint = load_input(1, Source::Input)?.previous_output();
    let reward = load_cell_capacity(1, Source::Input)?
        .checked_sub(request.receipt_capacity)
        .ok_or(Error::Backing)?;
    let payouts = domain
        .verify_service(
            policy,
            domain_id,
            outpoint
                .as_slice()
                .try_into()
                .map_err(|_| Error::Encoding)?,
            &result,
            reward,
            certificate.bitmap,
            certificate.signatures,
        )
        .map_err(|_| Error::Service)?;
    let receipt = payouts.len() + 1;
    for (output, (index, amount)) in payouts.into_iter().enumerate() {
        pay(output + 1, &domain.positions[index].beneficiary, amount)?;
    }
    let receipt_args = [domain_id.as_slice(), outpoint.as_slice()].concat();
    let expected_type = domain_script
        .clone()
        .as_builder()
        .args(receipt_args.pack())
        .build();
    if load_cell_type(receipt, Source::Output)?.as_ref() != Some(&expected_type)
        || load_cell_lock(receipt, Source::Output)? != payee_lock(&request.buyer)
        || load_cell_capacity(receipt, Source::Output)? != request.receipt_capacity
        || load_cell_data(receipt, Source::Output)? != result
    {
        return Err(Error::Service);
    }
    Ok(())
}

fn verify_request(script: &Script, id: &Hash32, request: Request) -> Result<(), Error> {
    if !absent(1, Source::GroupInput) {
        return Err(Error::Identity);
    }
    if load_cell_lock_hash(0, Source::Input)? == *id {
        let (kind, data) = operation(0)?;
        if kind != security::REFUND
            || !data.is_empty()
            || load_input_since(0, Source::Input)? != request.refund_height
            || load_cell_type(0, Source::Input)?.is_some()
            || !load_cell_data(0, Source::Input)?.is_empty()
        {
            return Err(Error::Liability);
        }
        return pay(0, &request.buyer, load_cell_capacity(0, Source::Input)?);
    }
    if load_cell_lock_hash(1, Source::Input)? != *id {
        return Err(Error::Identity);
    }
    require_service(script, &request.domain)
}

fn verify_receipt(script: &Script, args: &[u8]) -> Result<(), Error> {
    if absent(0, Source::GroupInput) {
        if !absent(1, Source::GroupOutput)
            || load_input(1, Source::Input)?.previous_output().as_slice() != &args[32..]
        {
            return Err(Error::Identity);
        }
        require_service(script, args[..32].try_into().unwrap())
    } else if absent(0, Source::GroupOutput) {
        Ok(())
    } else {
        Err(Error::Identity)
    }
}

fn require_service(script: &Script, domain_id: &Hash32) -> Result<(), Error> {
    let source = load_cell_type(0, Source::Input)?.ok_or(Error::Identity)?;
    if !same_program(script, &source)
        || source.args().raw_data().len() != Policy::SIZE
        || source.calc_script_hash().as_slice() != *domain_id
        || operation(0)?.0 != security::SERVE
    {
        return Err(Error::Identity);
    }
    Ok(())
}

fn same_program(a: &Script, b: &Script) -> bool {
    a.code_hash() == b.code_hash() && a.hash_type() == b.hash_type()
}

fn absent(index: usize, source: Source) -> bool {
    load_cell_capacity(index, source) == Err(SysError::IndexOutOfBound)
}

fn operation(index: usize) -> Result<(u8, ckb_std::ckb_types::bytes::Bytes), Error> {
    let witness = load_witness_args(index, Source::Input)?;
    let encoded = witness
        .input_type()
        .to_opt()
        .ok_or(Error::Encoding)?
        .raw_data();
    let kind = encoded.first().ok_or(Error::Encoding)?;
    Ok((*kind, encoded.slice(1..)))
}

fn pay(index: usize, beneficiary: &Payee, amount: u64) -> Result<(), Error> {
    if load_cell_lock(index, Source::Output)? != payee_lock(beneficiary)
        || load_cell_capacity(index, Source::Output)? != amount
        || load_cell_type(index, Source::Output)?.is_some()
        || !load_cell_data(index, Source::Output)?.is_empty()
    {
        return Err(Error::Backing);
    }
    Ok(())
}

fn payee_lock(payee: &Payee) -> Script {
    Script::new_builder()
        .code_hash(Byte32::from_slice(&security::SIGHASH_TYPE).expect("32-byte code hash"))
        .hash_type(ScriptHashType::Type)
        .args(payee.as_slice().pack())
        .build()
}

fn occupied(lock: &Script, type_: Option<&Script>, bytes: usize) -> Result<u64, Error> {
    let script_bytes = |script: &Script| 33 + script.args().raw_data().len();
    let bytes = 8 + script_bytes(lock) + type_.map_or(0, script_bytes) + bytes;
    (bytes as u64)
        .checked_mul(100_000_000)
        .ok_or(Error::Backing)
}
