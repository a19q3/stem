//! Funding, signed orders and an independent accounting oracle for the CLOB fixture.
use std::{collections::BTreeMap, path::Path};

use anyhow::ensure;
use ckb_crypto::secp::Privkey;
use ckb_hash::blake2b_256;
use ckb_types::{packed::Byte32, prelude::*};
use serde::Serialize;
use serde_json::{Value, json};
use stem_ledger::lineage::{self, Command, LineageRef, Target};

use super::{Bytes, Cell, Context, FEE, FUND, Fixture, GroupWitness, Result, Transaction, lineage::DynamicLineage};

const TRADERS: usize = 4;
const DEPTH: usize = 16;
const INVENTORY: u64 = 10_000_000;
const BALANCES_AT: usize = 24 + 32 * TRADERS;
const ORDERS_AT: usize = BALANCES_AT + 16 * TRADERS;
const BOOK_BYTES: usize = ORDERS_AT + 26 * DEPTH;

#[derive(Clone, Copy)]
enum Action {
    Place { ask: bool, price: u64, size: u64 },
    Cancel(u64),
}

impl Action {
    fn encode(self) -> Bytes {
        let (kind, side, first, second) = match self {
            Self::Place { ask, price, size } => (0, u8::from(ask), price, size),
            Self::Cancel(id) => (1, 0, id, 0),
        };
        let mut bytes = [0; 18];
        bytes[0] = kind;
        bytes[1] = side;
        bytes[2..10].copy_from_slice(&first.to_le_bytes());
        bytes[10..].copy_from_slice(&second.to_le_bytes());
        Bytes::copy_from_slice(&bytes)
    }
}

fn scripted(index: usize, books: usize) -> (usize, Action) {
    let round = index / books;
    match round {
        0 => (1, Action::Cancel(1)),
        1 => (0, Action::Cancel(2)),
        _ => {
            let actor = round % TRADERS;
            let ask = actor % 2 == 1;
            (actor, Action::Place { ask, price: 100, size: 1 + (round % 3) as u64 })
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Order {
    owner: usize,
    ask: bool,
    price: u64,
    size: u64,
}

impl Order {
    fn locked(self) -> [u64; 2] {
        if self.ask { [self.size, 0] } else { [0, self.price * self.size] }
    }
}

const INITIAL: [(u64, Order); 2] =
    [(1, Order { owner: 1, ask: true, price: 101, size: 2 }), (2, Order { owner: 0, ask: false, price: 99, size: 2 })];

struct Trader {
    key: Privkey,
    fee: Cell,
    lock_hash: Byte32,
}

fn seed_data(traders: &[Trader]) -> Bytes {
    let mut data = Vec::with_capacity(BOOK_BYTES);
    data.extend(3_u64.to_le_bytes());
    data.extend([0; 16]);
    for trader in traders {
        data.extend(trader.lock_hash.as_slice());
    }
    let mut balances = [[INVENTORY; 2]; TRADERS];
    for (_, order) in INITIAL {
        let [base, quote] = order.locked();
        balances[order.owner][0] -= base;
        balances[order.owner][1] -= quote;
    }
    for balance in balances {
        for value in balance {
            data.extend(value.to_le_bytes());
        }
    }
    for (id, order) in INITIAL {
        data.extend(id.to_le_bytes());
        data.extend(order.price.to_le_bytes());
        data.extend(order.size.to_le_bytes());
        data.extend([order.owner as u8, u8::from(order.ask)]);
    }
    data.resize(BOOK_BYTES, 0);
    Bytes::from(data)
}

fn setup(fixture: &mut Fixture, dynamic: &mut DynamicLineage) -> Result<Vec<Trader>> {
    let program = super::contract::native_contract(Path::new(env!("CARGO_MANIFEST_DIR")), "stem-lineage-clob")?;
    dynamic.executor = blake2b_256(&program).pack();
    fixture
        .spec
        .development_cells
        .iter_mut()
        .find(|(point, _)| point == &dynamic.executor_dep)
        .context("missing CLOB Executor dependency")?
        .1 = Cell::new(Cell::minimum_capacity(&fixture.lock, &[], program.len())?, fixture.lock.clone(), vec![], program)?;
    let traders = (0..TRADERS)
        .map(|actor| {
            let key = Privkey::from_slice(&[90 + actor as u8; 32]);
            let digest = blake2b_256(key.pubkey()?.serialize());
            let lock = lineage::lock(&dynamic.fee_cell.lock().code_hash(), digest[..20].try_into()?, false);
            let lock_hash = lock.calc_script_hash();
            let fee = Cell::new(FUND, lock, vec![], Bytes::new())?;
            Ok(Trader { key, fee, lock_hash })
        })
        .collect::<Result<Vec<_>>>()?;
    let state = seed_data(&traders);
    for (point, cell) in &mut fixture.spec.development_cells {
        if dynamic.anchors.contains(point) {
            *cell = Cell::new(FUND, fixture.lock.clone(), vec![], state.clone())?;
        }
        let index: u32 = point.index().unpack();
        let index = index as usize;
        if dynamic.fee_points.get(index) == Some(point) {
            *cell = traders[scripted(index, dynamic.ids.len()).0].fee.clone();
        }
    }
    Ok(traders)
}

fn signed(
    fixture: &Fixture,
    dynamic: &DynamicLineage,
    traders: &[Trader],
    fee: usize,
    book: usize,
    actor: usize,
    action: Action,
) -> Result<Transaction> {
    let tx = Transaction::new_lineage_command(
        dynamic.deps.clone(),
        dynamic.fee_points[fee].clone(),
        1_000,
        Command {
            targets: vec![Target {
                reference: LineageRef::new(dynamic.ids[book].clone(), None)?,
                latest_bounds: None,
                min_capacity: FUND - FEE,
            }],
            executor: dynamic.executor.clone(),
            executor_dep: dynamic.executor_dep.clone(),
            executor_cycles: 600_000,
            validation_cycles: 5_000_000,
            fee: FEE,
            payload: action.encode(),
        },
    )?;
    let trader = &traders[actor];
    let digest = tx.lineage_signing_digest(&fixture.spec.network, &trader.fee)?;
    tx.with_witnesses(vec![GroupWitness {
        group_index: 0,
        proof_data: Bytes::new(),
        seal: Bytes::from(trader.key.sign_recoverable(&digest.unpack())?.serialize()),
    }])
}

pub(super) fn fund(fixture: &mut Fixture, dynamic: &mut DynamicLineage) -> Result<(Vec<Transaction>, Vec<Transaction>)> {
    let traders = setup(fixture, dynamic)?;
    let books = dynamic.ids.len();
    let head_lock = lineage::lock(&dynamic.fee_cell.lock().code_hash(), &[0; 20], true);
    let registrations = (0..books).map(|book| dynamic.register_with_lock(fixture, book, head_lock.clone())).collect::<Result<_>>()?;
    let commands = (0..dynamic.fee_points.len())
        .map(|index| {
            let (actor, action) = scripted(index, books);
            signed(fixture, dynamic, &traders, index, index % books, actor, action)
        })
        .collect::<Result<_>>()?;
    Ok((registrations, commands))
}

#[derive(Clone)]
struct Book {
    // Assets owned include outstanding collateral. Derive free balances from the open orders.
    owned: [[u64; 2]; TRADERS],
    orders: BTreeMap<u64, Order>,
    next: u64,
    fills: u64,
    traded: u64,
}

impl Default for Book {
    fn default() -> Self {
        Self { owned: [[INVENTORY; 2]; TRADERS], orders: BTreeMap::from(INITIAL), next: 3, fills: 0, traded: 0 }
    }
}

impl Book {
    fn available(&self, owner: usize) -> [u64; 2] {
        let mut free = self.owned[owner];
        for order in self.orders.values().filter(|order| order.owner == owner) {
            let [base, quote] = order.locked();
            free[0] -= base;
            free[1] -= quote;
        }
        free
    }

    fn apply(&mut self, owner: usize, action: Action) -> Option<Vec<u64>> {
        let mut candidate = self.clone();
        let mut filled_orders = Vec::new();
        match action {
            Action::Cancel(id) => {
                if !candidate.orders.get(&id).is_some_and(|order| order.owner == owner) {
                    return None;
                }
                candidate.orders.remove(&id);
            }
            Action::Place { ask, price, size } => {
                let mut proposal = Order { owner, ask, price, size };
                if !ask {
                    price.checked_mul(size)?;
                }
                let [base, quote] = proposal.locked();
                let free = candidate.available(owner);
                if price == 0 || size == 0 || free[0] < base || free[1] < quote {
                    return None;
                }
                let mut crossing = candidate
                    .orders
                    .iter()
                    .filter_map(|(&id, order)| {
                        (order.ask != ask && if ask { order.price >= price } else { order.price <= price }).then_some((id, *order))
                    })
                    .collect::<Vec<_>>();
                crossing.sort_by(|(a_id, a), (b_id, b)| {
                    let price = a.price.cmp(&b.price);
                    (if ask { price.reverse() } else { price }).then(a_id.cmp(b_id))
                });
                for (id, mut maker) in crossing {
                    if proposal.size == 0 {
                        break;
                    }
                    if maker.owner == owner {
                        return None;
                    }
                    let filled = proposal.size.min(maker.size);
                    let notional = filled * maker.price;
                    let (buyer, seller) = if ask { (maker.owner, owner) } else { (owner, maker.owner) };
                    candidate.owned[buyer][0] += filled;
                    candidate.owned[buyer][1] -= notional;
                    candidate.owned[seller][0] -= filled;
                    candidate.owned[seller][1] += notional;
                    proposal.size -= filled;
                    maker.size -= filled;
                    if maker.size == 0 {
                        candidate.orders.remove(&id);
                    } else {
                        candidate.orders.insert(id, maker);
                    }
                    candidate.fills += 1;
                    candidate.traded += filled;
                    filled_orders.push(id);
                }
                if proposal.size > 0 {
                    if candidate.orders.len() == DEPTH {
                        return None;
                    }
                    candidate.orders.insert(candidate.next, proposal);
                }
                candidate.next += 1;
            }
        }
        *self = candidate;
        Some(filled_orders)
    }

    fn verify(&self, bytes: &[u8]) -> Result<()> {
        ensure!(bytes.len() == BOOK_BYTES, "invalid CLOB state length");
        let read = |i| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        ensure!((read(0), read(8), read(16)) == (self.next, self.fills, self.traded), "CLOB counters differ from serial oracle");
        for owner in 0..TRADERS {
            let offset = BALANCES_AT + 16 * owner;
            ensure!([read(offset), read(offset + 8)] == self.available(owner), "CLOB free balance differs for trader {owner}");
        }
        let mut actual = BTreeMap::new();
        for index in 0..DEPTH {
            let offset = ORDERS_AT + 26 * index;
            if read(offset) != 0 {
                let order = Order {
                    price: read(offset + 8),
                    size: read(offset + 16),
                    owner: usize::from(bytes[offset + 24]),
                    ask: bytes[offset + 25] == 1,
                };
                ensure!(actual.insert(read(offset), order).is_none(), "duplicate CLOB order ID");
            }
        }
        ensure!(actual == self.orders, "CLOB order IDs, priority or remaining sizes differ");
        for asset in 0..2 {
            let total = self.owned.iter().map(|holding| holding[asset]).sum::<u64>();
            ensure!(total == INVENTORY * TRADERS as u64, "CLOB supply changed");
        }
        Ok(())
    }
}

#[derive(Default, Serialize)]
struct Stats {
    placements: u64,
    cancellations: u64,
    rejected: u64,
    fills: u64,
    base_quantity: u64,
}

pub(super) struct Oracle {
    markets: Vec<Book>,
    active: BTreeMap<Byte32, usize>,
    counters: Stats,
    matches: BTreeMap<String, u64>,
    makers: BTreeMap<(usize, u64), String>,
    fill_pairs: Vec<[String; 2]>,
}

impl Oracle {
    pub(super) fn new(ids: &[Byte32]) -> Self {
        Self {
            markets: vec![Book::default(); ids.len()],
            active: ids.iter().cloned().enumerate().map(|(i, id)| (id, i)).collect(),
            counters: Stats::default(),
            matches: BTreeMap::new(),
            makers: BTreeMap::new(),
            fill_pairs: Vec::new(),
        }
    }

    pub(super) fn apply(
        &mut self,
        dynamic: &DynamicLineage,
        network: &Byte32,
        transaction: &Transaction,
        command: &Command,
        applied: bool,
    ) -> Result<()> {
        let fee = transaction.input(0).context("missing CLOB fee input")?.previous_output;
        let index: u32 = fee.index().unpack();
        let index = index as usize;
        ensure!(dynamic.fee_points.get(index) == Some(&fee), "unknown CLOB fee input");
        let market = index % self.markets.len();
        ensure!(command.targets.len() == 1 && command.targets[0].reference.id() == &dynamic.ids[market], "unexpected CLOB market");
        let (actor, action) = scripted(index, self.markets.len());
        ensure!(command.payload == action.encode(), "unexpected signed market operation");
        let book = &mut self.markets[market];
        let order_id = book.next;
        let traded = book.traded;
        let matches = book.apply(actor, action);
        ensure!(matches.is_some() == applied, "CLOB outcome differs from serial oracle");
        let Some(matches) = matches else {
            self.counters.rejected += 1;
            return Ok(());
        };
        let tx_id = hex::encode(transaction.id(network).as_slice());
        match action {
            Action::Place { .. } => {
                self.counters.placements += 1;
                if book.orders.contains_key(&order_id) {
                    self.makers.insert((market, order_id), tx_id.clone());
                }
            }
            Action::Cancel(id) => {
                self.counters.cancellations += 1;
                self.makers.remove(&(market, id));
            }
        }
        for id in &matches {
            if let Some(maker) = self.makers.get(&(market, *id)) {
                self.fill_pairs.push([maker.clone(), tx_id.clone()]);
            }
            if !book.orders.contains_key(id) {
                self.makers.remove(&(market, *id));
            }
        }
        if !matches.is_empty() {
            self.matches.insert(tx_id, matches.len() as u64);
        }
        self.counters.fills += matches.len() as u64;
        self.counters.base_quantity += book.traded - traded;
        Ok(())
    }

    pub(super) fn verify(&self, id: &Byte32, bytes: &[u8]) -> Result<()> {
        let index = self.active.get(id).context("missing CLOB market")?;
        self.markets[*index].verify(bytes)
    }

    pub(super) fn evidence(&self) -> Value {
        json!({
            "markets": self.markets.len(), "operations": self.counters, "fills_by_transaction": self.matches,
            "maker_and_taker_transactions": self.fill_pairs,
            "resting_orders": self.markets.iter().map(|market| market.orders.len()).sum::<usize>(),
        })
    }
}

#[test]
fn native_clob_enforces_priority_ownership_and_atomic_rejection() -> Result<()> {
    use super::{Retention, Store, TIME, lineage::FixtureLineage};
    use stem_ledger::lineage::Outcome;

    let ask = |price, size| Action::Place { ask: true, price, size };
    let bid = |price, size| Action::Place { ask: false, price, size };
    let mut operations = vec![
        (2, Action::Cancel(1), false),
        (1, Action::Cancel(1), true),
        (0, Action::Cancel(2), true),
        (1, ask(105, 4), true),
        (3, ask(100, 2), true),
        (1, ask(100, 3), true),
        (0, bid(110, 4), true),
        (2, bid(103, 1), true),
        (1, Action::Cancel(3), true),
        (0, bid(101, INVENTORY), false),
    ];
    operations.extend((0..DEPTH).map(|_| (1, ask(200, 1), true)));
    operations.extend([(3, ask(200, 1), false), (1, bid(200, 1), false), (0, bid(200, 2), true), (3, ask(200, 1), true)]);
    let mut fixture = Fixture::new()?;
    let registry = FixtureLineage::fund(&mut fixture)?;
    let mut dynamic = DynamicLineage::fund_using(&mut fixture, &registry, operations.len(), 1, None)?;
    let traders = setup(&mut fixture, &mut dynamic)?;
    for (index, (actor, _, _)) in operations.iter().enumerate() {
        fixture
            .spec
            .development_cells
            .iter_mut()
            .find(|(point, _)| point == &dynamic.fee_points[index])
            .context("missing test fee Cell")?
            .1 = traders[*actor].fee.clone();
    }
    let lock = lineage::lock(&dynamic.fee_cell.lock().code_hash(), &[0; 20], true);
    let registration = dynamic.register_with_lock(&fixture, 0, lock)?;
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(directory.path(), &fixture.spec, Retention::Archive)?;
    store.commit(store.prepare(&[registration], TIME, false)?)?;
    let mut expected = Book::default();
    for (index, (actor, action, applied)) in operations.into_iter().enumerate() {
        let tx = signed(&fixture, &dynamic, &traders, index, 0, actor, action)?;
        let block = store.prepare(&[tx], TIME + 1 + index as u64, false)?;
        let receipt = &block.receipts()[0];
        assert_eq!(
            Outcome::decode(&receipt.protocol_result)?,
            if applied { Outcome::Applied } else { Outcome::Rejected },
            "operation {index}"
        );
        assert_eq!(receipt.fee, FEE);
        let matches = expected.apply(actor, action);
        assert_eq!(matches.is_some(), applied);
        if index == 6 {
            assert_eq!(matches, Some(vec![4, 5]));
        }
        if index == 7 {
            assert_eq!(matches, Some(vec![5]));
        }
        store.commit(block)?;
        let proof = store.prove_current_lineage(&dynamic.ids[0])?;
        assert!(proof.verify(&fixture.spec.network, store.tip(), &LineageRef::new(dynamic.ids[0].clone(), None)?, &dynamic.marker));
        let (_, cell, _) = proof.head.context("missing CLOB Head")?;
        expected.verify(&cell.cell.data())?;
    }
    drop(store);
    let reopened = Store::open(directory.path(), &fixture.spec, Retention::Archive)?;
    let (_, cell, _) = reopened.prove_current_lineage(&dynamic.ids[0])?.head.context("missing reopened CLOB Head")?;
    expected.verify(&cell.cell.data())?;
    Ok(())
}
