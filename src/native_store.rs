//! Atomic persistence for the native ledger. Both state trees share the same
//! column family and commit batch as the header, execution data, and MMR peaks.

use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{DB, Options, WriteBatch, WriteOptions, prelude::*};
use ckb_script::fern::VerificationCache;
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::*,
};
use fern_ledger::{
    BlockContext, CapacityPolicy, Cell, LiveCell, Origin, ResolvedTransaction, Transaction,
    block::{Header, HeaderContext, Receipt, StateCommitment},
    history::History,
    patricia::{Key, Namespace, Patricia, Proof},
    proof::Tree,
    protocol::Operation,
    record,
};

mod staking;
mod state_snapshot;

pub use staking::StakeGenesis;
use staking::{StagedStake, Staking};
pub use state_snapshot::StateSnapshot;

const MARKER: &[u8] = b"fern/native-spec";
const TIP: &[u8] = b"fern/tip";
const HISTORY: &[u8] = b"fern/history";
const EXECUTION: u8 = b'e';
const HEADER: u8 = b'h';
const HEADER_ID: u8 = b'i';
const STATE_NODE: u8 = b'n';
const STATE_RECORD: u8 = b's';
const FEE_KEY: &[u8] = b"fees/extinguished";
const PARAMETER_KEY: &[u8] = b"genesis/network-parameters";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub transaction_count: u32,
    pub transaction_bytes: u64,
    pub transaction_cycles: u64,
    pub block_cycles: u64,
    pub cell_reads: u64,
    pub cell_read_bytes: u64,
    pub cell_write_bytes: u64,
    pub header_dep_age: u64,
}

impl Limits {
    fn encode(self) -> Vec<u8> {
        [
            u64::from(self.transaction_count),
            self.transaction_bytes,
            self.transaction_cycles,
            self.block_cycles,
            self.cell_reads,
            self.cell_read_bytes,
            self.cell_write_bytes,
            self.header_dep_age,
        ]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect()
    }
}

pub struct Spec {
    pub chain_id: String,
    pub development_validators: Vec<([u8; 32], i64)>,
    pub stake: Option<StakeGenesis>,
    pub network: Byte32,
    pub genesis_time_ms: u64,
    pub capacity: CapacityPolicy,
    pub limits: Limits,
    /// Explicit research-network fixture allocations and reference programs.
    pub development_cells: Vec<(OutPoint, Cell)>,
}

#[derive(Clone, Copy)]
pub enum Retention {
    Recent(u64),
    Archive,
}

pub struct Store {
    db: DB,
    network: Byte32,
    chain_id: String,
    genesis_validators: Vec<([u8; 32], i64)>,
    staking: Option<Staking>,
    genesis_id: Byte32,
    tip: Header,
    history: History,
    cells: Patricia,
    system: Patricia,
    retention: Retention,
    capacity: CapacityPolicy,
    limits: Limits,
    cache: VerificationCache,
}

pub struct Prepared {
    parent: Byte32,
    next: Header,
    history: History,
    receipts: Vec<Receipt>,
    validator_updates: Vec<fern_stake::Member>,
    batch: Batch,
}

impl Prepared {
    pub fn header(&self) -> &Header {
        &self.next
    }

    pub fn receipts(&self) -> &[Receipt] {
        &self.receipts
    }
    pub fn validator_updates(&self) -> &[fern_stake::Member] {
        &self.validator_updates
    }
}

impl Store {
    pub fn open(path: &Path, spec: &Spec, retention: Retention) -> Result<Self> {
        ensure!(
            spec.network != Byte32::default(),
            "zero Fern network identity"
        );
        ensure!(
            spec.limits.transaction_cycles <= spec.limits.block_cycles,
            "transaction cycles exceed block cycles"
        );
        if let Retention::Recent(blocks) = retention {
            ensure!(
                blocks >= spec.limits.header_dep_age
                    && blocks != 0
                    && spec
                        .stake
                        .as_ref()
                        .is_none_or(|stake| blocks >= stake.signing_window),
                "retention is shorter than the consensus Header Dep horizon"
            );
        }
        ensure!(
            !spec.chain_id.is_empty() && spec.chain_id.len() <= 50,
            "invalid Fern chain ID"
        );
        let configured_staking = spec
            .stake
            .as_ref()
            .map(|genesis| Staking::initialize(genesis, spec.network.unpack()))
            .transpose()?;
        ensure!(
            configured_staking.is_none()
                || (spec.development_validators.is_empty() && spec.development_cells.is_empty()),
            "stake-backed genesis cannot specify unfunded development allocations or a development committee"
        );
        let mut validators = if let Some((_, state, _, _)) = &configured_staking {
            state
                .announced
                .iter()
                .map(|member| Ok((member.key, i64::try_from(member.power)?)))
                .collect::<Result<_>>()?
        } else {
            spec.development_validators.clone()
        };
        validators.sort_unstable();
        ensure!(
            validators.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "duplicate genesis validator"
        );
        let power = validators.iter().try_fold(0i64, |total, (key, power)| {
            ensure!(*power > 0, "nonpositive genesis voting power");
            ensure!(
                !ed25519_dalek::VerifyingKey::from_bytes(key)?.is_weak(),
                "weak genesis validator key"
            );
            total
                .checked_add(*power)
                .context("genesis voting power overflow")
        })?;
        ensure!(
            power <= i64::MAX / 8,
            "genesis voting power exceeds CometBFT maximum"
        );
        let mut options = Options::default();
        options.create_if_missing(true);
        let db = DB::open(&options, path)?;
        let network = spec.network.clone();
        let cells = Patricia::new(network.clone(), Namespace::Cells);
        let system = Patricia::new(network.clone(), Namespace::System);
        let mut history = History::empty(network.clone());
        let context = HeaderContext {
            height: 0,
            timestamp_ms: spec.genesis_time_ms,
            parent: Byte32::default(),
            transaction_root: Tree::transactions(network.clone(), &[])?.root(),
        };
        let origin = Origin::new(&network, &context);
        let mut batch = Batch::default();
        let mut cell_edit = cells.edit(cells.empty_root(), |_| Ok(None));
        spec.development_cells
            .iter()
            .try_fold(0u64, |sum, (point, cell)| -> Result<u64> {
                ensure!(!point.is_null(), "null genesis allocation OutPoint");
                spec.capacity.verify(cell)?;
                let cell = LiveCell {
                    cell: cell.clone(),
                    origin: origin.clone(),
                };
                let id = record::cell_key(&cells, point);
                let record = record::encode_cell(point, &cell);
                ensure!(
                    cell_edit
                        .set(id, Some(cells.value_hash(&record)))?
                        .is_none(),
                    "duplicate genesis allocation"
                );
                batch.put(state_key(STATE_RECORD, Namespace::Cells, &id), record);
                sum.checked_add(cell.cell.capacity())
                    .context("genesis Capacity overflow")
            })?;
        let mut system_edit = system.edit(system.empty_root(), |_| Ok(None));
        let mut params = spec.limits.encode();
        params.extend_from_slice(&spec.capacity.bond_per_byte.to_le_bytes());
        params.extend_from_slice(&spec.capacity.state_overhead.to_le_bytes());
        params.push(spec.chain_id.len() as u8);
        params.extend_from_slice(spec.chain_id.as_bytes());
        for (public_key, power) in &validators {
            params.extend_from_slice(public_key);
            params.extend_from_slice(&power.to_le_bytes());
        }
        let id = system.record_key(PARAMETER_KEY);
        system_edit.set(id, Some(system.value_hash(&params)))?;
        batch.put(state_key(STATE_RECORD, Namespace::System, &id), params);
        let staking = if let Some((staking, state, config, snapshot)) = configured_staking {
            for (name, value) in state
                .genesis_records()
                .into_iter()
                .chain([(staking::CONFIG, config), (staking::SNAPSHOT, snapshot)])
            {
                let id = system.record_key(name);
                system_edit.set(id, Some(system.value_hash(&value)))?;
                batch.put(state_key(STATE_RECORD, Namespace::System, &id), value);
            }
            Some(staking)
        } else {
            None
        };
        let genesis = Header {
            context,
            state: StateCommitment {
                cell_root: cell_edit.root().pack(),
                system_root: system_edit.root().pack(),
                history_root: history.root(),
            },
            receipt_root: Receipt::root(&network, &[])?,
        };
        let genesis_id = genesis.id(&network);
        let cell_nodes = cell_edit.into_writes();
        let system_nodes = system_edit.into_writes();
        let tip = if let Some(marker) = db.get(MARKER)? {
            ensure!(
                marker.as_ref() == genesis_id.as_slice(),
                "database network or genesis differs"
            );
            let tip = Header::decode(&db.get(TIP)?.context("missing Fern tip")?)?;
            history = History::decode(
                network.clone(),
                &db.get(HISTORY)?.context("missing Fern history frontier")?,
            )?;
            ensure!(
                history.count() == tip.context.height && history.root() == tip.state.history_root,
                "history frontier differs from tip"
            );
            tip
        } else {
            ensure!(
                db.iterator(ckb_db::internal::IteratorMode::Start)
                    .next()
                    .is_none(),
                "database is not empty"
            );
            put_nodes(&mut batch, Namespace::Cells, cell_nodes);
            put_nodes(&mut batch, Namespace::System, system_nodes);
            batch.put(MARKER, genesis_id.as_slice());
            put_tip(&mut batch, &network, &genesis, &history);
            sync(&db, &batch)?;
            genesis
        };
        Ok(Self {
            db,
            network,
            chain_id: spec.chain_id.clone(),
            genesis_validators: validators,
            staking,
            genesis_id,
            tip,
            history,
            cells,
            system,
            retention,
            capacity: spec.capacity,
            limits: spec.limits,
            cache: VerificationCache::new(4096),
        })
    }

    pub fn tip(&self) -> &Header {
        &self.tip
    }

    pub fn network(&self) -> &Byte32 {
        &self.network
    }

    pub fn chain_id(&self) -> &str {
        &self.chain_id
    }

    pub fn genesis_id(&self) -> &Byte32 {
        &self.genesis_id
    }

    pub fn genesis_validators(&self) -> &[([u8; 32], i64)] {
        &self.genesis_validators
    }

    fn system_record(&self, name: &[u8]) -> Result<Option<Vec<u8>>> {
        let id = self.system.record_key(name);
        Ok(self
            .db
            .get(state_key(STATE_RECORD, Namespace::System, &id))?
            .map(|value| value.to_vec()))
    }

    fn stage_stake(&self) -> Result<Option<StagedStake>> {
        self.staking
            .as_ref()
            .map(|staking| staking.load(self))
            .transpose()
    }

    pub fn fee_surplus(&self) -> Result<u64> {
        let id = self.system.record_key(FEE_KEY);
        self.db
            .get(state_key(STATE_RECORD, Namespace::System, &id))?
            .map(|bytes| Ok(u64::from_le_bytes(bytes.as_ref().try_into()?)))
            .transpose()
            .map(|value| value.unwrap_or_default())
    }

    pub fn live_cell(&self, point: &OutPoint) -> Result<Option<LiveCell>> {
        let id = record::cell_key(&self.cells, point);
        self.db
            .get(state_key(STATE_RECORD, Namespace::Cells, &id))?
            .map(|bytes| {
                let (stored_point, record) = record::decode_cell(&bytes)?;
                ensure!(
                    &stored_point == point,
                    "Live Cell key differs from its record"
                );
                Ok(record)
            })
            .transpose()
    }

    pub fn prove_cell(&self, point: &OutPoint) -> Result<(Option<LiveCell>, Proof)> {
        let key = record::cell_key(&self.cells, point);
        let mut tree = self.cells.edit(as_key(&self.tip.state.cell_root), |id| {
            self.node(Namespace::Cells, id)
        });
        let (actual, proof) = tree.prove(&key)?;
        let record = self.live_cell(point)?;
        ensure!(
            actual
                == record
                    .as_ref()
                    .map(|record| record::cell_value(&self.cells, point, record)),
            "Live Cell record differs from its authenticated value"
        );
        Ok((record, proof))
    }

    pub fn header(&self, height: u64) -> Result<Option<Header>> {
        self.db
            .get(index_key(HEADER, &height.to_be_bytes()))?
            .map(|bytes| Header::decode(&bytes))
            .transpose()
    }

    pub fn execution(&self, height: u64) -> Result<Option<(Vec<Transaction>, Vec<Receipt>)>> {
        let Some(bytes) = self.db.get(index_key(EXECUTION, &height.to_be_bytes()))? else {
            return Ok(None);
        };
        let mut cursor = bytes.as_ref();
        let count = take_u32(&mut cursor)?;
        ensure!(
            count <= self.limits.transaction_count,
            "invalid execution item count"
        );
        let mut transactions = Vec::new();
        let mut receipts = Vec::new();
        for _ in 0..count {
            transactions.push(Transaction::from_slice(take_record(&mut cursor)?)?);
            receipts.push(Receipt::decode(take_record(&mut cursor)?)?);
        }
        ensure!(cursor.is_empty(), "trailing block execution bytes");
        Ok(Some((transactions, receipts)))
    }

    fn resolve_header(&self, id: &Byte32, candidate: u64) -> Result<Header> {
        let height = self
            .db
            .get(index_key(HEADER_ID, id.as_slice()))?
            .context("unknown Header Dep")?;
        let height = u64::from_be_bytes(height.as_ref().try_into()?);
        let age = candidate
            .checked_sub(height)
            .context("Header Dep from the future")?;
        ensure!(
            age != 0 && age <= self.limits.header_dep_age,
            "Header Dep outside consensus horizon"
        );
        self.header(height)?.context("missing indexed Header")
    }

    fn node(&self, namespace: Namespace, id: &Key) -> Result<Option<Vec<u8>>> {
        Ok(self
            .db
            .get(state_key(STATE_NODE, namespace, id))?
            .map(|bytes| bytes.to_vec()))
    }

    /// Resolve in proposal order, execute Script Groups against immutable
    /// contexts, and prepare one durable batch. Dropping it has no effect.
    pub fn prepare(
        &self,
        transactions: &[Transaction],
        timestamp_ms: u64,
        parallel: bool,
    ) -> Result<Prepared> {
        ensure!(
            transactions.len() <= self.limits.transaction_count as usize,
            "block transaction count exceeded"
        );
        let mut budget = Budget::new(self.limits);
        let mut stake = self.stage_stake()?;
        let mut live = BTreeMap::<OutPoint, Option<LiveCell>>::new();
        let mut receipts = Vec::with_capacity(transactions.len());
        let mut fees = 0u64;
        let total_bytes = transactions
            .iter()
            .try_fold(0u64, |total, tx| {
                total.checked_add(tx.as_slice().len() as u64)
            })
            .context("block transaction size overflow")?;
        ensure!(
            total_bytes <= budget.bytes,
            "block transaction bytes exceeded"
        );
        let context = HeaderContext::after(
            &self.network,
            &self.tip,
            timestamp_ms,
            Tree::transactions(self.network.clone(), transactions)?.root(),
        )?;
        let height = context.height;
        let origin = Origin::new(&self.network, &context);
        for tx in transactions {
            let receipt = self.stage(
                tx,
                &context,
                &origin,
                &mut budget,
                &mut live,
                &mut stake,
                parallel,
            )?;
            fees = fees
                .checked_add(receipt.fee)
                .context("block fee overflow")?;
            receipts.push(receipt);
        }
        let mut batch = Batch::default();
        let mut cells = self.cells.edit(as_key(&self.tip.state.cell_root), |id| {
            self.node(Namespace::Cells, id)
        });
        for (point, record) in live {
            let id = record::cell_key(&self.cells, &point);
            let key = state_key(STATE_RECORD, Namespace::Cells, &id);
            if let Some(record) = record {
                let bytes = record::encode_cell(&point, &record);
                cells.set(id, Some(self.cells.value_hash(&bytes)))?;
                batch.put(key, bytes);
            } else if cells.set(id, None)?.is_some() {
                batch.delete(key);
            }
        }
        let cell_root = cells.root().pack();
        put_nodes(&mut batch, Namespace::Cells, cells.into_writes());
        let mut system = self.system.edit(as_key(&self.tip.state.system_root), |id| {
            self.node(Namespace::System, id)
        });
        if fees != 0 {
            let value = self
                .fee_surplus()?
                .checked_add(fees)
                .context("total extinguished Capacity overflow")?
                .to_le_bytes();
            let id = self.system.record_key(FEE_KEY);
            system.set(id, Some(self.system.value_hash(&value)))?;
            batch.put(state_key(STATE_RECORD, Namespace::System, &id), value);
        }
        let mut validator_updates = Vec::new();
        if let Some((staking, stake)) = self.staking.as_ref().zip(stake) {
            let (records, updates) = staking.finish(stake, height)?;
            validator_updates = updates;
            for (name, value) in records {
                let id = self.system.record_key(name);
                system.set(id, Some(self.system.value_hash(&value)))?;
                batch.put(state_key(STATE_RECORD, Namespace::System, &id), value);
            }
        }
        let system_root = system.root().pack();
        put_nodes(&mut batch, Namespace::System, system.into_writes());
        let mut history = self.history.clone();
        let next = Header::next(
            &self.network,
            &self.tip,
            &mut history,
            context,
            cell_root,
            system_root,
            Receipt::root(&self.network, &receipts)?,
        )?;
        put_tip(&mut batch, &self.network, &next, &history);
        batch.put(
            index_key(EXECUTION, &height.to_be_bytes()),
            encode_execution(transactions, &receipts)?,
        );
        if let Retention::Recent(window) = self.retention
            && let Some(pruned) = height.checked_sub(window)
        {
            if let Some(header) = self.header(pruned)? {
                batch.delete(index_key(HEADER_ID, header.id(&self.network).as_slice()));
                batch.delete(index_key(HEADER, &pruned.to_be_bytes()));
            }
            batch.delete(index_key(EXECUTION, &pruned.to_be_bytes()));
        }
        Ok(Prepared {
            parent: self.tip.id(&self.network),
            next,
            history,
            receipts,
            validator_updates,
            batch,
        })
    }

    /// The proposer filters stale entries and conflicts in the supplied order.
    /// Final proposal verification binds creation records to its actual transaction root.
    pub fn select(
        &self,
        transactions: Vec<Transaction>,
        timestamp_ms: u64,
        max_bytes: u64,
    ) -> Result<Vec<Transaction>> {
        let mut budget = Budget::new(self.limits);
        let mut stake = self.stage_stake()?;
        let mut wire_bytes = max_bytes;
        let context =
            HeaderContext::after(&self.network, &self.tip, timestamp_ms, Byte32::default())?;
        let origin = Origin::new(&self.network, &context);
        let mut live = BTreeMap::new();
        let mut selected = Vec::new();
        for tx in transactions {
            if selected.len() == self.limits.transaction_count as usize
                || budget.bytes == 0
                || wire_bytes == 0
            {
                break;
            }
            let len = tx.as_slice().len();
            let entry_bytes = (1 + prost::length_delimiter_len(len) + len) as u64;
            let Some(next_wire) = wire_bytes.checked_sub(entry_bytes) else {
                continue;
            };
            if self
                .stage(
                    &tx,
                    &context,
                    &origin,
                    &mut budget,
                    &mut live,
                    &mut stake,
                    true,
                )
                .is_ok()
            {
                wire_bytes = next_wire;
                selected.push(tx);
            }
        }
        Ok(selected)
    }

    pub fn check(&self, tx: &Transaction) -> Result<Receipt> {
        let context = HeaderContext::after(
            &self.network,
            &self.tip,
            self.tip.context.timestamp_ms,
            Byte32::default(),
        )?;
        self.stage(
            tx,
            &context,
            &Origin::new(&self.network, &context),
            &mut Budget::new(self.limits),
            &mut BTreeMap::new(),
            &mut self.stage_stake()?,
            false,
        )
    }

    fn stage(
        &self,
        tx: &Transaction,
        context: &HeaderContext,
        origin: &Origin,
        budget: &mut Budget,
        live: &mut BTreeMap<OutPoint, Option<LiveCell>>,
        stake: &mut Option<StagedStake>,
        parallel: bool,
    ) -> Result<Receipt> {
        let mut next = *budget;
        charge(
            &mut next.bytes,
            tx.as_slice().len() as u64,
            "block transaction bytes",
        )?;
        if matches!(tx.operation(), Some(Operation::StakeSnapshot(_))) {
            let (staking, stake) = self
                .staking
                .as_ref()
                .zip(stake.as_mut())
                .context("native staking is not configured")?;
            let receipt = staking.verify(self, context.height, tx, stake)?;
            *budget = next;
            return Ok(receipt);
        }
        let resolved = ResolvedTransaction::resolve(
            self.network.clone(),
            tx.clone(),
            BlockContext {
                candidate: context,
                parent_timestamp_ms: self.tip.context.timestamp_ms,
                capacity: self.capacity,
            },
            |point| {
                match live.get(point) {
                    Some(staged) => staged.clone(),
                    None => self.live_cell(point)?,
                }
                .context("input or dependency is not a Live Cell")
            },
            |hash| self.resolve_header(hash, context.height),
        )?;
        charge(
            &mut next.reads,
            resolved.cell_reads() as u64,
            "block Cell reads",
        )?;
        charge(
            &mut next.read_bytes,
            resolved.resolved_cell_bytes() as u64,
            "block Cell read bytes",
        )?;
        let id = tx.id(&self.network);
        let outputs = resolved
            .outputs()
            .iter()
            .enumerate()
            .map(|(index, cell)| {
                charge(
                    &mut next.write_bytes,
                    (OutPoint::TOTAL_SIZE + Origin::LEN + cell.as_slice().len()) as u64,
                    "block Cell write bytes",
                )?;
                let point = OutPoint::new(id.clone(), u32::try_from(index)?);
                ensure!(
                    !live.contains_key(&point) && self.live_cell(&point)?.is_none(),
                    "duplicate output OutPoint"
                );
                Ok((
                    point,
                    Some(LiveCell {
                        cell: cell.clone(),
                        origin: origin.clone(),
                    }),
                ))
            })
            .collect::<Result<Vec<_>>>()?;
        let fee = resolved.fee();
        let cycle_limit = self.limits.transaction_cycles.min(next.cycles);
        let cycles = self
            .cache
            .verify(Arc::new(resolved), cycle_limit, parallel)?;
        charge(&mut next.cycles, cycles, "block VM cycles")?;

        live.extend(tx.inputs().map(|input| (input.previous_output, None)));
        live.extend(outputs);
        *budget = next;
        Ok(Receipt {
            fee,
            cycles,
            protocol_result: Default::default(),
        })
    }

    pub fn commit(&mut self, prepared: Prepared) -> Result<()> {
        ensure!(
            prepared.parent == self.tip.id(&self.network),
            "stale prepared block"
        );
        sync(&self.db, &prepared.batch)?;
        self.history = prepared.history;
        self.tip = prepared.next;
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Budget {
    bytes: u64,
    cycles: u64,
    reads: u64,
    read_bytes: u64,
    write_bytes: u64,
}

impl Budget {
    fn new(limits: Limits) -> Self {
        Self {
            bytes: limits.transaction_bytes,
            cycles: limits.block_cycles,
            reads: limits.cell_reads,
            read_bytes: limits.cell_read_bytes,
            write_bytes: limits.cell_write_bytes,
        }
    }
}

fn charge(remaining: &mut u64, amount: u64, name: &str) -> Result<()> {
    *remaining = remaining
        .checked_sub(amount)
        .with_context(|| format!("{name} exceeded"))?;
    Ok(())
}

fn as_key(id: &Byte32) -> Key {
    id.as_slice().try_into().expect("Byte32")
}

fn state_key(prefix: u8, namespace: Namespace, id: &Key) -> [u8; 34] {
    let mut bytes = [0; 34];
    bytes[..2].copy_from_slice(&[prefix, namespace.tag()]);
    bytes[2..].copy_from_slice(id);
    bytes
}

fn index_key(prefix: u8, suffix: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(1 + suffix.len());
    bytes.push(prefix);
    bytes.extend_from_slice(suffix);
    bytes
}

fn put_nodes(batch: &mut Batch, namespace: Namespace, nodes: BTreeMap<Key, Option<Vec<u8>>>) {
    for (id, value) in nodes {
        let key = state_key(STATE_NODE, namespace, &id);
        match value {
            Some(value) => batch.put(key, value),
            None => batch.delete(key),
        }
    }
}

fn put_tip(batch: &mut Batch, network: &Byte32, header: &Header, history: &History) {
    let data = header.encode();
    let height = header.context.height.to_be_bytes();
    batch.put(TIP, &data);
    batch.put(HISTORY, history.encode());
    batch.put(index_key(HEADER, &height), &data);
    batch.put(index_key(HEADER_ID, header.id(network).as_slice()), height);
}

fn encode_execution(transactions: &[Transaction], receipts: &[Receipt]) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::try_from(transactions.len())?.to_le_bytes());
    for (tx, receipt) in transactions.iter().zip(receipts) {
        for item in [tx.as_slice(), receipt.encode().as_ref()] {
            bytes.extend_from_slice(&u32::try_from(item.len())?.to_le_bytes());
            bytes.extend_from_slice(item);
        }
    }
    Ok(bytes)
}

fn take_u32(cursor: &mut &[u8]) -> Result<u32> {
    let (prefix, rest) = cursor
        .split_at_checked(4)
        .context("truncated execution length")?;
    *cursor = rest;
    Ok(u32::from_le_bytes(prefix.try_into()?))
}

fn take_record<'a>(cursor: &mut &'a [u8]) -> Result<&'a [u8]> {
    let length = take_u32(cursor)? as usize;
    let (item, rest) = cursor
        .split_at_checked(length)
        .context("truncated execution record")?;
    *cursor = rest;
    Ok(item)
}

#[derive(Default)]
struct Batch(Vec<(Vec<u8>, Option<Vec<u8>>)>);

impl Batch {
    fn put(&mut self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
        self.0
            .push((key.as_ref().to_vec(), Some(value.as_ref().to_vec())));
    }

    fn delete(&mut self, key: impl AsRef<[u8]>) {
        self.0.push((key.as_ref().to_vec(), None));
    }
}

fn sync(db: &DB, batch: &Batch) -> Result<()> {
    let mut raw = WriteBatch::default();
    for (key, entry) in &batch.0 {
        match entry {
            Some(value) => raw.put(key, value)?,
            None => raw.delete(key)?,
        }
    }
    let mut options = WriteOptions::default();
    options.set_sync(true);
    db.write_opt(&raw, &options)?;
    Ok(())
}
