//! Apply a CKB-issued permit to a single native object transition.
#![no_std]
#![no_main]

use ckb_std::{
    ckb_constants::{SYS_LOAD_CELL_BY_FIELD, Source},
    ckb_types::prelude::Entity,
    default_alloc, entry,
    error::SysError,
    high_level::{load_cell_capacity, load_input_out_point, load_script},
};
use core::arch::asm;
use stem_bridge_wire::object::permit::{Action, Permit, SIZE};
use stem_contracts::native_type::{contains, live_ckb_data};

const LOAD_STEM_NETWORK: u64 = 2116;
const CELL_CONTENT_HASH: u64 = 7;

entry!(main);
default_alloc!();

fn field(number: u64, source: u64, field: u64) -> Option<[u8; 32]> {
    let mut value = [0u8; 32];
    let mut length = 32u64;
    let mut status = value.as_mut_ptr() as u64;
    unsafe {
        asm!("ecall", inout("a0") status, in("a1") &mut length, in("a2") 0u64,
        in("a3") 0u64, in("a4") source, in("a5") field, in("a7") number);
    }
    (status == 0 && length == 32).then_some(value)
}

fn verify() -> Option<()> {
    let script = load_script().ok()?;
    let args = script.args().raw_data();
    let (object, issuer) = args.split_at_checked(32)?;
    let object = object.try_into().ok()?;
    let record = live_ckb_data::<{ 32 + SIZE }>(issuer.try_into().ok()?)?;
    let permit = Permit::decode(&record[32..])?;
    let output = Source::GroupOutput;
    let input = Source::GroupInput;
    if permit.network != field(LOAD_STEM_NETWORK, 0, 0)?
        || &permit.object != object
        || contains(output, 0, object) != Some(true)
        || load_cell_capacity(1, output) != Err(SysError::IndexOutOfBound)
        || permit.output_hash != field(SYS_LOAD_CELL_BY_FIELD, output as u64, CELL_CONTENT_HASH)?
    {
        return None;
    }
    let source = match (load_cell_capacity(0, input), permit.action) {
        (Err(SysError::IndexOutOfBound), Action::Create) => Source::Input,
        (Ok(_), Action::Transfer)
            if contains(input, 0, object) == Some(true)
                && load_cell_capacity(1, input) == Err(SysError::IndexOutOfBound) =>
        {
            input
        }
        _ => return None,
    };
    (load_input_out_point(0, source).ok()?.as_slice() == permit.source).then_some(())
}

fn main() -> i8 {
    if verify().is_some() { 0 } else { 120 }
}
