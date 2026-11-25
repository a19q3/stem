#![no_std]
#![no_main]

use ckb_hash::new_blake2b;
use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    default_alloc, entry,
    error::SysError,
    high_level::{
        load_cell_capacity, load_cell_lock, load_cell_lock_hash, load_cell_type,
        load_cell_type_hash, load_script, load_script_hash,
    },
    syscalls, type_id,
};
use fern_bridge_wire::{self as wire, Asset, Gate};

entry!(main);
default_alloc!();

const EMPTY: [u8; 32] = [0; 32];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Consolidate = 0,
    Release = 1,
    Rotate = 2,
    SendReserve = 3,
    ReceiveReserve = 4,
}

struct State {
    action: Action,
    next: u64,
    deposit_code: [u8; 32],
    multisig_code: [u8; 32],
    amount: u64,
    payout_lock: [u8; 32],
    burn_id: [u8; 32],
    asset: Option<Asset>,
    units: u128,
}

#[derive(Default)]
struct Assets {
    deposit_capacity: u64,
    deposited_units: u128,
    escrow_capacity: u64,
    escrow_units: u128,
}

#[derive(Debug)]
enum Error {
    Syscall = 10,
    Encoding = 11,
    Identity = 12,
    Rotation = 13,
    Accounting = 14,
    Payout = 15,
}

impl From<SysError> for Error {
    fn from(_: SysError) -> Self {
        Self::Syscall
    }
}

impl State {
    fn load(source: Source) -> Result<Self, Error> {
        Self::at(0, source)
    }

    fn at(index: usize, source: Source) -> Result<Self, Error> {
        let mut data = [0; wire::XUDT_VAULT_SIZE];
        let len = syscalls::load_cell_data(&mut data, 0, index, source)?;
        let asset = match (data[0], len) {
            (1, wire::NATIVE_VAULT_SIZE) => None,
            (2, wire::XUDT_VAULT_SIZE) => Some(wire::vault_asset(&data).ok_or(Error::Encoding)?),
            _ => return Err(Error::Encoding),
        };
        let action = match data[1] {
            0 => Action::Consolidate,
            1 => Action::Release,
            2 => Action::Rotate,
            3 => Action::SendReserve,
            4 => Action::ReceiveReserve,
            _ => return Err(Error::Encoding),
        };
        Ok(Self {
            action,
            next: u64::from_le_bytes(data[2..10].try_into().unwrap()),
            deposit_code: data[10..42].try_into().unwrap(),
            multisig_code: data[42..74].try_into().unwrap(),
            amount: u64::from_le_bytes(data[74..82].try_into().unwrap()),
            payout_lock: data[82..114].try_into().unwrap(),
            burn_id: data[114..146].try_into().unwrap(),
            asset,
            units: wire::amount(&data[wire::PAYOUT_UNITS_OFFSET..]).unwrap(),
        })
    }

    fn no_payout(&self) -> bool {
        self.amount == 0 && self.units == 0 && self.payout_lock == EMPTY && self.burn_id == EMPTY
    }
}

fn main() -> i8 {
    match verify() {
        Ok(()) => 0,
        Err(error) => error as i8,
    }
}

fn verify() -> Result<(), Error> {
    if load_script()?.args().raw_data().len() != 32 {
        return Err(Error::Identity);
    }
    type_id::check_type_id(0, 32)?;

    // Requiring a group output also rejects destruction of the unique vault.
    let next = State::load(Source::GroupOutput)?;
    let output_lock = load_cell_lock(0, Source::GroupOutput)?;
    if output_lock.code_hash().as_slice() != next.multisig_code
        || u8::from(output_lock.hash_type()) != ScriptHashType::Type as u8
        || output_lock.args().raw_data().len() != 20
        || next.deposit_code == EMPTY
        || next.multisig_code == EMPTY
    {
        return Err(Error::Rotation);
    }
    let before = match load_cell_capacity(0, Source::GroupInput) {
        Ok(capacity) => capacity,
        Err(SysError::IndexOutOfBound) => {
            return if next.next == 0 && next.action == Action::Consolidate && next.no_payout() {
                Ok(())
            } else {
                Err(Error::Identity)
            };
        }
        Err(error) => return Err(error.into()),
    };
    let previous = State::load(Source::GroupInput)?;
    if previous.deposit_code != next.deposit_code
        || previous.multisig_code != next.multisig_code
        || previous.asset != next.asset
    {
        return Err(Error::Identity);
    }
    let after = load_cell_capacity(0, Source::GroupOutput)?;
    let same_lock = load_cell_lock(0, Source::GroupInput)? == output_lock;
    let native_deposits = deposit_capacity(&next)?;
    if next.asset.is_some() && native_deposits != 0 {
        return Err(Error::Accounting);
    }
    let (assets_in, assets_out) = match &next.asset {
        Some(asset) => {
            let vault = load_script_hash()?;
            let assets_in = scan_assets(asset, &vault, Source::Input)?;
            let assets_out = scan_assets(asset, &vault, Source::Output)?;
            if assets_in.escrow_capacity != assets_out.escrow_capacity {
                return Err(Error::Accounting);
            }
            (assets_in, assets_out)
        }
        None => (Assets::default(), Assets::default()),
    };
    let deposits = native_deposits
        .checked_add(assets_in.deposit_capacity)
        .ok_or(Error::Accounting)?;
    match next.action {
        Action::Consolidate => {
            if !same_lock || next.next != previous.next || !next.no_payout() {
                return Err(Error::Identity);
            }
            if deposits == 0
                || before.checked_add(deposits) != Some(after)
                || assets_in
                    .escrow_units
                    .checked_add(assets_in.deposited_units)
                    != Some(assets_out.escrow_units)
            {
                return Err(Error::Accounting);
            }
        }
        Action::Release => {
            if !same_lock
                || deposits != 0
                || previous.next.checked_add(1) != Some(next.next)
                || next.amount == 0
                || next.burn_id != withdrawal_id(previous.next)?
                || before.checked_sub(next.amount) != Some(after)
                || assets_in.escrow_units.checked_sub(next.units) != Some(assets_out.escrow_units)
            {
                return Err(Error::Accounting);
            }
            // Output zero is the beneficiary. Token escrow and native capacity
            // must move by exactly the amounts in the authenticated vault state.
            if load_cell_lock_hash(0, Source::Output)? != next.payout_lock
                || load_cell_capacity(0, Source::Output)? != next.amount
            {
                return Err(Error::Payout);
            }
            if next.units == 0 {
                if load_cell_type_hash(0, Source::Output)?.is_some()
                    || syscalls::load_cell_data(&mut [], 0, 0, Source::Output)? != 0
                {
                    return Err(Error::Payout);
                }
            } else {
                let asset = next.asset.as_ref().ok_or(Error::Payout)?;
                if load_cell_type_hash(0, Source::Output)? != Some(asset.type_hash)
                    || token_amount(0, Source::Output)? != next.units
                {
                    return Err(Error::Payout);
                }
            }
        }
        Action::Rotate => {
            if same_lock
                || deposits != 0
                || next.next != previous.next
                || !next.no_payout()
                || before != after
                || assets_in.escrow_units != assets_out.escrow_units
            {
                return Err(Error::Rotation);
            }
        }
        Action::SendReserve | Action::ReceiveReserve => {
            if !same_lock
                || deposits != 0
                || next.next != previous.next
                || assets_in.escrow_units != assets_out.escrow_units
                || next.amount == 0
                || next.payout_lock == EMPTY
                || next.burn_id != EMPTY
                || next.units != 0
            {
                return Err(Error::Accounting);
            }
            let (capacity, opposite) = if next.action == Action::SendReserve {
                (before.checked_sub(next.amount), Action::ReceiveReserve)
            } else {
                (before.checked_add(next.amount), Action::SendReserve)
            };
            if capacity != Some(after) {
                return Err(Error::Accounting);
            }
            verify_reserve_peer(&next, opposite)?;
        }
    }
    Ok(())
}

fn verify_reserve_peer(state: &State, action: Action) -> Result<(), Error> {
    let own = load_script()?;
    let id = load_script_hash()?;
    if id == state.payout_lock {
        return Err(Error::Identity);
    }
    let mut matched = false;
    for index in 0.. {
        match load_cell_type_hash(index, Source::Output) {
            Ok(Some(hash)) if hash == state.payout_lock => {
                let counterpart = load_cell_type(index, Source::Output)?.ok_or(Error::Identity)?;
                let other = State::at(index, Source::Output)?;
                if matched
                    || counterpart.code_hash() != own.code_hash()
                    || counterpart.hash_type() != own.hash_type()
                    || other.action != action
                    || other.amount != state.amount
                    || other.payout_lock != id
                {
                    return Err(Error::Accounting);
                }
                matched = true;
            }
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => break,
            Err(error) => return Err(error.into()),
        }
    }
    if !matched {
        return Err(Error::Accounting);
    }
    Ok(())
}

fn scan_assets(asset: &Asset, vault: &[u8; 32], source: Source) -> Result<Assets, Error> {
    let mut totals = Assets::default();
    for index in 0.. {
        let lock = match load_cell_lock(index, source) {
            Ok(lock) => lock,
            Err(SysError::IndexOutOfBound) => break,
            Err(error) => return Err(error.into()),
        };
        if lock.code_hash().as_slice() != asset.gate_code
            || u8::from(lock.hash_type()) != ScriptHashType::Data1 as u8
        {
            continue;
        }
        let args = lock.args().raw_data();
        if !args.starts_with(vault) {
            continue;
        }
        let (_, role) = wire::gate(&args).ok_or(Error::Encoding)?;
        if role != Gate::Escrow && source == Source::Output {
            continue;
        }
        if load_cell_type_hash(index, source)? != Some(asset.type_hash) {
            return Err(Error::Accounting);
        }
        let amount = token_amount(index, source)?;
        let capacity = load_cell_capacity(index, source)?;
        if role == Gate::Escrow {
            totals.escrow_capacity = totals
                .escrow_capacity
                .checked_add(capacity)
                .ok_or(Error::Accounting)?;
            totals.escrow_units = totals
                .escrow_units
                .checked_add(amount)
                .ok_or(Error::Accounting)?;
        } else {
            if amount == 0 {
                return Err(Error::Accounting);
            }
            totals.deposit_capacity = totals
                .deposit_capacity
                .checked_add(capacity)
                .ok_or(Error::Accounting)?;
            totals.deposited_units = totals
                .deposited_units
                .checked_add(amount)
                .ok_or(Error::Accounting)?;
        }
    }
    Ok(totals)
}

fn token_amount(index: usize, source: Source) -> Result<u128, Error> {
    let mut data = [0; wire::AMOUNT_SIZE];
    if syscalls::load_cell_data(&mut data, 0, index, source)? != data.len() {
        return Err(Error::Accounting);
    }
    wire::amount(&data).ok_or(Error::Accounting)
}

fn withdrawal_id(nonce: u64) -> Result<[u8; 32], Error> {
    let mut hash = new_blake2b();
    hash.update(b"CKB bridge withdrawal v1\0");
    hash.update(&load_script_hash()?);
    hash.update(&nonce.to_le_bytes());
    let mut id = [0; 32];
    hash.finalize(&mut id);
    Ok(id)
}

fn deposit_capacity(state: &State) -> Result<u64, Error> {
    let args = load_script_hash()?;
    let expected = Script::new_builder()
        .code_hash(state.deposit_code.pack())
        .hash_type(ScriptHashType::Data1)
        .args(args.as_slice().pack())
        .build()
        .calc_script_hash();
    let mut total = 0u64;
    for index in 0.. {
        match load_cell_lock_hash(index, Source::Input) {
            Ok(lock) if expected.as_slice() == lock => {
                if load_cell_type_hash(index, Source::Input)?.is_some() {
                    return Err(Error::Accounting);
                }
                total = total
                    .checked_add(load_cell_capacity(index, Source::Input)?)
                    .ok_or(Error::Accounting)?;
            }
            Ok(_) => {}
            Err(SysError::IndexOutOfBound) => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(total)
}
