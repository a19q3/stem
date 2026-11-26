//! Native object identity and escrow for the dedicated immutable route.

#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data, load_cell_lock_hash, load_input_out_point, load_script,
        load_script_hash,
    },
};
use stem_bridge_wire::{
    intercell::{Identity, MAX_MESSAGE, MESSAGE_HEADER, Message, RECEIVE_ACTION, SEND_ACTION},
    metatype::MAX_STATE_BYTES,
    object::{ObjectArgs, creation_id, revision},
    spore::{MAX_SPORE_DATA, SporeData, SporeTransfer, return_nonce},
};

use stem_contracts::metatype::bounded_data;

entry!(main);
default_alloc!();

const LOAD_TYPE_HASH: u64 = 2112;
const FIND_TYPE: u64 = 2113;
const LOAD_GROUP_INDEX: u64 = 2115;
const LOAD_NETWORK: u64 = 2116;
const LOAD_ACTION: u64 = 2123;
const INDEX_OUT_OF_BOUND: u64 = 1;
const ITEM_MISSING: u64 = 2;

fn load(number: u64, source: u64, index: usize, arg: u64, data: &mut [u8]) -> (u64, usize) {
    let mut size = data.len() as u64;
    let mut status = data.as_mut_ptr() as u64;
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut size, in("a2") 0u64, in("a3") index as u64,
             in("a4") source, in("a5") arg, in("a7") number);
    }
    (status, size as usize)
}

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 116)?;
    let args = script.args().raw_data();
    if u8::from(script.hash_type()) != ScriptHashType::Data as u8 {
        return Err(116);
    }
    let mut network = [0; 32];
    if load(LOAD_NETWORK, 0, 0, 0, &mut network) != (0, 32) {
        return Err(116);
    }
    if let Some(object) = ObjectArgs::decode(&args) {
        return identity(&object, &network);
    }
    let standard_route = args.len() == 257 && args[0] == 1;
    let raw = if standard_route {
        &args[1..]
    } else {
        &args[..]
    };
    let (fields, rest) = raw.as_chunks::<32>();
    if !rest.is_empty() || fields.contains(&[0; 32]) {
        return Err(116);
    }
    if let [id, _counterpart] = fields {
        return plain_identity(id, &network);
    }
    escrow(
        &script,
        fields.try_into().map_err(|_| 116)?,
        &network,
        standard_route,
    )
}

fn identity(object: &ObjectArgs<'_>, network: &Identity) -> Result<(), i8> {
    let original = load_script_hash().map_err(|_| 116)?;
    let args = [original.as_slice(), object.native_authority].concat();
    let policy = Script::new_builder()
        .code_hash(object.policy_code.pack())
        .hash_type(ScriptHashType::Data)
        .args(args.pack())
        .build()
        .calc_script_hash();
    let limit = if object.revisable {
        MAX_STATE_BYTES
    } else {
        MAX_SPORE_DATA
    };
    let read = |source| -> Result<_, i8> {
        match bounded_data(0, source, limit) {
            Ok(data)
                if object.valid_state(&data)
                    && load_cell_capacity(1, source) == Err(SysError::IndexOutOfBound)
                    && load(
                        FIND_TYPE,
                        source as u64,
                        0,
                        policy.as_slice().as_ptr() as u64,
                        &mut [],
                    )
                    .0 == 0 =>
            {
                Ok(Some(data))
            }
            Err(SysError::IndexOutOfBound) => Ok(None),
            _ => Err(117),
        }
    };
    match (read(Source::GroupInput)?, read(Source::GroupOutput)?) {
        (Some(input), Some(output)) if object.permits(&input, &output) => Ok(()),
        (None, Some(output)) if !object.revisable || revision(&output) == Some(0) => {
            verify_creation(network, object.id)?;
            Ok(())
        }
        _ => Err(117),
    }
}

fn verify_creation(network: &Identity, id: &Identity) -> Result<(), i8> {
    let first = load_input_out_point(0, Source::Input).map_err(|_| 117)?;
    let mut index = [0; 4];
    if load(
        LOAD_GROUP_INDEX,
        Source::GroupOutput as u64,
        0,
        0,
        &mut index,
    ) != (0, 4)
        || creation_id(
            network,
            first.as_slice().try_into().map_err(|_| 117)?,
            u32::from_le_bytes(index),
        ) != *id
    {
        return Err(117);
    }
    Ok(())
}

fn plain_identity(id: &Identity, network: &Identity) -> Result<(), i8> {
    let read = |source| match bounded_data(0, source, MAX_SPORE_DATA) {
        Ok(data)
            if load_cell_capacity(1, source) == Err(SysError::IndexOutOfBound)
                && load(LOAD_TYPE_HASH, source as u64, 0, 1, &mut []).0 == INDEX_OUT_OF_BOUND =>
        {
            let immortal = SporeData::unclustered(&data).ok_or(117)?.immortal();
            Ok(Some((data, immortal)))
        }
        Err(SysError::IndexOutOfBound) => Ok(None),
        _ => Err(117),
    };
    match (read(Source::GroupInput)?, read(Source::GroupOutput)?) {
        (None, Some(_)) => verify_creation(network, id),
        (Some(input), Some(output)) if input == output => Ok(()),
        (Some((_, false)), None) => Ok(()),
        _ => Err(117),
    }
}

fn escrow(
    script: &Script,
    fields: &[[u8; 32]; 8],
    network: &Identity,
    standard_route: bool,
) -> Result<(), i8> {
    let [
        object,
        ckb,
        mailbox_code,
        type_id,
        owner_code,
        checkpoint_code,
        checkpoint_id,
        // Approved objects bind the CKB authority. Plain objects bind the complete CKB Spore Type Script hash.
        binding,
    ] = fields;
    let lock = load_script_hash().map_err(|_| 116)?;
    let owner_args = [type_id.as_slice(), &lock, object, mailbox_code, binding].concat();
    let owner = Script::new_builder()
        .code_hash(owner_code.pack())
        .hash_type(ScriptHashType::Data2)
        .args(owner_args.pack())
        .build()
        .calc_script_hash();
    let mailbox_args = [
        type_id.as_slice(),
        ckb,
        network,
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

    let mut action = [0; 12 + MAX_MESSAGE];
    let (status, len) = load(LOAD_ACTION, 0, 0, 0, &mut action);
    if status != 0
        || !(12 + MESSAGE_HEADER..=action.len()).contains(&len)
        || action[2..6] != 1u32.to_le_bytes()
        || action[8..12] != ((len - 12) as u32).to_le_bytes()
    {
        return Err(118);
    }
    let kind = u16::from_le_bytes(action[6..8].try_into().unwrap());
    let message = Message::decode(&action[12..len]).ok_or(118)?;
    let transfer = if standard_route {
        SporeTransfer::decode(message.payload).filter(|s| s.cluster_id.is_none())
    } else {
        SporeTransfer::decode_plain(message.payload)
    }
    .ok_or(118)?;
    if transfer.original_type != *object {
        return Err(118);
    }
    let exporting = match kind {
        SEND_ACTION
            if message.source == *network
                && message.destination == *ckb
                && message.sender == lock
                && message.receiver.as_slice() == mailbox.as_slice()
                && transfer.recipient.as_slice() != owner.as_slice() =>
        {
            true
        }
        RECEIVE_ACTION
            if message.source == *ckb
                && message.destination == *network
                && message.sender.as_slice() == mailbox.as_slice()
                && message.receiver == lock
                && transfer.recipient != lock =>
        {
            false
        }
        _ => return Err(118),
    };
    if exporting {
        let input = load_input_out_point(0, Source::GroupInput).map_err(|_| 118)?;
        if message.nonce != return_nonce(input.as_slice().try_into().map_err(|_| 118)?) {
            return Err(118);
        }
    }
    let left = inspect(
        Source::Input,
        script,
        object,
        binding,
        &lock,
        transfer.data,
        standard_route,
    )?;
    let right = inspect(
        Source::Output,
        script,
        object,
        binding,
        &lock,
        transfer.data,
        standard_route,
    )?;
    let (
        (Some(before_bond), Some((before, input_lock))),
        (Some(after_bond), Some((after, output_lock))),
    ) = (left, right)
    else {
        return Err(119);
    };
    if before_bond != after_bond
        || before != after
        || (exporting && (input_lock == lock || output_lock != lock))
        || (!exporting && (input_lock != lock || output_lock != transfer.recipient))
    {
        return Err(119);
    }
    Ok(())
}

type Assets = (Option<u64>, Option<(u64, Identity)>);

fn inspect(
    source: Source,
    code: &Script,
    object: &Identity,
    binding: &Identity,
    escrow: &Identity,
    data: &[u8],
    standard_route: bool,
) -> Result<Assets, i8> {
    let mut bond = None;
    let mut asset = None;
    for index in 0.. {
        let lock = match load_cell_lock_hash(index, source) {
            Ok(lock) => lock,
            Err(SysError::IndexOutOfBound) => return Ok((bond, asset)),
            Err(_) => return Err(119),
        };
        let mut raw = [0u8; 53 + 4 * 32];
        match load(
            FIND_TYPE,
            source as u64,
            index,
            object.as_ptr() as u64,
            &mut raw,
        ) {
            (0, size)
                if size
                    == (if standard_route {
                        53 + 2 * 32
                    } else {
                        raw.len()
                    }) =>
            {
                let ty = Script::from_slice(&raw[..size]).map_err(|_| 119)?;
                let args = ty.args().raw_data();
                if ty.code_hash() != code.code_hash()
                    || ty.hash_type() != code.hash_type()
                    || (if standard_route {
                        &args[32..64]
                    } else {
                        &args[64..96]
                    }) != binding
                    || load(
                        LOAD_TYPE_HASH,
                        source as u64,
                        index,
                        if standard_route { 1 } else { 2 },
                        &mut [],
                    )
                    .0 != INDEX_OUT_OF_BOUND
                    || load_cell_data(index, source).map_err(|_| 119)? != data
                    || asset
                        .replace((load_cell_capacity(index, source).map_err(|_| 119)?, lock))
                        .is_some()
                {
                    return Err(119);
                }
            }
            (ITEM_MISSING, _) => {
                if lock == *escrow
                    && (load(LOAD_TYPE_HASH, source as u64, index, 0, &mut []).0
                        != INDEX_OUT_OF_BOUND
                        || !load_cell_data(index, source).map_err(|_| 119)?.is_empty()
                        || bond
                            .replace(load_cell_capacity(index, source).map_err(|_| 119)?)
                            .is_some())
                {
                    return Err(119);
                }
            }
            _ => return Err(119),
        }
    }
    unreachable!()
}
