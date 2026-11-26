//! The same custody gate for any approved immutable Metatype deployment.

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
    metatype::custody::{Gate, ImmutableTransfer},
};
use stem_contracts::{
    metatype::{ManifestResolver, bounded_data},
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
    let input = find_unique(Source::Input, &gate.metatype)?;
    let output = find_unique(Source::Output, &gate.metatype)?;
    let (source, index, manifest, content) = if next.outgoing.is_empty() {
        let (None, Some(index)) = (input, output) else {
            return Err(51);
        };
        let witness = load_witness_args(0, Source::GroupInput).map_err(|_| 51)?;
        let delivery = witness.input_type().to_opt().ok_or(51)?.raw_data();
        let delivery = Delivery::decode(&delivery).ok_or(51)?;
        let message = Message::decode(delivery.record).ok_or(51)?;
        let asset = ImmutableTransfer::decode(message.payload).ok_or(51)?;
        if message.sender != gate.stem_escrow
            || message.source != policy.stem_network
            || message.receiver != mailbox_hash
            || message.destination != policy.ckb_genesis
            || asset.recipient == self_hash
            || load_cell_lock_hash(index, Source::Output).map_err(|_| 51)? != asset.recipient
        {
            return Err(51);
        }
        (Source::Output, index, asset.manifest, asset.data.to_vec())
    } else {
        let (Some(index), None) = (input, output) else {
            return Err(51);
        };
        let message = Message::decode(next.outgoing).ok_or(51)?;
        let asset = ImmutableTransfer::decode(message.payload).ok_or(51)?;
        if message.receiver != gate.stem_escrow
            || message.destination != policy.stem_network
            || message.sender != mailbox_hash
            || message.source != policy.ckb_genesis
            || asset.recipient == gate.stem_escrow
        {
            return Err(51);
        }
        (Source::Input, index, asset.manifest, asset.data.to_vec())
    };
    let state = ManifestResolver::default()
        .manifest_data(index as u16, source)
        .ok_or(51)?;
    if manifest != gate.custody.manifest || gate.custody.data(&state) != Some(content.as_slice()) {
        return Err(51);
    }
    Ok(())
}
