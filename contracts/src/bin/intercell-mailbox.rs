//! A unique mailbox publishes CKB messages and consumes finalized Stem messages.
#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_data, load_cell_lock_hash, load_header, load_script, load_script_hash,
        load_witness_args,
    },
    type_id,
};
use stem_bridge_wire::intercell::{Delivery, MailboxPolicy, MailboxState, Message, record_key};
use stem_contracts::load_checkpoint;
use stem_stake::{
    finality::CheckpointId,
    intercell::{consume, outbox_system_root},
    patricia::{Commitment, Namespace, decode_path},
};

entry!(main);
default_alloc!();

#[repr(i8)]
enum Error {
    Syscall = 80,
    Identity = 81,
    Encoding = 82,
    State = 83,
    Proof = 84,
    Replay = 85,
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
    if u8::from(script.hash_type()) != ScriptHashType::Data2 as u8 {
        return Err(Error::Identity);
    }
    let policy = MailboxPolicy::decode(&script.args().raw_data()).ok_or(Error::Encoding)?;
    type_id::validate_type_id(&policy.type_id)?;
    let mailbox = load_script_hash()?;
    // Always require a same-Type successor, including for an empty Inbox.
    if load_cell_lock_hash(0, Source::GroupOutput)? != policy.owner {
        return Err(Error::Identity);
    }
    let next_bytes = load_cell_data(0, Source::GroupOutput)?;
    let next = MailboxState::decode(&next_bytes).ok_or(Error::Encoding)?;
    let previous_bytes = match load_cell_lock_hash(0, Source::GroupInput) {
        Err(SysError::IndexOutOfBound) if next == MailboxState::default() => {
            let genesis = load_header(0, Source::HeaderDep)?;
            let height: u64 = genesis.raw().number().unpack();
            return if height == 0 && genesis.calc_header_hash().as_slice() == policy.ckb_genesis {
                Ok(())
            } else {
                Err(Error::Identity)
            };
        }
        Ok(owner) if owner == policy.owner => load_cell_data(0, Source::GroupInput)?,
        _ => return Err(Error::Identity),
    };
    let previous = MailboxState::decode(&previous_bytes).ok_or(Error::Encoding)?;
    if !next.outgoing.is_empty() {
        let message = Message::decode(next.outgoing).ok_or(Error::Encoding)?;
        if message.source != policy.ckb_genesis
            || message.destination != policy.stem_network
            || message.sender != mailbox
            || message.nonce != previous.next_nonce
            || next.consumed != previous.consumed
        {
            return Err(Error::State);
        }
        return Ok(());
    }
    if next.next_nonce != previous.next_nonce {
        return Err(Error::State);
    }

    let witness = load_witness_args(0, Source::GroupInput)?;
    let input_type = witness
        .input_type()
        .to_opt()
        .ok_or(Error::Encoding)?
        .raw_data();
    let delivery = Delivery::decode(&input_type).ok_or(Error::Encoding)?;
    let message = Message::decode(delivery.record).ok_or(Error::Encoding)?;
    if message.source != policy.stem_network
        || message.destination != policy.ckb_genesis
        || message.receiver != mailbox
    {
        return Err(Error::Identity);
    }
    let checkpoint = load_checkpoint(
        &CheckpointId {
            code_hash: policy.checkpoint_code,
            type_id: policy.checkpoint_id,
        },
        &policy.ckb_genesis,
        &policy.stem_network,
        None,
    )?
    .ok_or(Error::Proof)?;
    let system_root = outbox_system_root(
        &policy.stem_network,
        &checkpoint.tip.header,
        delivery.history,
    )
    .map_err(|_| Error::Proof)?;
    let id = message.id();
    let commitment = Commitment::new(policy.stem_network, Namespace::System);
    if !commitment.verify(
        &system_root,
        &commitment.record_key(&record_key(false, &id)),
        Some(&commitment.value_hash(delivery.record)),
        decode_path(delivery.outbox)
            .map_err(|_| Error::Proof)?
            .into_iter(),
    ) {
        return Err(Error::Proof);
    }
    let expected =
        consume(&mailbox, &id, &previous.consumed, delivery.replay).map_err(|_| Error::Replay)?;
    if expected != next.consumed {
        return Err(Error::Replay);
    }
    Ok(())
}
