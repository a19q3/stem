//! Native object constraint backed by a resolved or authenticated rule record.

#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{
        core::ScriptHashType,
        packed::{Script, ScriptReader},
        prelude::*,
    },
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_capacity, load_cell_lock, load_script},
};
use stem_bridge_wire::metatype::{
    LogicalScript, MAX_ADAPTER_TABLE_BYTES, MAX_STATE_BYTES, MetatypeAdapterTable,
    MetatypeDeployment,
    custody::EscrowArgs,
    object_policy::{ObjectPolicy, RouteCodes, RuleSource, StateLimit, valid_state},
};
use stem_contracts::{
    metatype::{ManifestResolver, bounded_data},
    native_type,
};

entry!(main);
default_alloc!();

fn main() -> i8 {
    if verify().is_some() { 0 } else { 56 }
}

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let args = script.args().raw_data();
    let policy = ObjectPolicy::decode(&args)?;
    let read = |source| match bounded_data(0, source, MAX_STATE_BYTES) {
        Ok(data)
            if valid_state(&data)
                && native_type::contains(source, 0, policy.object) == Some(true)
                && load_cell_capacity(1, source) == Err(SysError::IndexOutOfBound) =>
        {
            Some(Some(data))
        }
        Err(SysError::IndexOutOfBound) if source == Source::GroupInput => Some(None),
        _ => None,
    };
    let before = read(Source::GroupInput)?;
    let after = read(Source::GroupOutput)??;
    match policy.rule {
        RuleSource::DataHash(hash) => {
            let limit = StateLimit::decode(
                &ManifestResolver::default().dependency_data(hash, StateLimit::ENCODED_BYTES)?,
            )?;
            (before.as_deref().is_none_or(|data| limit.permits(data)) && limit.permits(&after))
                .then_some(())
        }
        RuleSource::LiveCkbType { registry, route } => {
            let limit = StateLimit::decode(
                &native_type::live_ckb_data::<{ 32 + StateLimit::ENCODED_BYTES }>(registry)?[32..],
            )?;
            if limit.permits_successor(before.as_deref(), &after) {
                return Some(());
            }
            // The escrow's independent Lock authenticates a receive from the
            // pinned CKB route, including its actual returned object bytes.
            authorized_return(&script, policy.object, route)
        }
    }
}

fn authorized_return(rule: &Script, object: &[u8; 32], commitment: &[u8; 32]) -> Option<()> {
    const MAX_PROOF: usize = RouteCodes::SIZE + RouteCodes::WRAPPER_SIZE + MAX_ADAPTER_TABLE_BYTES;
    let mut proof = [0; MAX_PROOF];
    let mut length = MAX_PROOF as u64;
    let mut status = proof.as_mut_ptr() as u64;
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut length, in("a2") 0u64,
            in("a3") 0u64, in("a4") 0u64, in("a5") 0u64, in("a7") 2117u64);
    }
    if status != 0 {
        return None;
    }
    let (codes, proof) = proof
        .get(..length as usize)?
        .split_at_checked(RouteCodes::SIZE)?;
    let (wrapper, table) = proof.split_at_checked(RouteCodes::WRAPPER_SIZE)?;
    let codes = RouteCodes::authenticate(codes, commitment)?;
    let lock = load_cell_lock(0, Source::GroupInput).ok()?;
    if lock.code_hash().as_slice() != codes.escrow
        || u8::from(lock.hash_type()) != ScriptHashType::Data as u8
    {
        return None;
    }
    let args = lock.args().raw_data();
    let escrow = EscrowArgs::decode(&args)?;
    if !codes.matches_custody(&escrow.custody, object)
        || &ckb_hash::blake2b_256(wrapper) != escrow.metatype
    {
        return None;
    }
    let wrapper = ScriptReader::from_slice(wrapper).ok()?;
    if wrapper.code_hash().as_slice() != codes.dispatcher
        || u8::from(wrapper.hash_type().to_entity()) != ScriptHashType::Data2 as u8
    {
        return None;
    }
    let deployment = MetatypeDeployment::decode(wrapper.args().raw_data())?;
    if &deployment.ckb_genesis != escrow.ckb
        || deployment.adapter_table_data_hash != ckb_hash::blake2b_256(table)
    {
        return None;
    }
    let table = MetatypeAdapterTable::decode(table)?;
    if table.custody != Some(escrow.custody)
        || table.adapter(LogicalScript::decode(rule.as_slice())?) != Some(codes.adapter)
    {
        return None;
    }
    Some(())
}
