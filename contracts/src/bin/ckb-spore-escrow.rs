//! CKB mailbox Lock that holds and releases the same original Spore V2 Cell.

#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_data, load_cell_data_hash, load_cell_lock,
        load_cell_lock_hash, load_cell_type, load_cell_type_hash, load_input, load_script,
        load_script_hash, load_witness_args,
    },
};
use stem_bridge_wire::{
    intercell::{Delivery, Identity, MailboxPolicy, MailboxState, Message},
    spore::{
        CKB_SPORE_CODE, CellTransfer, Handoff, MAX_CELL_SPORE_DATA, MeltReceipt, SporeData,
        TESTNET_SPORE_BETA2,
    },
};

entry!(main);
const HEAP_BYTES: usize = 1536 * 1024;
default_alloc!(4096, HEAP_BYTES, 64);

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 110)?;
    let args = script.args().raw_data();
    if args.len() == 117 && args[0] == 5 {
        let gate = args[1..33].try_into().map_err(|_| 115)?;
        let spore = args[33..65].try_into().map_err(|_| 115)?;
        if u8::from(script.hash_type()) != ScriptHashType::Data2 as u8
            || load_cell_type_hash(0, Source::GroupInput).map_err(|_| 115)? != Some(spore)
            || load_cell_capacity(1, Source::GroupInput) != Err(SysError::IndexOutOfBound)
        {
            return Err(115);
        }
        return require_input_lock(gate);
    }
    if args.len() == 129 && args[0] == 1 {
        let [spore, authority, gate, holder] = args[1..].as_chunks::<32>().0 else {
            return Err(115);
        };
        return holder_transfer(&script, spore, authority, gate, holder);
    }
    let mutant_origin = args.len() == 161 && args[0] == 3;
    let raw = if mutant_origin { &args[1..] } else { &args[..] };
    let (fields, rest) = raw.as_chunks::<32>();
    if !rest.is_empty()
        || fields.contains(&[0; 32])
        || u8::from(script.hash_type()) != ScriptHashType::Data2 as u8
    {
        return Err(110);
    }
    let (mailbox_id, endpoint, origin, mailbox_code, counterpart, approval) = match fields {
        [id, code] => return reservation(&script, id, code),
        [id, endpoint, original, code] => (id, endpoint, original, code, None, None),
        [id, endpoint, native, code, spore] => (id, endpoint, native, code, Some(spore), None),
        [id, endpoint, native, code, spore, authority] => {
            (id, endpoint, native, code, Some(spore), Some(authority))
        }
        _ => return Err(110),
    };
    let original = counterpart.unwrap_or(origin);
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
    let spore_input = matching(Source::Input, original)?;
    let Some(spore_output) = matching(Source::Output, original)? else {
        // A terminal receipt is available only to the external holder of a
        // native-origin counterpart whose CKB Type accepts this melt.
        let input = spore_input.ok_or(111)?;
        if counterpart.is_none()
            || approval.is_some()
            || load_cell_type_hash(0, Source::GroupInput).map_err(|_| 110)? != Some(mailbox)
            || load_cell_capacity(1, Source::GroupInput) != Err(SysError::IndexOutOfBound)
            || load_cell_lock_hash(input, Source::Input).map_err(|_| 110)? == owner
            || (mutant_origin
                && custody_holder(&script, input, Source::Input, &owner, original)?.is_some())
        {
            return Err(111);
        }
        let ty = load_cell_type(input, Source::Input)
            .map_err(|_| 110)?
            .ok_or(112)?;
        let bytes =
            stem_contracts::metatype::bounded_data(input, Source::Input, MAX_CELL_SPORE_DATA)
                .map_err(|_| 110)?;
        let meltable = if mutant_origin {
            ty.code_hash().as_slice() == TESTNET_SPORE_BETA2
                && SporeData::custody_mutant(&bytes).is_some()
        } else {
            ty.code_hash().as_slice() == CKB_SPORE_CODE
                && SporeData::unclustered(&bytes).is_some_and(|s| !s.immortal())
        };
        if !meltable
            || u8::from(ty.hash_type()) != ScriptHashType::Data1 as u8
            || ty.args().raw_data().len() != 32
        {
            return Err(112);
        }
        let message = Message::decode(next.outgoing).ok_or(113)?;
        let receipt = MeltReceipt::decode(message.payload).ok_or(113)?;
        if message.receiver != *endpoint
            || receipt.original_type != *origin
            || receipt.recipient == *endpoint
            || !receipt
                .content
                .matches(&bytes, || load_cell_data_hash(input, Source::Input).ok())
        {
            return Err(113);
        }
        return Ok(());
    };
    let first_export = spore_input.is_none() && counterpart.is_some();
    let input_index = spore_input.unwrap_or(0);
    let registered_custody = mutant_origin || counterpart.is_none();
    let previous = if registered_custody && !first_export {
        custody_holder(&script, input_index, Source::Input, &owner, original)?
    } else {
        None
    };
    let following = if registered_custody {
        custody_holder(&script, spore_output, Source::Output, &owner, original)?
    } else {
        None
    };
    let held_before = if first_export {
        let expected = Script::new_builder()
            .code_hash(script.code_hash())
            .hash_type(script.hash_type())
            .args([mailbox_id.as_slice(), mailbox_code].concat().pack())
            .build()
            .calc_script_hash();
        if spore_output != 0
            || load_cell_type_hash(0, Source::Input)
                .map_err(|_| 110)?
                .is_some()
            || load_cell_lock_hash(0, Source::Input)
                .map_err(|_| 110)?
                .as_slice()
                != expected.as_slice()
        {
            return Err(111);
        }
        true
    } else {
        previous.is_some()
            || load_cell_lock_hash(spore_input.ok_or(111)?, Source::Input).map_err(|_| 110)?
                == owner
    };
    let after = load_cell_lock_hash(spore_output, Source::Output).map_err(|_| 110)?;
    let held_after = following.is_some() || after == owner;
    if held_before == held_after && !(previous.is_some() && following.is_some()) {
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
    let object_type = load_cell_type(spore_output, Source::Output)
        .map_err(|_| 110)?
        .ok_or(110)?;
    if u8::from(object_type.hash_type()) != ScriptHashType::Data1 as u8
        || object_type.args().raw_data().len() != 32
    {
        return Err(112);
    }
    let output =
        stem_contracts::metatype::bounded_data(spore_output, Source::Output, MAX_CELL_SPORE_DATA)
            .map_err(|_| 110)?;
    let code = object_type.code_hash();
    if (previous.is_some() || following.is_some()) && code.as_slice() != TESTNET_SPORE_BETA2 {
        return Err(112);
    }
    let supported = (!mutant_origin
        && code.as_slice() == CKB_SPORE_CODE
        && SporeData::from_cell(&output)
            .is_some_and(|s| counterpart.is_none() || s.cluster_id.is_none()))
        || ((counterpart.is_none() || mutant_origin)
            && code.as_slice() == TESTNET_SPORE_BETA2
            && SporeData::custody_mutant(&output).is_some());
    if (!first_export
        && (stem_contracts::metatype::cell_data_length(input_index, Source::Input)
            .map_err(|_| 110)?
            != output.len()
            || load_cell_data_hash(input_index, Source::Input).map_err(|_| 110)?
                != load_cell_data_hash(spore_output, Source::Output).map_err(|_| 110)?))
        || !supported
        || load_cell_capacity(input_index, Source::Input).map_err(|_| 110)?
            != load_cell_capacity(spore_output, Source::Output).map_err(|_| 110)?
    {
        return Err(112);
    }
    if let (Some(from), Some(to)) = (&previous, &following) {
        let message = Message::decode(next.outgoing).ok_or(113)?;
        let grant = Handoff::decode(message.payload).ok_or(113)?;
        if message.receiver != *endpoint
            || grant.transfer.original_type != *origin
            || *grant.previous != from.native
            || grant.transfer.recipient != to.native
            || from.native_code != to.native_code
            || !grant.transfer.content.matches(&output, || {
                load_cell_data_hash(spore_output, Source::Output).ok()
            })
        {
            return Err(113);
        }
        return require_input_lock(&from.ckb);
    }
    if held_after {
        let message = Message::decode(next.outgoing).ok_or(113)?;
        if message.receiver != *endpoint {
            return Err(113);
        }
        verify_transfer(
            message.payload,
            origin,
            spore_output,
            &output,
            following.as_ref().map(|holder| &holder.native),
            endpoint,
        )?;
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
        let recipient = if let Some(authority) = approval {
            require_input_lock(authority)?;
            let next = load_cell_lock(spore_output, Source::Output).map_err(|_| 115)?;
            holder_identity(&script, &next, original, authority, &owner)?
        } else {
            after
        };
        verify_transfer(
            message.payload,
            origin,
            spore_output,
            &output,
            Some(&recipient),
            &owner,
        )?;
    }
    Ok(())
}

struct CustodyHolder {
    native_code: Identity,
    native: Identity,
    ckb: Identity,
}

fn custody_holder(
    program: &Script,
    index: usize,
    source: Source,
    gate: &Identity,
    spore: &Identity,
) -> Result<Option<CustodyHolder>, i8> {
    let lock = load_cell_lock(index, source).map_err(|_| 110)?;
    let args = lock.args().raw_data();
    if lock.code_hash() != program.code_hash()
        || lock.hash_type() != program.hash_type()
        || args.len() != 117
        || args[0] != 5
        || &args[1..33] != gate
        || &args[33..65] != spore
    {
        return Ok(None);
    }
    let native_code: Identity = args[65..97].try_into().map_err(|_| 115)?;
    let key = &args[97..];
    if native_code == [0; 32] || key == [0; 20] {
        return Err(115);
    }
    let account = |code: &Identity, kind: ScriptHashType| -> Identity {
        Script::new_builder()
            .code_hash(code.pack())
            .hash_type(kind)
            .args(key.pack())
            .build()
            .calc_script_hash()
            .unpack()
    };
    // CKB's standard secp256k1_blake160_sighash_all Type hash.
    let secp = [
        0x9b, 0xd7, 0xe0, 0x6f, 0x3e, 0xcf, 0x4b, 0xe0, 0xf2, 0xfc, 0xd2, 0x18, 0x8b, 0x23, 0xf1,
        0xb9, 0xfc, 0xc8, 0x8e, 0x5d, 0x4b, 0x65, 0xa8, 0x63, 0x7b, 0x17, 0x72, 0x3b, 0xbd, 0xa3,
        0xcc, 0xe8,
    ];
    Ok(Some(CustodyHolder {
        native_code,
        native: account(&native_code, ScriptHashType::Data),
        ckb: account(&secp, ScriptHashType::Type),
    }))
}

fn require_input_lock(required: &Identity) -> Result<(), i8> {
    for i in 0.. {
        match load_cell_lock_hash(i, Source::Input) {
            Ok(lock) if &lock == required => return Ok(()),
            Ok(_) => {}
            Err(_) => return Err(115),
        }
    }
    unreachable!()
}

fn holder_identity(
    program: &Script,
    output: &Script,
    spore: &Identity,
    authority: &Identity,
    gate: &Identity,
) -> Result<Identity, i8> {
    let args = output.args().raw_data();
    if program.code_hash() != output.code_hash()
        || program.hash_type() != output.hash_type()
        || args.len() != 129
        || args[0] != 1
    {
        return Err(115);
    }
    let [asset, policy, escrow, next] = args[1..].as_chunks::<32>().0 else {
        return Err(115);
    };
    if asset != spore || policy != authority || escrow != gate || next == &[0; 32] {
        return Err(115);
    }
    Ok(*next)
}

// A standard Spore has one Type slot. Its owner Lock carries forward the
// immutable approval policy while its ordinary owner and approver authorize
// the transaction with their own CKB input Locks.
fn holder_transfer(
    program: &Script,
    spore: &Identity,
    authority: &Identity,
    gate: &Identity,
    holder: &Identity,
) -> Result<(), i8> {
    if u8::from(program.hash_type()) != ScriptHashType::Data2 as u8
        || [spore, authority, gate, holder].contains(&&[0; 32])
        || load_cell_type_hash(0, Source::GroupInput).map_err(|_| 115)? != Some(*spore)
        || load_cell_capacity(1, Source::GroupInput) != Err(SysError::IndexOutOfBound)
    {
        return Err(115);
    }
    require_input_lock(holder)?;
    require_input_lock(authority)?;
    let index = matching(Source::Output, spore)?.ok_or(115)?;
    let lock = load_cell_lock(index, Source::Output).map_err(|_| 115)?;
    if lock.calc_script_hash().as_slice() == gate {
        require_input_lock(gate)
    } else {
        holder_identity(program, &lock, spore, authority, gate).map(|_| ())
    }
}

// A reservation uses the unique mailbox creation ID. Its own future OutPoint
// can therefore be used to derive the standard Spore ID without a hash cycle.
fn reservation(script: &Script, id: &Identity, code: &Identity) -> Result<(), i8> {
    let first = load_input(0, Source::Input).map_err(|_| 114)?;
    if first != load_input(0, Source::GroupInput).map_err(|_| 114)?
        || load_cell_capacity(1, Source::GroupInput) != Err(SysError::IndexOutOfBound)
        || load_cell_type_hash(0, Source::Input)
            .map_err(|_| 114)?
            .is_some()
    {
        return Err(114);
    }
    let spore = load_cell_type(0, Source::Output)
        .map_err(|_| 114)?
        .ok_or(114)?;
    let mut hash = ckb_hash::new_blake2b();
    hash.update(first.as_slice());
    hash.update(&0u64.to_le_bytes());
    let mut spore_id = [0; 32];
    hash.finalize(&mut spore_id);
    if u8::from(spore.hash_type()) != ScriptHashType::Data1 as u8
        || spore.args().raw_data().as_ref() != spore_id
    {
        return Err(114);
    }
    let spore_hash = spore.calc_script_hash();
    for index in 1.. {
        let ty = match load_cell_type(index, Source::Input) {
            Ok(Some(ty)) => ty,
            Ok(None) => continue,
            Err(_) => return Err(114),
        };
        if ty.code_hash().as_slice() != code
            || u8::from(ty.hash_type()) != ScriptHashType::Data2 as u8
        {
            continue;
        }
        let lock = load_cell_lock(index, Source::Input).map_err(|_| 114)?;
        let args = lock.args().raw_data();
        let mutant_gate = args.len() == 161 && args[0] == 3;
        let raw = if mutant_gate { &args[1..] } else { &args[..] };
        let (fields, rest) = raw.as_chunks::<32>();
        let expected = if mutant_gate {
            TESTNET_SPORE_BETA2
        } else {
            CKB_SPORE_CODE
        };
        if let [gate_id, _, _, gate_code, counterpart, tail @ ..] = fields
            && tail.len() <= 1
            && rest.is_empty()
            && spore.code_hash().as_slice() == expected
            && gate_id == id
            && gate_code == code
            && counterpart.as_slice() == spore_hash.as_slice()
            && lock.code_hash() == script.code_hash()
            && lock.hash_type() == script.hash_type()
            && MailboxPolicy::decode(&ty.args().raw_data()).is_some_and(|p| {
                p.type_id == *id && p.owner.as_slice() == lock.calc_script_hash().as_slice()
            })
        {
            return Ok(());
        }
    }
    unreachable!()
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
    matching(source, target)?.ok_or(111)
}

fn matching(source: Source, target: &Identity) -> Result<Option<usize>, i8> {
    let mut matching = None;
    for index in 0.. {
        match load_cell_type_hash(index, source) {
            Ok(Some(ty)) if ty == *target => {
                if matching.replace(index).is_some() {
                    return Err(111);
                }
            }
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => return Ok(matching),
            Err(_) => return Err(110),
        }
    }
    unreachable!()
}

fn verify_transfer(
    payload: &[u8],
    original: &Identity,
    output_index: usize,
    data: &[u8],
    recipient: Option<&Identity>,
    custody: &Identity,
) -> Result<(), i8> {
    let transfer = CellTransfer::decode_custody(payload).ok_or(113)?;
    if transfer.original_type != *original
        || !transfer.content.matches(data, || {
            load_cell_data_hash(output_index, Source::Output).ok()
        })
        || &transfer.recipient == custody
        || recipient.is_some_and(|recipient| *recipient != transfer.recipient)
    {
        return Err(113);
    }
    Ok(())
}
