//! Closed order-book fixture for the native Lineage benchmarks.
#![no_std]
#![no_main]

use core::arch::asm;

use ckb_std::{default_alloc, entry};

entry!(main);
default_alloc!();

const ACCOUNTS: usize = 4;
const ORDERS: usize = 16;
const BALANCES_AT: usize = 24 + 32 * ACCOUNTS;
const ORDERS_AT: usize = BALANCES_AT + 16 * ACCOUNTS;
const ORDER_BYTES: usize = 26;
const BOOK_BYTES: usize = ORDERS_AT + ORDERS * ORDER_BYTES;

fn load<const N: usize>(call: u64, source: u64) -> Option<[u8; N]> {
    let mut bytes = [0; N];
    let mut length = N as u64;
    let mut status = bytes.as_mut_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") status,
            in("a1") &mut length,
            in("a2") 0u64,
            in("a3") 0u64,
            in("a4") source,
            in("a7") call,
        );
    }
    (status == 0 && length == N as u64).then_some(bytes)
}

fn number(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().unwrap())
}

#[derive(Clone, Copy)]
struct Order {
    id: u64,
    price: u64,
    size: u64,
    owner: usize,
    side: usize,
}

impl Order {
    fn read(raw: &[u8]) -> Option<Self> {
        let order = Self {
            id: number(&raw[..8]),
            price: number(&raw[8..16]),
            size: number(&raw[16..24]),
            owner: usize::from(raw[24]),
            side: usize::from(raw[25]),
        };
        (order.id != 0).then_some(order)
    }

    fn write(self, raw: &mut [u8]) {
        raw[..8].copy_from_slice(&self.id.to_le_bytes());
        raw[8..16].copy_from_slice(&self.price.to_le_bytes());
        raw[16..24].copy_from_slice(&self.size.to_le_bytes());
        raw[24] = self.owner as u8;
        raw[25] = self.side as u8;
    }

    fn collateral(&self) -> Option<u64> {
        if self.side == 0 { self.price.checked_mul(self.size) } else { Some(self.size) }
    }
}

struct Book {
    free: [[u64; 2]; ACCOUNTS],
    orders: [Option<Order>; ORDERS],
    next_id: u64,
    fills: u64,
    traded: u64,
}

impl Book {
    fn load(raw: &[u8; BOOK_BYTES]) -> Option<Self> {
        let mut free = [[0; 2]; ACCOUNTS];
        for (entry, raw) in free.iter_mut().zip(raw[BALANCES_AT..ORDERS_AT].chunks_exact(16)) {
            *entry = [number(&raw[..8]), number(&raw[8..])];
        }
        let mut orders = [None; ORDERS];
        for (entry, raw) in orders.iter_mut().zip(raw[ORDERS_AT..].chunks_exact(ORDER_BYTES)) {
            *entry = Order::read(raw);
            if entry.is_some_and(|order| order.owner >= ACCOUNTS || order.side > 1 || order.price == 0 || order.size == 0) {
                return None;
            }
        }
        Some(Self { free, orders, next_id: number(&raw[..8]), fills: number(&raw[8..16]), traded: number(&raw[16..24]) })
    }

    fn write(self, raw: &mut [u8; BOOK_BYTES]) {
        raw[..8].copy_from_slice(&self.next_id.to_le_bytes());
        raw[8..16].copy_from_slice(&self.fills.to_le_bytes());
        raw[16..24].copy_from_slice(&self.traded.to_le_bytes());
        for (balance, raw) in self.free.into_iter().zip(raw[BALANCES_AT..ORDERS_AT].chunks_exact_mut(16)) {
            raw[..8].copy_from_slice(&balance[0].to_le_bytes());
            raw[8..].copy_from_slice(&balance[1].to_le_bytes());
        }
        for (order, slot) in self.orders.into_iter().zip(raw[ORDERS_AT..].chunks_exact_mut(ORDER_BYTES)) {
            if let Some(order) = order {
                order.write(slot);
            } else {
                slot.fill(0);
            }
        }
    }

    fn place(&mut self, mut taker: Order) -> Option<()> {
        if taker.side > 1 || taker.price == 0 || taker.size == 0 {
            return None;
        }
        let reserved = &mut self.free[taker.owner][1 - taker.side];
        *reserved = reserved.checked_sub(taker.collateral()?)?;
        self.next_id = self.next_id.checked_add(1)?;
        while taker.size != 0 {
            let Some(index) = self.best_maker(taker) else { break };
            let mut maker = self.orders[index]?;
            if maker.owner == taker.owner {
                return None;
            }

            let size = maker.size.min(taker.size);
            let cost = size.checked_mul(maker.price)?;
            let (buyer, seller, refund) = if taker.side == 0 {
                let refund = size.checked_mul(taker.price - maker.price)?;
                (taker.owner, maker.owner, refund)
            } else {
                (maker.owner, taker.owner, 0)
            };
            self.credit(buyer, 0, size)?;
            self.credit(buyer, 1, refund)?;
            self.credit(seller, 1, cost)?;
            taker.size -= size;
            maker.size -= size;
            self.orders[index] = (maker.size != 0).then_some(maker);
            self.fills = self.fills.checked_add(1)?;
            self.traded = self.traded.checked_add(size)?;
        }
        if taker.size != 0 {
            *self.orders.iter_mut().find(|slot| slot.is_none())? = Some(taker);
        }
        Some(())
    }

    fn best_maker(&self, taker: Order) -> Option<usize> {
        let mut best: Option<(usize, u64, u64)> = None;
        for (index, slot) in self.orders.iter().enumerate() {
            let Some(order) = slot else { continue };
            if order.side == taker.side {
                continue;
            }
            let priority = if taker.side == 0 && order.price <= taker.price {
                order.price
            } else if taker.side == 1 && order.price >= taker.price {
                u64::MAX - order.price
            } else {
                continue;
            };
            if best.is_none_or(|(_, price, id)| (priority, order.id) < (price, id)) {
                best = Some((index, priority, order.id));
            }
        }
        best.map(|(index, _, _)| index)
    }

    fn cancel(&mut self, owner: usize, id: u64) -> Option<()> {
        let slot = self.orders.iter_mut().find(|slot| slot.is_some_and(|order| order.id == id && order.owner == owner))?;
        let order = slot.take()?;
        self.credit(owner, 1 - order.side, order.collateral()?)
    }

    fn credit(&mut self, owner: usize, asset: usize, value: u64) -> Option<()> {
        self.free[owner][asset] = self.free[owner][asset].checked_add(value)?;
        Some(())
    }
}

fn transition() -> Option<()> {
    let context = load::<52>(2126, 0)?;
    if context[48..] != 1u32.to_le_bytes() {
        return None;
    }
    let mut data = load::<BOOK_BYTES>(2127, 1)?;
    let owner = data[24..BALANCES_AT].chunks_exact(32).position(|payer| payer == &context[16..48])?;
    let payload = load::<18>(2127, 0)?;
    let mut book = Book::load(&data)?;
    let first = number(&payload[2..10]);
    let second = number(&payload[10..]);
    match payload[0] {
        0 => book.place(Order { id: book.next_id, price: first, size: second, owner, side: usize::from(payload[1]) })?,
        1 if payload[1] == 0 && second == 0 => book.cancel(owner, first)?,
        _ => return None,
    }
    book.write(&mut data);
    let head = load::<48>(2126, 1)?;
    let capacity = number(&head[40..48]);
    let mut status = data.as_ptr() as u64;
    unsafe {
        asm!(
            "ecall",
            inout("a0") status,
            in("a1") BOOK_BYTES as u64,
            in("a2") 0u64,
            in("a3") capacity,
            in("a7") 2128u64,
        );
    }
    (status == 0).then_some(())
}

fn main() -> i8 {
    if transition().is_some() { 0 } else { 10 }
}
