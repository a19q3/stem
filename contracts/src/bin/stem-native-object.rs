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
        load_cell_capacity, load_cell_data, load_cell_lock, load_cell_lock_hash,
        load_input_out_point, load_script, load_script_hash,
    },
};
use stem_bridge_wire::{
    intercell::{Identity, MAX_MESSAGE, MESSAGE_HEADER, Message, RECEIVE_ACTION, SEND_ACTION},
    metatype::MAX_STATE_BYTES,
    object::{ObjectArgs, creation_id, revision},
    spore::{
        CellTransfer, CustodyProfile, Handoff, MAX_CELL_SPORE_DATA, MAX_SPORE_DATA, MELT_TAG, MeltReceipt, SporeData, SporeTransfer,
        TransferContent, return_nonce,
    },
};

use stem_contracts::metatype::bounded_data;

entry!(main);
const HEAP_BYTES: usize = 1536 * 1024;
default_alloc!(4096, HEAP_BYTES, 64);

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
        return identity(&object, &network, false);
    }
    if args.len() == 160 {
        let object = ObjectArgs::decode(&args[..128]).ok_or(116)?;
        if args[128..] == [0; 32] {
            return Err(116);
        }
        return identity(&object, &network, true);
    }
    if args.len() == 97
        && let Some(profile) = CustodyProfile::from_identity_tag(args[0])
    {
        let [id, counterpart, route] = args[1..].as_chunks::<32>().0 else {
            return Err(116);
        };
        if [id, counterpart, route].contains(&&[0; 32]) {
            return Err(116);
        }
        return custody_identity(&script, id, counterpart, route, &network, profile);
    }
    let (raw, route) = match (args.first(), args.len()) {
        (Some(&tag), 257) if let Some(profile) = CustodyProfile::from_gate_tag(tag) => (&args[1..], Route::Custody(profile)),
        (Some(1), 257) => (&args[1..], Route::Standard),
        (Some(2), 289) => {
            let authority = (&args[257..]).try_into().map_err(|_| 116)?;
            if authority == &[0; 32] {
                return Err(116);
            }
            (&args[1..257], Route::ApprovedStandard(authority))
        }
        _ => (&args[..], Route::Original),
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
        route,
    )
}

#[derive(Clone, Copy)]
enum Route<'a> {
    Original,
    Standard,
    Custody(CustodyProfile),
    ApprovedStandard(&'a Identity),
}

fn identity(object: &ObjectArgs<'_>, network: &Identity, standard: bool) -> Result<(), i8> {
    let original = load_script_hash().map_err(|_| 116)?;
    let args = [original.as_slice(), object.native_authority].concat();
    let policy = Script::new_builder()
        .code_hash(object.policy_code.pack())
        .hash_type(ScriptHashType::Data)
        .args(args.pack())
        .build()
        .calc_script_hash();
    let limit = if standard {
        MAX_CELL_SPORE_DATA
    } else if object.revisable {
        MAX_STATE_BYTES
    } else {
        MAX_SPORE_DATA
    };
    let valid = |data: &[u8]| {
        if standard {
            SporeData::from_cell(data).is_some_and(|spore| spore.is_plain())
        } else {
            object.valid_state(data)
        }
    };
    let read = |source| -> Result<_, i8> {
        match bounded_data(0, source, limit) {
            Ok(data)
                if valid(&data)
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
        (Some(input), Some(output)) => {
            let permitted = if standard {
                stem_contracts::spore::same_content(&input, &output)
            } else {
                object.permits(&input, &output)
            };
            permitted.then_some(()).ok_or(117)
        }
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

// The native title moves only through the object's pinned custody endpoint.
fn custody_identity(
    script: &Script,
    id: &Identity,
    counterpart: &Identity,
    route: &Identity,
    network: &Identity,
    profile: CustodyProfile,
) -> Result<(), i8> {
    let read = |source| match bounded_data(0, source, MAX_CELL_SPORE_DATA) {
        Ok(data)
            if profile.parse(&data).is_some()
                && load_cell_capacity(1, source) == Err(SysError::IndexOutOfBound)
                && load(LOAD_TYPE_HASH, source as u64, 0, 1, &mut []).0 == INDEX_OUT_OF_BOUND =>
        {
            Ok(Some(data))
        }
        Err(SysError::IndexOutOfBound) => Ok(None),
        _ => Err(117),
    };
    let object = script.calc_script_hash();
    let gate = |source, index| -> Result<Option<Identity>, i8> {
        let lock = load_cell_lock(index, source).map_err(|_| 117)?;
        let args = lock.args().raw_data();
        Ok((lock.code_hash() == script.code_hash()
            && lock.hash_type() == script.hash_type()
            && args.len() == 257
            && args[0] == profile.gate_tag()
            && &args[1..33] == object.as_slice()
            && ckb_hash::blake2b_256(&args[33..225]) == *route
            && &args[225..] == counterpart)
            .then(|| lock.calc_script_hash().unpack()))
    };
    match (read(Source::GroupInput)?, read(Source::GroupOutput)?) {
        (None, Some(_)) => verify_creation(network, id),
        (Some(before), Some(after)) if stem_contracts::spore::same_content(&before, &after) => {
            match (gate(Source::GroupInput, 0)?, gate(Source::GroupOutput, 0)?) {
                (Some(_), None) => Ok(()), // The spending endpoint Lock executes the authenticated return.
                (None, Some(endpoint)) => {
                    // Export must also consume the endpoint bond and execute its outbox rule.
                    for index in 0.. {
                        if load_cell_lock_hash(index, Source::Input).map_err(|_| 117)? == endpoint {
                            return Ok(());
                        }
                    }
                    unreachable!()
                }
                (None, None) => {
                    // The endpoint bond verifies the CKB receipt and this exact successor.
                    for index in 0.. {
                        if gate(Source::Input, index)?.is_some() {
                            return Ok(());
                        }
                    }
                    unreachable!()
                }
                _ => Err(117),
            }
        }
        (Some(_), None) if gate(Source::GroupInput, 0)?.is_some() => Ok(()),
        _ => Err(117),
    }
}

fn plain_identity(id: &Identity, network: &Identity) -> Result<(), i8> {
    let read = |source| match bounded_data(0, source, MAX_CELL_SPORE_DATA) {
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
        (Some((input, _)), Some((output, _)))
            if stem_contracts::spore::same_content(&input, &output) =>
        {
            Ok(())
        }
        (Some((_, false)), None) => Ok(()),
        _ => Err(117),
    }
}

fn escrow(
    script: &Script,
    fields: &[[u8; 32]; 8],
    network: &Identity,
    route: Route<'_>,
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
    let mut owner_args = [type_id.as_slice(), &lock, object, mailbox_code, binding].concat();
    if let Route::ApprovedStandard(authority) = route {
        owner_args.extend_from_slice(authority);
    }
    if let Route::Custody(profile) = route {
        owner_args.insert(0, profile.gate_tag());
    }
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
    let handoff = Handoff::decode(message.payload);
    let melt = MeltReceipt::decode(message.payload);
    let terminal = melt.is_some();
    if terminal && !(kind == RECEIVE_ACTION && matches!(route, Route::Standard | Route::Custody(_))) {
        return Err(118);
    }
    if handoff.is_some() && !(kind == RECEIVE_ACTION && matches!(route, Route::Custody(_))) {
        return Err(118);
    }
    let transfer = handoff
        .as_ref()
        .map(|h| h.transfer)
        .or(melt)
        .or_else(|| {
            if matches!(route, Route::Custody(_)) {
                CellTransfer::decode_custody(message.payload)
            } else {
                CellTransfer::decode(message.payload)
            }
        })
        .ok_or(118)?;
    if matches!(route, Route::Original) && SporeTransfer::decode_plain(message.payload).is_none() {
        return Err(118);
    }
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
        transfer.content,
        route,
    )?;
    let right = inspect(
        Source::Output,
        script,
        object,
        binding,
        &lock,
        transfer.content,
        route,
    )?;
    if terminal {
        let ((Some(bond), Some((capacity, holder, _))), (Some(next_bond), None)) = (left, right)
        else {
            return Err(119);
        };
        if holder != lock || next_bond != bond {
            return Err(119);
        }
        // The native identity independently authorizes destruction. A tagged
        // refund makes each original's collateral independently accountable.
        return melt_refund(object, &transfer.recipient, capacity);
    }
    let (
        (Some(before_bond), Some((before, input_lock, input_index))),
        (Some(after_bond), Some((after, output_lock, output_index))),
    ) = (left, right)
    else {
        return Err(119);
    };
    if before_bond != after_bond || before != after {
        return Err(119);
    }
    if let Some(grant) = handoff {
        return (input_lock != lock
            && &input_lock == grant.previous
            && output_lock == transfer.recipient
            && stem_contracts::native_type::matches_handoff(&grant, input_index, output_index))
        .then_some(())
        .ok_or(119);
    }
    if (exporting && (input_lock == lock || output_lock != lock))
        || (!exporting && (input_lock != lock || output_lock != transfer.recipient))
    {
        return Err(119);
    }
    Ok(())
}

type Assets = (Option<u64>, Option<(u64, Identity, usize)>);

fn melt_refund(object: &Identity, recipient: &Identity, capacity: u64) -> Result<(), i8> {
    let tag = [MELT_TAG.as_slice(), object].concat();
    let mut found = false;
    for index in 0.. {
        match load_cell_lock_hash(index, Source::Output) {
            Ok(lock)
                if &lock == recipient
                    && load(LOAD_TYPE_HASH, Source::Output as u64, index, 0, &mut []).0
                        == INDEX_OUT_OF_BOUND
                    && bounded_data(index, Source::Output, tag.len())
                        .is_ok_and(|bytes| bytes == tag) =>
            {
                if found || load_cell_capacity(index, Source::Output).map_err(|_| 119)? != capacity
                {
                    return Err(119);
                }
                found = true;
            }
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => return found.then_some(()).ok_or(119),
            Err(_) => return Err(119),
        }
    }
    unreachable!()
}

fn inspect(
    source: Source,
    code: &Script,
    object: &Identity,
    binding: &Identity,
    escrow: &Identity,
    content: TransferContent<'_>,
    route: Route<'_>,
) -> Result<Assets, i8> {
    let mut bond = None;
    let mut asset = None;
    for index in 0.. {
        let lock = match load_cell_lock_hash(index, source) {
            Ok(lock) => lock,
            Err(SysError::IndexOutOfBound) => return Ok((bond, asset)),
            Err(_) => return Err(119),
        };
        let mut raw = [0u8; 53 + 5 * 32];
        let (arg_len, binding_offset, type_count) = match route {
            Route::Original => (128, 64, 2),
            Route::Standard => (64, 32, 1),
            Route::Custody(_) => (97, 33, 1),
            Route::ApprovedStandard(_) => (160, 128, 2),
        };
        match load(
            FIND_TYPE,
            source as u64,
            index,
            object.as_ptr() as u64,
            &mut raw,
        ) {
            (0, size) if size == 53 + arg_len => {
                let ty = Script::from_slice(&raw[..size]).map_err(|_| 119)?;
                let args = ty.args().raw_data();
                if ty.code_hash() != code.code_hash()
                    || ty.hash_type() != code.hash_type()
                    || args.get(binding_offset..binding_offset + 32) != Some(binding.as_slice())
                    || matches!(route, Route::ApprovedStandard(authority) if args.get(64..96) != Some(authority.as_slice()))
                    || load(LOAD_TYPE_HASH, source as u64, index, type_count, &mut []).0
                        != INDEX_OUT_OF_BOUND
                    || !stem_contracts::spore::matches_content(index, source, content)
                        .map_err(|_| 119)?
                    || asset
                        .replace((
                            load_cell_capacity(index, source).map_err(|_| 119)?,
                            lock,
                            index,
                        ))
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
