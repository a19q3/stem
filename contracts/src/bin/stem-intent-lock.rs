//! Spend an independently authorized input against its paired output.
#![no_std]
#![no_main]

use core::arch::asm;

use ckb_hash::blake2b_256;
use ckb_std::{default_alloc, entry, high_level::load_script};
use stem_intent::{Authorization, LOCK_ARGS_BYTES, SPONSOR_BYTES, Terms};

entry!(main);
default_alloc!();

const INPUT: u64 = 1;
const OUTPUT: u64 = 2;
const GROUP_INPUT: u64 = 0x0100_0000_0000_0001;
const LOAD_SCRIPT_HASH: u64 = 2062;
const LOAD_CELL_BY_FIELD: u64 = 2081;
const LOAD_INPUT_BY_FIELD: u64 = 2083;
const LOAD_FIRST_TYPE_SCRIPT: u64 = 2111;
const LOAD_SCRIPT_GROUP: u64 = 2114;
const LOAD_GROUP_INDEX: u64 = 2115;
const LOAD_NETWORK_ID: u64 = 2116;
const LOAD_GROUP_PROOF: u64 = 2117;
const LOAD_GROUP_SEAL: u64 = 2118;
const LOAD_GROUP_SIGNING_DIGEST: u64 = 2119;
const SECP256K1_RECOVER: u64 = 2121;
const LOAD_BLOCK_CONTEXT: u64 = 2122;
const LOAD_INTENT_CONTEXT: u64 = 2124;

fn read(call: u64, index: u64, source: u64, field: u64, data: &mut [u8]) -> (u64, u64) {
    let mut length = data.len() as u64;
    let mut result = data.as_mut_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") result,
            in("a1") &mut length,
            in("a2") 0u64,
            in("a3") index,
            in("a4") source,
            in("a5") field,
            in("a7") call,
        );
    }
    (result, length)
}

fn read_fixed<const N: usize>(call: u64, index: u64, source: u64, field: u64) -> Result<[u8; N], i8> {
    let mut data = [0; N];
    (read(call, index, source, field, &mut data) == (0, N as u64))
        .then_some(data)
        .ok_or(20)
}

fn main() -> i8 {
    verify().err().unwrap_or(0)
}

fn verify() -> Result<(), i8> {
    let script = load_script().map_err(|_| 10)?;
    let args = script.args().raw_data();
    if args.len() != LOCK_ARGS_BYTES || u8::from(script.hash_type()) != 0 {
        return Err(11);
    }
    let seal = read_fixed::<65>(LOAD_GROUP_SEAL, 0, 0, 0)?;
    let mut proof = [0; SPONSOR_BYTES];
    let (status, len) = read(LOAD_GROUP_PROOF, 0, 0, 0, &mut proof);
    if status != 0 || len > SPONSOR_BYTES as u64 {
        return Err(12);
    }
    let digest = if len == 0 {
        // A complete transaction signature always lets the owner spend the Cell.
        read_fixed::<32>(LOAD_GROUP_SIGNING_DIGEST, 0, 0, 0)?
    } else {
        trade_digest(&Terms::decode(&proof[..len as usize]).ok_or(13)?)?
    };
    let mut public_key = [0u8; 33];
    let mut result = seal.as_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") result,
            in("a1") digest.as_ptr(),
            in("a2") public_key.as_mut_ptr(),
            in("a7") SECP256K1_RECOVER,
        );
    }
    if result != 0 || blake2b_256(public_key)[..20] != args[..20] {
        return Err(18);
    }
    Ok(())
}

fn trade_digest(terms: &Terms) -> Result<[u8; 32], i8> {
    let group = read_fixed::<9>(LOAD_SCRIPT_GROUP, 0, 0, 0)?;
    if group != [0, 1, 0, 0, 0, 0, 0, 0, 0] {
        return Err(21);
    }
    if read_fixed::<9>(LOAD_INPUT_BY_FIELD, 0, GROUP_INPUT, 1)? != [0; 9] {
        return Err(21);
    }
    let index = u64::from(u32::from_le_bytes(read_fixed(LOAD_GROUP_INDEX, 0, GROUP_INPUT, 0)?));
    let block = read_fixed::<48>(LOAD_BLOCK_CONTEXT, 0, 0, 0)?;
    let height = u64::from_le_bytes(block[..8].try_into().unwrap());
    let context = read_fixed::<41>(LOAD_INTENT_CONTEXT, 0, 0, 0)?;
    let fee = u64::from_le_bytes(context[32..40].try_into().unwrap());
    if height >= terms.valid_before || fee > terms.max_fee || context[40] != 0 {
        return Err(22);
    }
    if read_fixed::<32>(LOAD_CELL_BY_FIELD, index, OUTPUT, 7)? != terms.delivery {
        return Err(23);
    }
    let lock = read_fixed(LOAD_SCRIPT_HASH, 0, 0, 0)?;
    if let Some(sponsor) = &terms.sponsorship {
        if index != 0
            || read_fixed::<36>(LOAD_INPUT_BY_FIELD, 1, INPUT, 0)? != sponsor.subject
            || read_fixed::<32>(LOAD_CELL_BY_FIELD, 1, OUTPUT, 7)? != sponsor.delivery
            || read_fixed::<32>(LOAD_CELL_BY_FIELD, 0, OUTPUT, 3)? != lock
            || !plain(GROUP_INPUT)?
            || !plain(OUTPUT)?
        {
            return Err(24);
        }
        let input = u64::from_le_bytes(read_fixed(LOAD_CELL_BY_FIELD, 0, GROUP_INPUT, 0)?);
        let change = u64::from_le_bytes(read_fixed(LOAD_CELL_BY_FIELD, 0, OUTPUT, 0)?);
        if !input.checked_sub(change).is_some_and(|debit| {
            debit > 0 && debit <= sponsor.max_debit && debit <= fee
        }) {
            return Err(24);
        }
    }
    Ok(terms.signing_digest(&Authorization {
        network: read_fixed(LOAD_NETWORK_ID, 0, 0, 0)?,
        lock,
        out_point: read_fixed(LOAD_INPUT_BY_FIELD, 0, GROUP_INPUT, 0)?,
        input_cell: read_fixed(LOAD_CELL_BY_FIELD, 0, GROUP_INPUT, 7)?,
        dependencies: context[..32].try_into().unwrap(),
    }))
}

fn plain(source: u64) -> Result<bool, i8> {
    Ok(read(LOAD_FIRST_TYPE_SCRIPT, 0, source, 0, &mut []).0 == 1
        && read_fixed::<32>(LOAD_CELL_BY_FIELD, 0, source, 1)? == blake2b_256([]))
}
