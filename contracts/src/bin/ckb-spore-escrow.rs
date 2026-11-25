//! CKB mailbox Lock that holds and releases the same original Spore V2 Cell.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data, load_cell_lock_hash, load_cell_type,
        load_cell_type_hash, load_script, load_script_hash, load_witness_args,
    },
};
use stem_bridge_wire::{
    intercell::{Delivery, Identity, MailboxPolicy, MailboxState, Message},
    spore::{CKB_SPORE_CODE, SporeTransfer},
};

entry!(main);
default_alloc!();

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 110)?;
    let args = script.args().raw_data();
    let (fields, rest) = args.as_chunks::<32>();
    let [mailbox_id, endpoint, original, mailbox_code] = fields else {
        return Err(110);
    };
    if !rest.is_empty()
        || fields.contains(&[0; 32])
        || u8::from(script.hash_type()) != ScriptHashType::Data2 as u8
    {
        return Err(110);
    }
    let owner = load_script_hash().map_err(|_| 110)?;
    let mailbox_input = locate_mailbox(&owner, mailbox_id, mailbox_code)?;
    let mailbox_script = load_cell_type(mailbox_input, Source::Input)
        .map_err(|_| 110)?
        .ok_or(110)?;
    let mailbox: Identity = mailbox_script.calc_script_hash().unpack();
    let successor = find(Source::Output, &mailbox)?;
    if load_cell_capacity(mailbox_input, Source::Input).map_err(|_| 110)?
        != load_cell_capacity(successor, Source::Output).map_err(|_| 110)?
    {
        return Err(111);
    }
    let next_bytes = load_cell_data(successor, Source::Output).map_err(|_| 110)?;
    let next = MailboxState::decode(&next_bytes).ok_or(110)?;
    let spore_input = find(Source::Input, original)?;
    let spore_output = find(Source::Output, original)?;
    let held_before = load_cell_lock_hash(spore_input, Source::Input).map_err(|_| 110)? == owner;
    let after = load_cell_lock_hash(spore_output, Source::Output).map_err(|_| 110)?;
    let held_after = after == owner;
    if held_before == held_after {
        return Err(111);
    }
    // Restrict this permissionless Lock to its unique mailbox and actual Spore.
    for index in 0.. {
        match load_cell_type_hash(index, Source::GroupInput) {
            Ok(Some(ty)) if ty == mailbox || (held_before && ty == *original) => {}
            Err(SysError::IndexOutOfBound) => break,
            _ => return Err(111),
        }
    }
    let object_type = load_cell_type(spore_input, Source::Input)
        .map_err(|_| 110)?
        .ok_or(110)?;
    if object_type.code_hash().as_slice() != CKB_SPORE_CODE
        || u8::from(object_type.hash_type()) != ScriptHashType::Data1 as u8
        || object_type.args().raw_data().len() != 32
    {
        return Err(112);
    }
    let input = load_cell_data(spore_input, Source::Input).map_err(|_| 110)?;
    let output = load_cell_data(spore_output, Source::Output).map_err(|_| 110)?;
    if input != output
        || load_cell_capacity(spore_input, Source::Input).map_err(|_| 110)?
            != load_cell_capacity(spore_output, Source::Output).map_err(|_| 110)?
    {
        return Err(112);
    }
    if held_after {
        let message = Message::decode(next.outgoing).ok_or(113)?;
        if message.receiver != *endpoint {
            return Err(113);
        }
        verify_transfer(message.payload, original, &input, None, endpoint)?;
    } else {
        if !next.outgoing.is_empty() {
            return Err(113);
        }
        let witness = load_witness_args(mailbox_input, Source::Input).map_err(|_| 110)?;
        let bytes = witness.input_type().to_opt().ok_or(113)?.raw_data();
        let record = Delivery::decode(&bytes).ok_or(113)?.record;
        let message = Message::decode(record).ok_or(113)?;
        if message.sender != *endpoint {
            return Err(113);
        }
        verify_transfer(message.payload, original, &input, Some(&after), &owner)?;
    }
    Ok(())
}

fn locate_mailbox(owner: &Identity, id: &Identity, code: &Identity) -> Result<usize, i8> {
    for index in 0.. {
        let script = match load_cell_type(index, Source::Input) {
            Ok(Some(script)) => script,
            Ok(None) => continue,
            Err(_) => return Err(110),
        };
        if script.code_hash().as_slice() != code
            || u8::from(script.hash_type()) != ScriptHashType::Data2 as u8
        {
            continue;
        }
        if MailboxPolicy::decode(&script.args().raw_data())
            .is_some_and(|policy| policy.type_id == *id && policy.owner == *owner)
            && load_cell_lock_hash(index, Source::Input).map_err(|_| 110)? == *owner
        {
            return Ok(index);
        }
    }
    unreachable!()
}

fn find(source: Source, target: &Identity) -> Result<usize, i8> {
    let mut matching = None;
    for index in 0.. {
        match load_cell_type_hash(index, source) {
            Ok(Some(ty)) if ty == *target => {
                if matching.replace(index).is_some() {
                    return Err(111);
                }
            }
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => return matching.ok_or(111),
            Err(_) => return Err(110),
        }
    }
    unreachable!()
}

fn verify_transfer(
    payload: &[u8],
    original: &Identity,
    data: &[u8],
    recipient: Option<&Identity>,
    custody: &Identity,
) -> Result<(), i8> {
    let transfer = SporeTransfer::decode(payload).ok_or(113)?;
    if transfer.original_type != *original
        || transfer.data != data
        || &transfer.recipient == custody
        || recipient.is_some_and(|recipient| *recipient != transfer.recipient)
    {
        return Err(113);
    }
    Ok(())
}
