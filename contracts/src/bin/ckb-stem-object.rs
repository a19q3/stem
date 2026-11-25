//! CKB representation and mailbox for a policy-bearing Stem object.

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
    intercell::{Delivery, Identity, MailboxPolicy, MailboxState, Message},
    spore::{SporeTransfer, plain_spore},
};

entry!(main);
default_alloc!();

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 120)?;
    let args = script.args().raw_data();
    let (fields, rest) = args.as_chunks::<32>();
    if !rest.is_empty()
        || u8::from(script.hash_type()) != ScriptHashType::Data2 as u8
        || fields.contains(&[0; 32])
    {
        return Err(120);
    }
    match fields {
        [mailbox, owner, authority] => wrapped(mailbox, owner, authority),
        fields => mailbox_lock(&script, fields.try_into().map_err(|_| 120)?),
    }
}

fn authorized(authority: &Identity) -> Result<(), i8> {
    for index in 0.. {
        match load_cell_lock_hash(index, Source::Input) {
            Ok(hash) if &hash == authority => return Ok(()),
            Ok(_) => {}
            Err(_) => return Err(121),
        }
    }
    unreachable!()
}

fn wrapped(mailbox: &Identity, owner: &Identity, authority: &Identity) -> Result<(), i8> {
    authorized(authority)?;
    let read = |source| -> Result<_, i8> {
        match load_cell_data(0, source) {
            Ok(data)
                if plain_spore(&data)
                    && load_cell_capacity(1, source) == Err(SysError::IndexOutOfBound) =>
            {
                Ok(Some(data))
            }
            Err(SysError::IndexOutOfBound) => Ok(None),
            _ => Err(122),
        }
    };
    match (read(Source::GroupInput)?, read(Source::GroupOutput)?) {
        (Some(before), Some(after)) if before == after => Ok(()),
        (Some(_), None) | (None, Some(_)) => {
            let carrier = find(Source::Input, mailbox)?.ok_or(122)?;
            (load_cell_lock_hash(carrier, Source::Input).map_err(|_| 122)? == *owner)
                .then_some(())
                .ok_or(122)
        }
        _ => Err(122),
    }
}

fn mailbox_lock(script: &Script, policy: &[[u8; 32]; 5]) -> Result<(), i8> {
    let [id, stem, original, mailbox_code, authority] = policy;
    if load_cell_type_hash(1, Source::GroupInput) != Err(SysError::IndexOutOfBound) {
        return Err(120);
    }
    let mailbox = load_cell_type(0, Source::GroupInput)
        .map_err(|_| 120)?
        .ok_or(120)?;
    let owner = load_script_hash().map_err(|_| 120)?;
    if mailbox.code_hash().as_slice() != mailbox_code
        || u8::from(mailbox.hash_type()) != ScriptHashType::Data2 as u8
        || !MailboxPolicy::decode(&mailbox.args().raw_data())
            .is_some_and(|p| p.type_id == *id && p.owner == owner)
    {
        return Err(120);
    }
    let mailbox: Identity = mailbox.calc_script_hash().unpack();
    let successor = find(Source::Output, &mailbox)?.ok_or(120)?;
    if load_cell_capacity(0, Source::GroupInput).map_err(|_| 120)?
        != load_cell_capacity(successor, Source::Output).map_err(|_| 120)?
    {
        return Err(120);
    }
    let args = [mailbox.as_slice(), &owner, authority].concat();
    let wrapped: Identity = script
        .clone()
        .as_builder()
        .args(args.pack())
        .build()
        .calc_script_hash()
        .unpack();
    let raw = load_cell_data(successor, Source::Output).map_err(|_| 120)?;
    let next = MailboxState::decode(&raw).ok_or(120)?;
    let left = find(Source::Input, &wrapped)?;
    let right = find(Source::Output, &wrapped)?;
    if next.outgoing.is_empty() {
        let witness = load_witness_args(0, Source::GroupInput).map_err(|_| 123)?;
        let input_type = witness.input_type().to_opt().ok_or(123)?.raw_data();
        let bytes = Delivery::decode(&input_type).ok_or(123)?.record;
        let message = Message::decode(bytes).ok_or(123)?;
        let transfer = SporeTransfer::decode(message.payload).ok_or(123)?;
        let output = right.ok_or(123)?;
        if left.is_some()
            || message.sender != *stem
            || transfer.original_type != *original
            || transfer.recipient == owner
            || load_cell_lock_hash(output, Source::Output).map_err(|_| 123)? != transfer.recipient
            || load_cell_data(output, Source::Output).map_err(|_| 123)? != transfer.data
        {
            return Err(123);
        }
    } else {
        let message = Message::decode(next.outgoing).ok_or(123)?;
        let transfer = SporeTransfer::decode(message.payload).ok_or(123)?;
        let input = left.ok_or(123)?;
        if right.is_some()
            || message.receiver != *stem
            || transfer.original_type != *original
            || transfer.recipient == *stem
            || load_cell_data(input, Source::Input).map_err(|_| 123)? != transfer.data
        {
            return Err(123);
        }
    }
    Ok(())
}

fn find(source: Source, target: &Identity) -> Result<Option<usize>, i8> {
    let mut found = None;
    for index in 0.. {
        match load_cell_type_hash(index, source) {
            Ok(Some(ty)) if &ty == target => {
                if found.replace(index).is_some() {
                    return Err(122);
                }
            }
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => return Ok(found),
            Err(_) => return Err(122),
        }
    }
    unreachable!()
}
