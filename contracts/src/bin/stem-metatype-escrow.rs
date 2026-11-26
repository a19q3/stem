//! Stem custody of the exact Type Script set and immutable data of a Cell.

#![no_std]
#![no_main]

use alloc::vec::Vec;
use core::arch::asm;

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_lock_hash, load_input_out_point, load_script,
        load_script_hash,
    },
};
use stem_bridge_wire::{
    intercell::{Identity, MAX_MESSAGE, MESSAGE_HEADER, Message, RECEIVE_ACTION, SEND_ACTION},
    metatype::{
        LogicalScript, MAX_SCRIPT_BYTES, MAX_TRANSACTION_CELLS_PER_SIDE, MAX_TYPE_SCRIPTS,
        custody::{Custody, Gate, ImmutableTransfer, escrow_nonce, manifest_id},
    },
};
use stem_contracts::metatype::bounded_data;

entry!(main);
default_alloc!();

const LOAD_TYPE: u64 = 2111;
const FIND_TYPE: u64 = 2113;
const LOAD_ACTION: u64 = 2123;
const LOAD_NETWORK: u64 = 2116;
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
    let script = load_script().map_err(|_| 52)?;
    if u8::from(script.hash_type()) != ScriptHashType::Data as u8 {
        return Err(52);
    }
    let args = script.args().raw_data();
    let (fields, tail) = args.as_chunks::<32>();
    let [ckb, metatype, checkpoint_code, checkpoint_id, custody @ ..] = fields else {
        return Err(52);
    };
    if !tail.is_empty() || fields.contains(&[0; 32]) {
        return Err(52);
    }
    let custody = Custody::decode(custody.as_flattened()).ok_or(52)?;
    let mut network = [0; 32];
    if load(LOAD_NETWORK, 0, 0, 0, &mut network) != (0, 32) {
        return Err(52);
    }
    let escrow = load_script_hash().map_err(|_| 52)?;
    let gate = Gate {
        stem_escrow: escrow,
        metatype: *metatype,
        custody,
    };
    let owner = direct_script(custody.guard_code, &gate.args().as_flattened()).calc_script_hash();
    let mailbox_args = [
        custody.mailbox_id,
        *ckb,
        network,
        owner.unpack(),
        *checkpoint_code,
        *checkpoint_id,
    ];
    let mailbox =
        direct_script(custody.mailbox_code, mailbox_args.as_flattened()).calc_script_hash();
    let mailbox: Identity = mailbox.unpack();

    let mut action = [0; 12 + MAX_MESSAGE];
    let (status, len) = load(LOAD_ACTION, 0, 0, 0, &mut action);
    if status != 0
        || !(12 + MESSAGE_HEADER..=action.len()).contains(&len)
        || action[2..6] != 1u32.to_le_bytes()
        || action[8..12] != ((len - 12) as u32).to_le_bytes()
    {
        return Err(53);
    }
    let kind = u16::from_le_bytes(action[6..8].try_into().expect("fixed action"));
    let message = Message::decode(&action[12..len]).ok_or(53)?;
    let asset = ImmutableTransfer::decode(message.payload).ok_or(53)?;
    if asset.manifest != custody.manifest {
        return Err(53);
    }
    let outbound = match kind {
        SEND_ACTION
            if message.source == network
                && message.destination == *ckb
                && message.sender == escrow
                && message.receiver == mailbox
                && asset.recipient.as_slice() != owner.as_slice() =>
        {
            true
        }
        RECEIVE_ACTION
            if message.source == *ckb
                && message.destination == network
                && message.sender == mailbox
                && message.receiver == escrow
                && asset.recipient != escrow =>
        {
            false
        }
        _ => return Err(53),
    };
    let before = inspect(Source::Input, &custody, &escrow, asset.data)?;
    let after = inspect(Source::Output, &custody, &escrow, asset.data)?;
    if outbound {
        let seed = load_input_out_point(before.bond_index, Source::Input).map_err(|_| 53)?;
        if message.nonce != escrow_nonce(seed.as_slice().try_into().map_err(|_| 53)?) {
            return Err(53);
        }
    }
    if before.bond_capacity != after.bond_capacity
        || before.asset_capacity != after.asset_capacity
        || (outbound && (before.asset_lock == escrow || after.asset_lock != escrow))
        || (!outbound && (before.asset_lock != escrow || after.asset_lock != asset.recipient))
    {
        return Err(54);
    }
    Ok(())
}

fn direct_script(code_hash: Identity, args: &[u8]) -> Script {
    Script::new_builder()
        .code_hash(code_hash.pack())
        .hash_type(ScriptHashType::Data2)
        .args(args.pack())
        .build()
}

struct Assets {
    bond_index: usize,
    bond_capacity: u64,
    asset_capacity: u64,
    asset_lock: Identity,
}

fn inspect(
    source: Source,
    custody: &Custody,
    escrow: &Identity,
    data: &[u8],
) -> Result<Assets, i8> {
    let mut bond = None;
    let mut asset = None;
    for index in 0..=MAX_TRANSACTION_CELLS_PER_SIDE {
        let lock = match load_cell_lock_hash(index, source) {
            Ok(lock) if index < MAX_TRANSACTION_CELLS_PER_SIDE => lock,
            Err(SysError::IndexOutOfBound) => break,
            _ => return Err(54),
        };
        match load(
            FIND_TYPE,
            source as u64,
            index,
            custody.data_script.as_ptr() as u64,
            &mut [],
        )
        .0
        {
            0 => {
                if scripts(source, index)? != custody.manifest
                    || bounded_data(index, source, data.len()).map_err(|_| 54)? != data
                    || asset
                        .replace((load_cell_capacity(index, source).map_err(|_| 54)?, lock))
                        .is_some()
                {
                    return Err(54);
                }
            }
            ITEM_MISSING if lock == *escrow => {
                if load(LOAD_TYPE, source as u64, index, 0, &mut []).0 != INDEX_OUT_OF_BOUND
                    || !bounded_data(index, source, 0).map_err(|_| 54)?.is_empty()
                    || bond
                        .replace((index, load_cell_capacity(index, source).map_err(|_| 54)?))
                        .is_some()
                {
                    return Err(54);
                }
            }
            ITEM_MISSING => (),
            _ => return Err(54),
        }
    }
    let (bond_index, bond_capacity) = bond.ok_or(54)?;
    let (asset_capacity, asset_lock) = asset.ok_or(54)?;
    Ok(Assets {
        bond_index,
        bond_capacity,
        asset_capacity,
        asset_lock,
    })
}

fn scripts(source: Source, index: usize) -> Result<Identity, i8> {
    let mut ordered = Vec::new();
    let mut bytes = [0; MAX_SCRIPT_BYTES];
    for item in 0..=MAX_TYPE_SCRIPTS {
        match load(LOAD_TYPE, source as u64, index, item as u64, &mut bytes) {
            (0, len) if item < MAX_TYPE_SCRIPTS => {
                let raw = bytes.get(..len).ok_or(54)?;
                let script = LogicalScript::decode(raw).ok_or(54)?;
                if script.code().1 != 0 {
                    return Err(54);
                }
                ordered.push(raw.to_vec());
            }
            (INDEX_OUT_OF_BOUND, _) => break,
            _ => return Err(54),
        }
    }
    ordered.sort_unstable();
    let ids = ordered
        .iter()
        .map(|raw| ckb_hash::blake2b_256(raw))
        .collect::<Vec<_>>();
    Ok(manifest_id(&ids))
}
