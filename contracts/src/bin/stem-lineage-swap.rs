//! Two-reserve constant-product swap. The participant's fee Lock authorizes
//! movement from that participant's reserve Cell. Asset Types verify totals.
#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{default_alloc, entry};

entry!(main);
default_alloc!();

const LOAD_CONTEXT: u64 = 2126;
const LOAD_DATA: u64 = 2127;
const WRITE_OUTPUT: u64 = 2128;
const RESERVE_DATA_BYTES: usize = 65;

fn load<const N: usize>(call: u64, source: u64, index: u64) -> Option<[u8; N]> {
    let mut bytes = [0; N];
    let mut length = N as u64;
    let mut status = bytes.as_mut_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") status,
            in("a1") &mut length,
            in("a2") 0u64,
            in("a3") index,
            in("a4") source,
            in("a7") call,
        );
    }
    (status == 0 && length == N as u64).then_some(bytes)
}

struct Reserve {
    amounts: [u128; 2],
    tag: u8,
    owner: [u8; 32],
    capacity: u64,
}

impl Reserve {
    fn load(index: u64) -> Option<Self> {
        let data = load::<RESERVE_DATA_BYTES>(LOAD_DATA, 1, index)?;
        let context = load::<48>(LOAD_CONTEXT, 1, index)?;
        Some(Self {
            amounts: [
                u128::from_le_bytes(data[..16].try_into().ok()?),
                u128::from_le_bytes(data[16..32].try_into().ok()?),
            ],
            tag: data[32],
            owner: data[33..].try_into().ok()?,
            capacity: u64::from_le_bytes(context[40..].try_into().ok()?),
        })
    }

    fn write(&self, index: u64) -> Option<()> {
        let mut data = [0; RESERVE_DATA_BYTES];
        data[..16].copy_from_slice(&self.amounts[0].to_le_bytes());
        data[16..32].copy_from_slice(&self.amounts[1].to_le_bytes());
        data[32] = self.tag;
        data[33..].copy_from_slice(&self.owner);
        let mut status = data.as_ptr() as u64;
        unsafe {
            asm!(
                "ecall",
                inout("a0") status,
                in("a1") RESERVE_DATA_BYTES as u64,
                in("a2") index,
                in("a3") self.capacity,
                in("a7") WRITE_OUTPUT,
            );
        }
        (status == 0).then_some(())
    }
}

fn swap() -> Option<()> {
    let context = load::<52>(LOAD_CONTEXT, 0, 0)?;
    let payload = load::<33>(LOAD_DATA, 0, 0)?;
    if context[48..] != 2u32.to_le_bytes() || payload[32] > 1 {
        return None;
    }
    let a = Reserve::load(0)?;
    let b = Reserve::load(1)?;
    let (pool_index, mut pool, mut trader) = match (a.tag, b.tag) {
        (0, 1) => (0, a, b),
        (1, 0) => (1, b, a),
        _ => return None,
    };
    if pool.owner != [0; 32] || trader.owner != context[16..48] {
        return None;
    }
    let sell = usize::from(payload[32]);
    let buy = 1 - sell;
    let amount = u128::from_le_bytes(payload[..16].try_into().ok()?);
    let minimum = u128::from_le_bytes(payload[16..32].try_into().ok()?);
    if amount == 0 || pool.amounts.contains(&0) {
        return None;
    }
    let new_sell = pool.amounts[sell].checked_add(amount)?;
    let received = amount
        .checked_mul(pool.amounts[buy])?
        .checked_div(new_sell)?;
    if received == 0 || received < minimum {
        return None;
    }
    pool.amounts[sell] = new_sell;
    pool.amounts[buy] = pool.amounts[buy].checked_sub(received)?;
    trader.amounts[sell] = trader.amounts[sell].checked_sub(amount)?;
    trader.amounts[buy] = trader.amounts[buy].checked_add(received)?;
    pool.write(pool_index)?;
    trader.write(1 - pool_index)
}

fn main() -> i8 {
    if swap().is_some() { 0 } else { 10 }
}
