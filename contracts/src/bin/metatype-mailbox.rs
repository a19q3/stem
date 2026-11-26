//! The same custody gate for any approved Metatype deployment.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_lock_hash, load_cell_type, load_script, load_script_hash,
        load_witness_args,
    },
};
use stem_bridge_wire::{
    intercell::{Delivery, MailboxState, Message},
    metatype::{
        MAX_LIVE_DATA,
        custody::{Boundary, Gate, Transfer},
    },
};
use stem_contracts::{
    metatype::{ManifestResolver, bounded_data, views},
    metatype_custody::{find_unique, mailbox_policy},
};
use stem_stake::Hash32;

entry!(main);
default_alloc!();

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 50)?;
    if u8::from(script.hash_type()) != ScriptHashType::Data2 as u8
        || load_cell_capacity(1, Source::GroupInput) != Err(SysError::IndexOutOfBound)
    {
        return Err(50);
    }
    let gate = Gate::decode(&script.args().raw_data(), script.code_hash().unpack()).ok_or(50)?;
    let mailbox = load_cell_type(0, Source::GroupInput)
        .map_err(|_| 50)?
        .ok_or(50)?;
    let policy = mailbox_policy(&mailbox, &gate.custody).ok_or(50)?;
    let self_hash = load_script_hash().map_err(|_| 50)?;
    if policy.owner != self_hash {
        return Err(50);
    }
    let mailbox_hash: Hash32 = mailbox.calc_script_hash().unpack();
    let successor = find_unique(Source::Output, &mailbox_hash)?.ok_or(50)?;
    if load_cell_lock_hash(successor, Source::Output).map_err(|_| 50)? != self_hash
        || load_cell_capacity(successor, Source::Output).map_err(|_| 50)?
            != load_cell_capacity(0, Source::GroupInput).map_err(|_| 50)?
    {
        return Err(50);
    }
    let next = bounded_data(
        successor,
        Source::Output,
        MailboxState::HEADER + stem_bridge_wire::intercell::MAX_MESSAGE,
    )
    .map_err(|_| 50)?;
    let next = MailboxState::decode(&next).ok_or(50)?;
    let mut manifests = ManifestResolver::default();
    let mut remaining = MAX_LIVE_DATA;
    let (_, inputs) = manifests
        .cells(Source::Input, &gate.metatype, &mut remaining)
        .map_err(|_| 51)?;
    let (_, outputs) = manifests
        .cells(Source::Output, &gate.metatype, &mut remaining)
        .map_err(|_| 51)?;
    let (boundary, principal) = gate
        .custody
        .transition(&views(&inputs).ok_or(51)?, &views(&outputs).ok_or(51)?)
        .ok_or(51)?;
    let incoming;
    let message = if next.outgoing.is_empty() {
        if boundary != Boundary::Import {
            return Err(51);
        }
        incoming = load_witness_args(0, Source::GroupInput)
            .map_err(|_| 51)?
            .input_type()
            .to_opt()
            .ok_or(51)?
            .raw_data();
        let delivery = Delivery::decode(&incoming).ok_or(51)?;
        let message = Message::decode(delivery.record).ok_or(51)?;
        if message.sender != gate.stem_escrow
            || message.source != policy.stem_network
            || message.receiver != mailbox_hash
            || message.destination != policy.ckb_genesis
        {
            return Err(51);
        }
        message
    } else {
        if boundary != Boundary::Return {
            return Err(51);
        }
        let message = Message::decode(next.outgoing).ok_or(51)?;
        if message.receiver != gate.stem_escrow
            || message.destination != policy.stem_network
            || message.sender != mailbox_hash
            || message.source != policy.ckb_genesis
        {
            return Err(51);
        }
        message
    };
    let transfer = Transfer::decode(gate.custody.profile, message.payload).ok_or(51)?;
    if transfer.manifest != gate.custody.manifest || transfer.principal != principal {
        return Err(51);
    }
    if boundary == Boundary::Import {
        if transfer.recipient == self_hash {
            return Err(51);
        }
        for (index, _) in outputs {
            if load_cell_lock_hash(usize::from(index), Source::Output).map_err(|_| 51)?
                != transfer.recipient
            {
                return Err(51);
            }
        }
    } else if transfer.recipient == gate.stem_escrow {
        return Err(51);
    }
    Ok(())
}
