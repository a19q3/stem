//! Wrapped Stem UDT and the permissionless, asset-restricted CKB mailbox Lock.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data, load_cell_lock_hash, load_cell_type,
        load_cell_type_hash, load_script, load_script_hash, load_witness_args,
    },
};
use stem_bridge_wire::{
    asset::{AssetTransfer, CKB_ESCROW_FIELDS},
    intercell::{Delivery, Identity, MailboxPolicy, MailboxState, Message},
};

entry!(main);
default_alloc!();

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 101)?;
    let args = script.args().raw_data();
    if u8::from(script.hash_type()) != ScriptHashType::Data2 as u8 {
        return Err(101);
    }
    let (fields, remaining) = args.as_chunks::<32>();
    if !remaining.is_empty() || fields.contains(&[0; 32]) {
        return Err(101);
    }
    if let Ok(fields) = <&[[u8; 32]; CKB_ESCROW_FIELDS]>::try_from(fields) {
        return mailbox_lock(&script, fields);
    }
    let [mailbox] = fields else { return Err(101) };
    let inputs = total(Source::GroupInput, None, None).ok_or(102)?;
    let outputs = total(Source::GroupOutput, None, None).ok_or(102)?;
    if inputs == outputs {
        return Ok(());
    }
    find(Source::Input, mailbox).ok_or(103)?;
    Ok(())
}

fn mailbox_lock(script: &Script, fields: &[[u8; 32]; CKB_ESCROW_FIELDS]) -> Result<(), i8> {
    let [type_id, stem_escrow, asset, mailbox_code] = fields;
    let owner = load_script_hash().map_err(|_| 101)?;
    if load_cell_lock_hash(1, Source::GroupInput) != Err(SysError::IndexOutOfBound) {
        return Err(101);
    }
    let mailbox = load_cell_type(0, Source::GroupInput)
        .map_err(|_| 101)?
        .ok_or(101)?;
    if mailbox.code_hash().as_slice() != mailbox_code
        || u8::from(mailbox.hash_type()) != ScriptHashType::Data2 as u8
    {
        return Err(101);
    }
    let policy = MailboxPolicy::decode(&mailbox.args().raw_data()).ok_or(101)?;
    if policy.type_id != *type_id || policy.owner != owner {
        return Err(101);
    }
    let mailbox: Identity = mailbox.calc_script_hash().unpack();
    let successor = find(Source::Output, &mailbox).ok_or(101)?;
    let before = load_cell_capacity(0, Source::GroupInput).map_err(|_| 101)?;
    if load_cell_capacity(successor, Source::Output).map_err(|_| 101)? != before {
        return Err(101);
    }
    let token: Identity = script
        .clone()
        .as_builder()
        .args(mailbox.as_slice().pack())
        .build()
        .calc_script_hash()
        .unpack();
    let raw = load_cell_data(successor, Source::Output).map_err(|_| 101)?;
    let state = MailboxState::decode(&raw).ok_or(101)?;
    let input = total(Source::Input, Some(&token), None).ok_or(102)?;
    if state.outgoing.is_empty() {
        let witness = load_witness_args(0, Source::GroupInput).map_err(|_| 103)?;
        let input_type = witness.input_type().to_opt().ok_or(103)?.raw_data();
        let record = Delivery::decode(&input_type).ok_or(103)?.record;
        let message = Message::decode(record).ok_or(103)?;
        let transfer = AssetTransfer::decode(message.payload).ok_or(103)?;
        let output = total(Source::Output, Some(&token), Some(&transfer.recipient)).ok_or(104)?;
        if message.sender != *stem_escrow
            || transfer.asset != *asset
            || input != 0
            || output != transfer.amount
            || transfer.recipient == owner
        {
            return Err(104);
        }
    } else {
        let message = Message::decode(state.outgoing).ok_or(103)?;
        let transfer = AssetTransfer::decode(message.payload).ok_or(103)?;
        let output = total(Source::Output, Some(&token), None).ok_or(104)?;
        if message.receiver != *stem_escrow
            || transfer.asset != *asset
            || transfer.recipient == *stem_escrow
            || input.checked_sub(output) != Some(transfer.amount)
        {
            return Err(104);
        }
    }
    Ok(())
}

fn find(source: Source, target: &Identity) -> Option<usize> {
    for index in 0.. {
        match load_cell_type_hash(index, source) {
            Ok(Some(hash)) if hash == *target => return Some(index),
            Ok(_) => {}
            Err(_) => return None,
        }
    }
    unreachable!()
}

fn total(source: Source, token: Option<&Identity>, recipient: Option<&Identity>) -> Option<u128> {
    let mut sum = 0u128;
    for index in 0.. {
        match load_cell_type_hash(index, source) {
            Ok(hash) if token.is_none() || hash.as_ref() == token => {
                if let Some(recipient) = recipient {
                    if load_cell_lock_hash(index, source).ok()? != *recipient {
                        return None;
                    }
                }
                let amount = load_cell_data(index, source).ok()?;
                let amount = u128::from_le_bytes(amount.as_slice().try_into().ok()?);
                if amount == 0 {
                    return None;
                }
                sum = sum.checked_add(amount)?;
            }
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => return Some(sum),
            Err(_) => return None,
        }
    }
    unreachable!()
}
