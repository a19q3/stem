//! Redeem a separately funded, transaction-specific reimbursement authorization on CKB.
#![no_std]
#![no_main]

use alloc::collections::{BTreeMap, BTreeSet};
use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data_hash, load_cell_lock_hash, load_cell_type_hash,
        load_input_out_point, load_script, load_script_hash,
    },
    syscalls::load_witness,
};
use stem_bridge_wire::expense::{Grant, MAX_EXPENSE_INPUTS, MAX_GRANT_WITNESS};

entry!(main);
default_alloc!();

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 120)?;
    let grant = Grant::decode(&script.args().raw_data()).ok_or(120)?;
    let own = load_script_hash().map_err(|_| 120)?;
    if u8::from(script.hash_type()) != ScriptHashType::Data2 as u8
        || own == grant.payer
        || own == grant.refund
    {
        return Err(120);
    }
    let lock = |i, src| load_cell_lock_hash(i, src).map_err(|_| 121);
    let capacity = |i, src| load_cell_capacity(i, src).map_err(|_| 121);
    if lock(1, Source::Input)? != own
        || load_cell_capacity(2, Source::Input) != Err(SysError::IndexOutOfBound)
    {
        return Err(121);
    }
    let principal = capacity(1, Source::Input)?;
    let signer = lock(0, Source::Input)?;
    // The refund authority can reclaim a stale or duplicate grant; the entire
    // grant principal goes back to that lock and the signer covers the fee.
    if signer == grant.refund {
        if lock(0, Source::Output)? != grant.refund
            || capacity(0, Source::Output)? < principal
            || load_cell_capacity(1, Source::Output) != Err(SysError::IndexOutOfBound)
        {
            return Err(121);
        }
        plain(0, Source::Output)?;
        return Ok(());
    }
    if signer != grant.payer
        || lock(0, Source::Output)? != grant.payer
        || lock(1, Source::Output)? != grant.refund
        || load_cell_capacity(2, Source::Output) != Err(SysError::IndexOutOfBound)
    {
        return Err(121);
    }
    for source in [Source::Input, Source::Output] {
        for index in 0..2 {
            plain(index, source)?;
        }
    }
    let point = load_input_out_point(0, Source::Input).map_err(|_| 122)?;
    if point.tx_hash().as_slice() != grant.expense {
        return Err(122);
    }
    let fee = original_fee(&grant, u32::from(point.index()) as usize)?;
    let payer_input = capacity(0, Source::Input)?;
    let payer_output = capacity(0, Source::Output)?;
    let refund = capacity(1, Source::Output)?;
    if fee == 0
        || fee > grant.max_expense_fee
        || payer_output.checked_sub(payer_input) != Some(fee)
        || principal
            .checked_sub(refund)
            .and_then(|debit| debit.checked_sub(fee))
            .is_none_or(|miner_fee| miner_fee > grant.max_settlement_fee)
    {
        return Err(123);
    }
    Ok(())
}

fn plain(index: usize, source: Source) -> Result<(), i8> {
    if load_cell_type_hash(index, source)
        .map_err(|_| 121)?
        .is_some()
        || load_cell_data_hash(index, source).map_err(|_| 121)? != [0; 32]
    {
        return Err(121);
    }
    Ok(())
}

fn original_fee(grant: &Grant, change_index: usize) -> Result<u64, i8> {
    // Input zero is a live UTXO of this exact, already validated expense.
    // It commits to all intermediate OutPoints and can authorize at most one claim.
    let size = match load_witness(&mut [], 0, 0, Source::GroupInput) {
        Ok(n) | Err(SysError::LengthNotEnough(n)) if n <= MAX_GRANT_WITNESS => n,
        _ => return Err(122),
    };
    let mut buffer = alloc::vec![0; size];
    load_witness(&mut buffer, 0, 0, Source::GroupInput).map_err(|_| 122)?;
    let witness = packed::WitnessArgs::from_slice(&buffer).map_err(|_| 122)?;
    let data = witness.lock().to_opt().ok_or(122)?.raw_data();
    let items = packed::BytesVec::from_slice(&data).map_err(|_| 122)?;
    if !(2..=1 + MAX_EXPENSE_INPUTS).contains(&items.len()) {
        return Err(122);
    }
    let mut iter = items.into_iter();
    let original =
        packed::RawTransaction::from_slice(&iter.next().ok_or(122)?.raw_data()).map_err(|_| 122)?;
    if original.calc_tx_hash().as_slice() != grant.expense
        || original.outputs().len().checked_sub(1) != Some(change_index)
        || !(1..=MAX_EXPENSE_INPUTS).contains(&original.inputs().len())
    {
        return Err(122);
    }

    let mut parents = BTreeMap::new();
    for bytes in iter {
        let parent = packed::RawTransaction::from_slice(&bytes.raw_data()).map_err(|_| 122)?;
        if parents.insert(parent.calc_tx_hash(), parent).is_some() {
            return Err(122);
        }
    }
    let mut used = BTreeSet::new();
    let mut total = 0u64;
    for input in original.inputs() {
        let point = input.previous_output();
        let parent = parents.get(&point.tx_hash()).ok_or(122)?;
        let output = parent
            .outputs()
            .get(u32::from(point.index()) as usize)
            .ok_or(122)?;
        if output.type_().to_opt().is_some()
            || output.lock().calc_script_hash().as_slice() != grant.payer
        {
            return Err(122);
        }
        used.insert(point.tx_hash());
        total = total.checked_add(u64::from(output.capacity())).ok_or(123)?;
    }
    if used.len() != parents.len() {
        return Err(122);
    }
    for output in original.outputs() {
        total = total.checked_sub(u64::from(output.capacity())).ok_or(123)?;
    }
    Ok(total)
}
