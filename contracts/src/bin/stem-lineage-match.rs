//! Bilateral settlement between two actor-owned Lineage reserves.
#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{default_alloc, entry};

entry!(main);
default_alloc!();

const CONTEXT: u64 = 2126;
const DATA: u64 = 2127;
const WRITE: u64 = 2128;

fn load<const N: usize>(call: u64, source: u64, index: u64) -> Option<[u8; N]> {
    let mut bytes = [0; N];
    let mut size = N as u64;
    let mut status = bytes.as_mut_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") status,
            in("a1") &mut size,
            in("a2") 0u64,
            in("a3") index,
            in("a4") source,
            in("a7") call,
        );
    }
    (status == 0 && size == N as u64).then_some(bytes)
}

fn write(index: u64, data: &[u8; 65]) -> Option<()> {
    let head = load::<48>(CONTEXT, 1, index)?;
    let capacity = u64::from_le_bytes(head[40..48].try_into().ok()?);
    let mut status = data.as_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") status,
            in("a1") data.len() as u64,
            in("a2") index,
            in("a3") capacity,
            in("a7") WRITE,
        );
    }
    (status == 0).then_some(())
}

fn settle() -> Option<()> {
    let context = load::<52>(CONTEXT, 0, 0)?;
    if context[48..] != 2u32.to_le_bytes() || context[16..48] != [0; 32] {
        return None;
    }
    let actor_a = load::<32>(CONTEXT, 2, 0)?;
    let actor_b = load::<32>(CONTEXT, 2, 1)?;
    if load::<32>(CONTEXT, 2, 2).is_some() {
        return None;
    }
    let mut a = load::<65>(DATA, 1, 0)?;
    let mut b = load::<65>(DATA, 1, 1)?;
    let owns =
        (a[33..] == actor_a && b[33..] == actor_b) || (a[33..] == actor_b && b[33..] == actor_a);
    if a[32] != 2 || b[32] != 2 || !owns {
        return None;
    }

    let terms = if let Some(gated) = load::<40>(DATA, 0, 0) {
        let from = u64::from_le_bytes(gated[32..].try_into().ok()?);
        let height = u64::from_le_bytes(context[..8].try_into().ok()?);
        if height < from {
            return None;
        }
        gated[..32].try_into().ok()?
    } else {
        load::<32>(DATA, 0, 0)?
    };
    let x = u128::from_le_bytes(terms[..16].try_into().ok()?);
    let y = u128::from_le_bytes(terms[16..].try_into().ok()?);
    if x == 0 || y == 0 {
        return None;
    }
    let ax = u128::from_le_bytes(a[..16].try_into().ok()?).checked_sub(x)?;
    let ay = u128::from_le_bytes(a[16..32].try_into().ok()?).checked_add(y)?;
    let bx = u128::from_le_bytes(b[..16].try_into().ok()?).checked_add(x)?;
    let by = u128::from_le_bytes(b[16..32].try_into().ok()?).checked_sub(y)?;
    a[..16].copy_from_slice(&ax.to_le_bytes());
    a[16..32].copy_from_slice(&ay.to_le_bytes());
    b[..16].copy_from_slice(&bx.to_le_bytes());
    b[16..32].copy_from_slice(&by.to_le_bytes());
    write(0, &a)?;
    write(1, &b)
}

fn main() -> i8 {
    if settle().is_some() { 0 } else { 10 }
}
