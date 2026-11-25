//! Stem representation of a single CKB Spore and its Intercell endpoint Lock.

#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data, load_cell_lock, load_cell_lock_hash,
        load_input_out_point, load_script, load_script_hash,
    },
};
use stem_bridge_wire::{
    intercell::{Identity, MAX_MESSAGE, MESSAGE_HEADER, Message, RECEIVE_ACTION, SEND_ACTION},
    spore::{MAX_SPORE_DATA, SporeTransfer, plain_spore, return_nonce},
};

entry!(main);
default_alloc!();

const LOAD_TYPE_HASH: u64 = 2112;
const FIND_TYPE: u64 = 2113;
const LOAD_NETWORK: u64 = 2116;
const LOAD_ACTION: u64 = 2123;
const INDEX_OUT_OF_BOUND: u64 = 1;
const ITEM_MISSING: u64 = 2;

fn load(number: u64, source: u64, index: usize, arg: u64, data: &mut [u8]) -> (u64, usize) {
    let mut size = data.len() as u64;
    let mut status = data.as_mut_ptr() as u64;
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut size, in("a2") 0u64,
             in("a3") index as u64, in("a4") source, in("a5") arg, in("a7") number);
    }
    (status, size as usize)
}

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 106)?;
    if u8::from(script.hash_type()) != ScriptHashType::Data as u8 {
        return Err(106);
    }
    let args = script.args().raw_data();
    let (fields, rest) = args.as_chunks::<32>();
    if !rest.is_empty() || fields.contains(&[0; 32]) {
        return Err(106);
    }
    match fields {
        [endpoint] => object_type(&script, endpoint),
        fields => endpoint_lock(&script, fields.try_into().map_err(|_| 106)?),
    }
}

fn object_type(script: &Script, endpoint: &Identity) -> Result<(), i8> {
    let read = |source| -> Result<_, i8> {
        match load_cell_data(0, source) {
            Ok(data)
                if plain_spore(&data)
                    && load_cell_capacity(1, source) == Err(SysError::IndexOutOfBound) =>
            {
                Ok(Some(data))
            }
            Err(SysError::IndexOutOfBound) => Ok(None),
            _ => Err(107),
        }
    };
    match (read(Source::GroupInput)?, read(Source::GroupOutput)?) {
        (Some(input), Some(output)) if input == output => Ok(()),
        (Some(_), None) | (None, Some(_)) => {
            // Only an actual consuming Lock group running this same program can
            // authenticate a mint or return for this representation.
            for index in 0.. {
                match load_cell_lock(index, Source::Input) {
                    Ok(lock) if lock.calc_script_hash().as_slice() == endpoint => {
                        return (lock.code_hash() == script.code_hash()
                            && lock.hash_type() == script.hash_type()
                            && lock.args().raw_data().len() == 7 * 32)
                            .then_some(())
                            .ok_or(107);
                    }
                    Ok(_) => {}
                    Err(_) => return Err(107),
                }
            }
            unreachable!()
        }
        _ => Err(107),
    }
}

fn endpoint_lock(script: &Script, fields: &[[u8; 32]; 7]) -> Result<(), i8> {
    let [
        original,
        ckb,
        mailbox_code,
        type_id,
        owner_code,
        checkpoint_code,
        checkpoint_id,
    ] = fields;
    let mut network = [0; 32];
    if load(LOAD_NETWORK, 0, 0, 0, &mut network) != (0, 32) {
        return Err(106);
    }
    let endpoint = load_script_hash().map_err(|_| 106)?;
    let owner_args = [type_id.as_slice(), &endpoint, original, mailbox_code].concat();
    let owner = Script::new_builder()
        .code_hash(owner_code.pack())
        .hash_type(ScriptHashType::Data2)
        .args(owner_args.pack())
        .build()
        .calc_script_hash();
    let mailbox_args = [
        type_id.as_slice(),
        ckb,
        &network,
        owner.as_slice(),
        checkpoint_code,
        checkpoint_id,
    ]
    .concat();
    let mailbox = Script::new_builder()
        .code_hash(mailbox_code.pack())
        .hash_type(ScriptHashType::Data2)
        .args(mailbox_args.pack())
        .build()
        .calc_script_hash();
    let representation: Identity = script
        .clone()
        .as_builder()
        .args(endpoint.as_slice().pack())
        .build()
        .calc_script_hash()
        .unpack();

    let mut action = [0; 12 + MAX_MESSAGE];
    let (status, size) = load(LOAD_ACTION, 0, 0, 0, &mut action);
    if status != 0
        || !(12 + MESSAGE_HEADER..=action.len()).contains(&size)
        || action[2..6] != 1u32.to_le_bytes()
        || action[8..12] != ((size - 12) as u32).to_le_bytes()
    {
        return Err(108);
    }
    let kind = u16::from_le_bytes(action[6..8].try_into().unwrap());
    let message = Message::decode(&action[12..size]).ok_or(108)?;
    let transfer = SporeTransfer::decode(message.payload).ok_or(108)?;
    if transfer.original_type != *original {
        return Err(108);
    }
    let importing = match kind {
        RECEIVE_ACTION
            if message.source == *ckb
                && message.destination == network
                && message.sender.as_slice() == mailbox.as_slice()
                && message.receiver == endpoint
                && transfer.recipient != endpoint =>
        {
            true
        }
        SEND_ACTION
            if message.source == network
                && message.destination == *ckb
                && message.sender == endpoint
                && message.receiver.as_slice() == mailbox.as_slice() =>
        {
            false
        }
        _ => return Err(108),
    };
    if !importing {
        let bond = load_input_out_point(0, Source::GroupInput).map_err(|_| 108)?;
        if message.nonce != return_nonce(bond.as_slice().try_into().map_err(|_| 108)?) {
            return Err(108);
        }
    }
    let left = inspect(Source::Input, &endpoint, &representation, &transfer)?;
    let right = inspect(Source::Output, &endpoint, &representation, &transfer)?;
    if left.bond.is_none()
        || left.bond != right.bond
        || left.object == right.object
        || right.object != importing
    {
        return Err(109);
    }
    Ok(())
}

#[derive(Default)]
struct State {
    bond: Option<u64>,
    object: bool,
}

fn inspect(
    source: Source,
    endpoint: &Identity,
    representation: &Identity,
    transfer: &SporeTransfer<'_>,
) -> Result<State, i8> {
    let mut state = State::default();
    for index in 0.. {
        let lock = match load_cell_lock_hash(index, source) {
            Ok(lock) => lock,
            Err(SysError::IndexOutOfBound) => return Ok(state),
            Err(_) => return Err(109),
        };
        let source_id = source as u64;
        if lock == *endpoint {
            let cap = load_cell_capacity(index, source).map_err(|_| 109)?;
            if state.bond.replace(cap).is_some()
                || load(LOAD_TYPE_HASH, source_id, index, 0, &mut []).0 != INDEX_OUT_OF_BOUND
                || !load_cell_data(index, source).map_err(|_| 109)?.is_empty()
            {
                return Err(109);
            }
            continue;
        }
        match load(
            FIND_TYPE,
            source_id,
            index,
            representation.as_ptr() as u64,
            &mut [],
        )
        .0
        {
            ITEM_MISSING => continue,
            0 => {}
            _ => return Err(109),
        }
        if state.object
            || load(LOAD_TYPE_HASH, source_id, index, 1, &mut []).0 != INDEX_OUT_OF_BOUND
            || (source == Source::Output && lock != transfer.recipient)
        {
            return Err(109);
        }
        let data = load_cell_data(index, source).map_err(|_| 109)?;
        if data.len() > MAX_SPORE_DATA || data.as_slice() != transfer.data {
            return Err(109);
        }
        state.object = true;
    }
    unreachable!()
}
