//! Escrow native UDT principal against a single asset-restricted CKB mailbox.

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
    asset::{AssetTransfer, STEM_ESCROW_FIELDS, export_nonce},
    intercell::{Identity, MESSAGE_HEADER, Message, RECEIVE_ACTION, SEND_ACTION},
};

entry!(main);
default_alloc!();

fn load(number: u64, source: u64, index: u64, arg: u64, data: &mut [u8]) -> (u64, usize) {
    let mut size = data.len() as u64;
    let mut status = data.as_mut_ptr() as u64;
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut size, in("a2") 0u64,
             in("a3") index, in("a4") source, in("a5") arg, in("a7") number);
    }
    (status, size as usize)
}

fn mailbox(
    fields: &[[u8; 32]; STEM_ESCROW_FIELDS],
    stem: &Identity,
    escrow: &Identity,
) -> (Identity, Identity) {
    let [
        asset,
        ckb,
        mailbox_code,
        mailbox_id,
        wrapped_code,
        checkpoint_code,
        checkpoint,
    ] = fields;
    let owner_args = [mailbox_id.as_slice(), escrow, asset, mailbox_code].concat();
    let owner = Script::new_builder()
        .code_hash(wrapped_code.pack())
        .hash_type(ScriptHashType::Data2)
        .args(owner_args.pack())
        .build()
        .calc_script_hash();
    let args = [
        mailbox_id.as_slice(),
        ckb,
        stem,
        owner.as_slice(),
        checkpoint_code,
        checkpoint,
    ]
    .concat();
    let mailbox = Script::new_builder()
        .code_hash(mailbox_code.pack())
        .hash_type(ScriptHashType::Data2)
        .args(args.pack())
        .build()
        .calc_script_hash();
    (mailbox.unpack(), owner.unpack())
}

#[derive(Default)]
struct Totals {
    capacity: u64,
    held: u128,
    outside: u128,
}

impl Totals {
    fn read(
        source: Source,
        escrow: &Identity,
        asset: &Identity,
        recipient: Option<&Identity>,
    ) -> Option<Self> {
        let mut result = Self::default();
        for index in 0.. {
            let lock = match load_cell_lock_hash(index, source) {
                Ok(lock) => lock,
                Err(SysError::IndexOutOfBound) => return Some(result),
                Err(_) => return None,
            };
            let held = &lock == escrow;
            if held {
                result.capacity = result
                    .capacity
                    .checked_add(load_cell_capacity(index, source).ok()?)?;
            }
            let mut first = [0; 32];
            let (first_status, size) = load(2112, source as u64, index as u64, 0, &mut first);
            if first_status == 1 {
                // An empty escrow Cell can carry the fixed CKB bond between deposits.
                continue;
            }
            if first_status != 0 || size != 32 {
                return None;
            }
            if &first != asset {
                if held
                    || load(
                        2113,
                        source as u64,
                        index as u64,
                        asset.as_ptr() as u64,
                        &mut [],
                    )
                    .0 != 2
                {
                    return None;
                }
                continue;
            }
            if load(2112, source as u64, index as u64, 1, &mut []).0 != 1 {
                return None;
            }
            let raw = load_cell_data(index, source).ok()?;
            let units = u128::from_le_bytes(raw.as_slice().try_into().ok()?);
            if units == 0 {
                return None;
            }
            if held {
                result.held = result.held.checked_add(units)?;
            } else {
                if recipient.is_some_and(|recipient| *recipient != lock) {
                    return None;
                }
                result.outside = result.outside.checked_add(units)?;
            }
        }
        unreachable!()
    }
}

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 95)?;
    let args = script.args().raw_data();
    let (fields, rest) = args.as_chunks::<32>();
    let fields: &[[u8; 32]; STEM_ESCROW_FIELDS] = fields.try_into().map_err(|_| 95)?;
    let mut stem = [0; 32];
    if !rest.is_empty() || fields.contains(&[0; 32]) || load(2116, 0, 0, 0, &mut stem) != (0, 32) {
        return Err(95);
    }
    let escrow = load_script_hash().map_err(|_| 95)?;
    let (remote, remote_owner) = mailbox(fields, &stem, &escrow);
    // A single Group Action has a two-byte group index, a four-byte
    // action count, a two-byte kind and a four-byte data length.
    let mut wire = [0; 12 + MESSAGE_HEADER + AssetTransfer::SIZE];
    if load(2123, 0, 0, 0, &mut wire) != (0, wire.len())
        || wire[2..6] != 1u32.to_le_bytes()
        || wire[8..12] != ((MESSAGE_HEADER + AssetTransfer::SIZE) as u32).to_le_bytes()
    {
        return Err(96);
    }
    let kind = u16::from_le_bytes(wire[6..8].try_into().unwrap());
    let message = Message::decode(&wire[12..]).ok_or(96)?;
    let transfer = AssetTransfer::decode(message.payload).ok_or(96)?;
    let asset = &fields[0];
    let ckb = &fields[1];
    if transfer.asset != *asset {
        return Err(97);
    }
    let export = match kind {
        SEND_ACTION
            if message.source == stem
                && message.destination == *ckb
                && message.sender == escrow
                && message.receiver == remote
                && transfer.recipient != remote_owner =>
        {
            true
        }
        RECEIVE_ACTION
            if message.source == *ckb
                && message.destination == stem
                && message.sender == remote
                && message.receiver == escrow
                && transfer.recipient != escrow =>
        {
            false
        }
        _ => return Err(97),
    };
    if export {
        let input = load_input_out_point(0, Source::GroupInput).map_err(|_| 97)?;
        if message.nonce != export_nonce(input.as_slice().try_into().map_err(|_| 97)?) {
            return Err(97);
        }
    }
    let input = Totals::read(Source::Input, &escrow, asset, None).ok_or(98)?;
    let output = Totals::read(
        Source::Output,
        &escrow,
        asset,
        (!export).then_some(&transfer.recipient),
    )
    .ok_or(98)?;
    if input.capacity != output.capacity {
        return Err(98);
    }
    let valid = if export {
        output.held.checked_sub(input.held) == Some(transfer.amount)
            && input.outside.checked_sub(output.outside) == Some(transfer.amount)
    } else {
        input.outside == 0
            && input.held.checked_sub(output.held) == Some(transfer.amount)
            && output.outside == transfer.amount
    };
    valid.then_some(()).ok_or(98)
}
