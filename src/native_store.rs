//! Atomic persistence for the native ledger. Both state trees share the same
//! column family and commit batch as the header, execution data, and MMR peaks.

use std::{
    collections::{BTreeMap, HashMap},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{DB, DBVector, Options, WriteBatch, WriteOptions, prelude::*};
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::*,
};
use rayon::prelude::*;
use stem_ledger::{
    BlockContext, CapacityPolicy, Cell, LiveCell, Origin, ResolvedTransaction, Transaction,
    block::{Header, HeaderContext, Receipt, StateCommitment},
    history::History,
    patricia::{Key, Namespace, Patricia, Proof},
    proof::Tree,
    protocol::Operation,
    record,
};
use stem_stake::jolteon::{COMMITTEE_RECORD, CommitteeChange};
use stem_vm::VerificationCache;

mod bridge;
mod cell_query;
pub use cell_query::{CellPage, ScriptQuery, find_cells};
mod history_index;
pub use history_index::HistoryIndex;
mod keeper;
pub use keeper::KeeperPolicy;
mod lineage;
pub use lineage::LineageProof;
mod staking;
mod state_snapshot;

use bridge::{Bridge, BridgeChanges, StagedBridge};
pub use bridge::{BridgeGenesis, LaneGenesis, RecipientLock, lane_id};
pub use staking::StakeGenesis;
use staking::{StagedStake, Staking};
pub use state_snapshot::{SnapshotReader, StateSnapshot};

const MARKER: &[u8] = b"\0stem/native-spec";
const TIP: &[u8] = b"\0stem/tip";
const HISTORY: &[u8] = b"\0stem/history";
const EXECUTION: u8 = b'e';
const HEADER: u8 = b'h';
const HEADER_ID: u8 = b'i';
const STATE_NODE: u8 = b'n';
const STATE_RECORD: u8 = b's';
const FEE_KEY: &[u8] = b"fees/extinguished";
#[derive(Debug)]
struct StateReadFault;

impl std::fmt::Display for StateReadFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cannot read committed Stem state")
    }
}

impl std::error::Error for StateReadFault {}

const PARAMETER_KEY: &[u8] = b"genesis/network-parameters";

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
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

/// Shannons per deterministic unit of transaction work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceFees {
    pub encoded_byte: u64,
    pub cycle: u64,
    pub cell_read: u64,
    pub cell_read_byte: u64,
    pub cell_write_byte: u64,
    pub cell_delete: u64,
}

impl ResourceFees {
    fn encode(self) -> [u8; 48] {
        let mut bytes = [0; 48];
        for (out, price) in bytes.chunks_exact_mut(8).zip([
            self.encoded_byte,
            self.cycle,
            self.cell_read,
            self.cell_read_byte,
            self.cell_write_byte,
            self.cell_delete,
        ]) {
            out.copy_from_slice(&price.to_le_bytes());
        }
        bytes
    }

    fn cycle_budget(
        self,
        resolved: &ResolvedTransaction,
        encoded_bytes: u64,
        reads: u64,
        read_bytes: u64,
        write_bytes: u64,
        reserved_fee: u64,
    ) -> Result<u64> {
        let fixed = [
            (self.encoded_byte, encoded_bytes),
            (self.cell_read, reads),
            (self.cell_read_byte, read_bytes),
            (self.cell_write_byte, write_bytes),
            (self.cell_delete, resolved.inputs().len() as u64),
        ]
        .into_iter()
        .try_fold(0u64, |sum, (price, units)| {
            price
                .checked_mul(units)
                .and_then(|cost| sum.checked_add(cost))
                .context("resource fee overflow")
        })?;
        let cycle_balance = resolved
            .fee()
            .checked_sub(reserved_fee)
            .and_then(|fee| fee.checked_sub(fixed))
            .context("insufficient fee for transaction resources")?;
        Ok(cycle_balance.checked_div(self.cycle).unwrap_or(u64::MAX))
    }
}

pub struct Spec {
    pub chain_id: String,
    pub development_validators: Vec<([u8; 32], i64)>,
    pub stake: Option<StakeGenesis>,
    pub bridge: Option<BridgeGenesis>,
    pub lineage_type: Option<Byte32>,
    pub keeper: Option<KeeperPolicy>,
    pub network: Byte32,
    pub genesis_time_ms: u64,
    pub capacity: CapacityPolicy,
    pub limits: Limits,
    pub fees: ResourceFees,
    /// Explicit research-network fixture allocations and reference programs.
    pub development_cells: Vec<(OutPoint, Cell)>,
}

#[derive(Clone, Copy)]
pub enum Retention {
    Recent(u64),
    Archive,
}

pub struct Store {
    db: Arc<DB>,
    network: Byte32,
    chain_id: String,
    genesis_validators: Vec<([u8; 32], i64)>,
    staking: Option<Arc<Staking>>,
    bridge: Option<Arc<Bridge>>,
    lineage: Option<Byte32>,
    keeper: Option<KeeperPolicy>,
    genesis_id: Byte32,
    tip: Header,
    history: History,
    cells: Patricia,
    system: Patricia,
    retention: Retention,
    capacity: CapacityPolicy,
    limits: Limits,
    fees: ResourceFees,
    cache: Arc<VerificationCache>,
    branch: Vec<Arc<Prepared>>,
    live_cache: Mutex<LiveCache>,
}

#[derive(Default)]
struct LiveCache {
    records: HashMap<OutPoint, LiveCell>,
    bytes: usize,
}

impl LiveCache {
    fn insert(&mut self, point: OutPoint, cell: LiveCell) {
        const MAX_BYTES: usize = 16 * 1024 * 1024;
        const MAX_ITEMS: usize = 4096;
        if self.records.contains_key(&point) {
            return;
        }
        let bytes = cell.cell.as_slice().len() + Origin::LEN + OutPoint::TOTAL_SIZE;
        if self.bytes + bytes > MAX_BYTES || self.records.len() == MAX_ITEMS {
            self.clear();
        }
        self.bytes += bytes;
        self.records.insert(point, cell);
    }

    fn clear(&mut self) {
        self.records.clear();
        self.bytes = 0;
    }
}

pub struct Prepared {
    parent: Byte32,
    next: Header,
    history: History,
    receipts: Vec<Receipt>,
    keeper_receipts: usize,
    validator_updates: Vec<stem_stake::Member>,
    committee_change: Option<CommitteeChange>,
    batch: Batch,
}

struct Overlay {
    live: BTreeMap<OutPoint, Option<LiveCell>>,
    stake: Option<StagedStake>,
    bridge: Option<StagedBridge>,
    lineages: lineage::Heads,
    keeper: keeper::Stage,
}

impl Prepared {
    pub fn header(&self) -> &Header {
        &self.next
    }

    pub fn receipts(&self) -> &[Receipt] {
        &self.receipts
    }
    /// CometBFT expects results only for transactions submitted with the proposal.
    pub fn submitted_receipts(&self) -> &[Receipt] {
        &self.receipts[self.keeper_receipts..]
    }

    pub fn validator_updates(&self) -> &[stem_stake::Member] {
        &self.validator_updates
    }

    pub(crate) fn committee_change(&self) -> Option<&CommitteeChange> {
        self.committee_change.as_ref()
    }
}

impl Store {
    pub fn open(path: &Path, spec: &Spec, retention: Retention) -> Result<Self> {
        ensure!(
            spec.network != Byte32::default(),
            "zero Stem network identity"
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
            "invalid Stem chain ID"
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
                .members
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
        if let Some(marker) = &spec.lineage_type {
            ensure!(
                *marker != Byte32::default(),
                "zero Lineage Type Script code hash"
            );
        }
        let mut allocations = Vec::with_capacity(spec.development_cells.len());
        spec.development_cells
            .iter()
            .try_fold(0u64, |sum, (point, cell)| -> Result<u64> {
                ensure!(!point.is_null(), "null genesis allocation OutPoint");
                ensure!(
                    spec.lineage_type.as_ref().is_none_or(|code| !cell
                        .type_scripts()
                        .any(|script| lineage::is_marker(code, &script))),
                    "Lineages require post-genesis registration"
                );
                spec.capacity.verify(cell)?;
                let cell = LiveCell {
                    cell: cell.clone(),
                    origin: origin.clone(),
                };
                let id = record::cell_key(&cells, point);
                let record = record::encode_cell(point, &cell);
                allocations.push((id, Some(cells.value_hash(&record))));
                batch.put(state_key(STATE_RECORD, Namespace::Cells, &id), record);
                sum.checked_add(cell.cell.capacity())
                    .context("genesis Capacity overflow")
            })?;
        allocations.sort_unstable_by_key(|(key, _)| *key);
        cell_edit
            .set_batch(&allocations, |_, _, _| {})
            .context("invalid genesis allocations")?;
        let mut system_edit = system.edit(system.empty_root(), |_| Ok(None));
        let mut params = spec.limits.encode();
        params.extend_from_slice(&spec.fees.encode());
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
        if let Some(marker) = &spec.lineage_type {
            let id = system.record_key(b"lineage/type");
            system_edit.set(id, Some(system.value_hash(marker.as_slice())))?;
            batch.put(
                state_key(STATE_RECORD, Namespace::System, &id),
                marker.as_slice(),
            );
        }
        if let Some(policy) = spec.keeper {
            ensure!(
                spec.lineage_type.is_some(),
                "Keeper requires configured Lineages"
            );
            policy.validate(spec.limits)?;
            let (name, value) = policy.record();
            let id = system.record_key(name);
            system_edit.set(id, Some(system.value_hash(&value)))?;
            batch.put(state_key(STATE_RECORD, Namespace::System, &id), value);
        }
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
            if let Some(genesis) = &spec.stake {
                for key in genesis.registry.retired_keys(0) {
                    let name = staking.retirement_key(&network, key);
                    let id = system.record_key(name.as_slice());
                    let effective = 1u64.to_le_bytes();
                    system_edit.set(id, Some(system.value_hash(&effective)))?;
                    batch.put(state_key(STATE_RECORD, Namespace::System, &id), effective);
                }
            }
            Some(staking)
        } else {
            None
        };
        let bridge = if let Some(config) = &spec.bridge {
            ensure!(
                spec.development_cells.is_empty(),
                "a bridged network cannot assign development Capacity"
            );
            let (bridge, records) = Bridge::initialize(config)?;
            for (name, value) in records {
                let value = value.context("bridge genesis is missing a record value")?;
                let id = system.record_key(&name);
                system_edit.set(id, Some(system.value_hash(&value)))?;
                batch.put(state_key(STATE_RECORD, Namespace::System, &id), value);
            }
            Some(bridge)
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
            let tip = Header::decode(&db.get(TIP)?.context("missing Stem tip")?)?;
            history = History::decode(
                network.clone(),
                &db.get(HISTORY)?.context("missing Stem history frontier")?,
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
            db: Arc::new(db),
            network,
            chain_id: spec.chain_id.clone(),
            genesis_validators: validators,
            staking: staking.map(Arc::new),
            bridge: bridge.map(Arc::new),
            lineage: spec.lineage_type.clone(),
            keeper: spec.keeper,
            genesis_id,
            tip,
            history,
            cells,
            system,
            retention,
            capacity: spec.capacity,
            limits: spec.limits,
            fees: spec.fees,
            live_cache: Mutex::default(),
            cache: Arc::new(VerificationCache::new(4096)),
            branch: Vec::new(),
        })
    }

    pub fn tip(&self) -> &Header {
        &self.tip
    }

    pub fn consensus_retain_height(&self) -> u64 {
        match self.retention {
            Retention::Archive => 0,
            Retention::Recent(window) => self
                .tip
                .context
                .height
                .checked_sub(window)
                .map_or(0, |height| height + 1),
        }
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

    /// Run the normal state machine against a contiguous, uncommitted branch.
    /// The closure borrows the view while the committed store is read-only.
    pub fn on_branch<T>(
        &self,
        branch: &[Arc<Prepared>],
        task: impl FnOnce(&Store) -> Result<T>,
    ) -> Result<T> {
        if branch.is_empty() {
            return task(self);
        }
        ensure!(
            self.branch.is_empty(),
            "candidate view cannot own a nested branch"
        );
        let mut tip = &self.tip;
        for candidate in branch {
            ensure!(
                candidate.parent == tip.id(&self.network)
                    && tip.context.height.checked_add(1) == Some(candidate.next.context.height),
                "noncontiguous candidate state branch",
            );
            tip = &candidate.next;
        }
        let view = Self {
            db: self.db.clone(),
            network: self.network.clone(),
            chain_id: self.chain_id.clone(),
            genesis_validators: self.genesis_validators.clone(),
            staking: self.staking.clone(),
            bridge: self.bridge.clone(),
            lineage: self.lineage.clone(),
            keeper: self.keeper,
            genesis_id: self.genesis_id.clone(),
            tip: tip.clone(),
            history: branch.last().unwrap().history.clone(),
            cells: self.cells.clone(),
            system: self.system.clone(),
            retention: self.retention,
            capacity: self.capacity,
            limits: self.limits,
            fees: self.fees,
            cache: self.cache.clone(),
            branch: branch.to_vec(),
            live_cache: Mutex::default(),
        };
        task(&view)
    }

    fn read(&self, key: impl AsRef<[u8]>) -> Result<Option<ReadValue<'_>>> {
        let key = key.as_ref();
        for prepared in self.branch.iter().rev() {
            if let Some(value) = prepared.batch.lookup(key) {
                return Ok(value.as_deref().map(ReadValue::Candidate));
            }
        }
        Ok(self.db.get(key)?.map(ReadValue::Committed))
    }

    pub(crate) fn consensus_record(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.read(index_key(b'j', key))?.map(|value| value.to_vec()))
    }

    pub(crate) fn persist_consensus(&self, records: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
        ensure!(self.branch.is_empty(), "cannot persist a speculative view");
        let mut batch = Batch::default();
        for (key, value) in records {
            batch.put(index_key(b'j', key), value);
        }
        sync(&self.db, &batch)
    }

    fn system_record(&self, name: &[u8]) -> Result<Option<Vec<u8>>> {
        let id = self.system.record_key(name);
        Ok(self
            .read(state_key(STATE_RECORD, Namespace::System, &id))
            .context(StateReadFault)?
            .map(|value| value.to_vec()))
    }

    pub fn prove_stake_snapshot(&self) -> Result<(Option<Vec<u8>>, Proof)> {
        self.prove_system(staking::SNAPSHOT)
    }

    pub(crate) fn prove_stake_epoch_end(&self, epoch: u64) -> Result<(Option<u64>, Proof)> {
        let (record, path) =
            self.prove_system(&stem_stake::jolteon::evidence::epoch_end_key(epoch))?;
        let end = record
            .map(|bytes| -> Result<_> { Ok(u64::from_le_bytes(bytes.as_slice().try_into()?)) })
            .transpose()?;
        Ok((end, path))
    }

    pub(crate) fn announced_committee(&self) -> Result<Option<CommitteeChange>> {
        Ok(self
            .system_record(COMMITTEE_RECORD)?
            .as_deref()
            .map(CommitteeChange::decode)
            .transpose()?)
    }

    pub(crate) fn stake_committee_limit(&self) -> Option<usize> {
        self.staking
            .as_ref()
            .map(|staking| staking.max_validators())
    }

    pub(crate) fn prove_announced_committee(&self) -> Result<(Vec<u8>, Proof)> {
        let (record, proof) = self.prove_system(COMMITTEE_RECORD)?;
        Ok((record.context("missing announced committee")?, proof))
    }

    pub fn prove_extinguished_fees(&self) -> Result<(Option<u64>, Proof)> {
        let (record, proof) = self.prove_system(FEE_KEY)?;
        let amount = record
            .as_deref()
            .map(decode_extinguished_fees)
            .transpose()?;
        Ok((amount, proof))
    }

    pub fn prove_stake_retirement(&self, validator: [u8; 32]) -> Result<(Option<u64>, Proof)> {
        let staking = self
            .staking
            .as_ref()
            .context("native staking is not configured")?;
        let key = staking.retirement_key(&self.network, validator);
        let (record, proof) = self.prove_system(key.as_slice())?;
        let height = record
            .map(|bytes| -> Result<_> { Ok(u64::from_le_bytes(bytes.as_slice().try_into()?)) })
            .transpose()?;
        Ok((height, proof))
    }

    fn prove_system(&self, name: &[u8]) -> Result<(Option<Vec<u8>>, Proof)> {
        let key = self.system.record_key(name);
        let mut tree = self.system.edit(as_key(&self.tip.state.system_root), |id| {
            self.node(Namespace::System, id)
        });
        let (actual, proof) = tree.prove(&key)?;
        let record = self.system_record(name)?;
        ensure!(
            actual == record.as_ref().map(|record| self.system.value_hash(record)),
            "protocol state differs from its authenticated record"
        );
        Ok((record, proof))
    }

    pub fn verify_stake_snapshot(
        network: &Byte32,
        header: &Header,
        record: Option<&[u8]>,
        proof: &Proof,
    ) -> bool {
        Self::verify_system_record(network, header, staking::SNAPSHOT, record, proof)
    }

    pub fn verify_extinguished_fees(
        network: &Byte32,
        header: &Header,
        amount: Option<u64>,
        proof: &Proof,
    ) -> bool {
        let record = amount.map(u64::to_le_bytes);
        amount != Some(0)
            && Self::verify_system_record(
                network,
                header,
                FEE_KEY,
                record.as_ref().map(|bytes| bytes.as_slice()),
                proof,
            )
    }

    pub fn verify_stake_retirement(
        network: &Byte32,
        header: &Header,
        registry: [u8; 32],
        validator: [u8; 32],
        effective: u64,
        proof: &Proof,
    ) -> bool {
        stem_stake::retirement::verify_marker(
            &network.unpack(),
            &registry,
            &validator,
            effective,
            header.context.height,
            &header.state.system_root.unpack(),
            proof.nodes.iter().map(Vec::as_slice),
        )
        .is_ok()
    }

    fn verify_system_record(
        network: &Byte32,
        header: &Header,
        name: &[u8],
        record: Option<&[u8]>,
        proof: &Proof,
    ) -> bool {
        let tree = Patricia::new(network.clone(), Namespace::System);
        let key = tree.record_key(name);
        tree.verify(
            &as_key(&header.state.system_root),
            &key,
            record.map(|value| tree.value_hash(value)).as_ref(),
            proof,
        )
    }

    fn stage_stake(&self) -> Result<Option<StagedStake>> {
        self.staking
            .as_ref()
            .map(|staking| staking.load(self))
            .transpose()
    }

    pub fn fee_surplus(&self) -> Result<u64> {
        let id = self.system.record_key(FEE_KEY);
        self.read(state_key(STATE_RECORD, Namespace::System, &id))?
            .map(|bytes| decode_extinguished_fees(bytes.as_ref()))
            .transpose()
            .map(|value| value.unwrap_or_default())
    }

    pub fn live_cell(&self, point: &OutPoint) -> Result<Option<LiveCell>> {
        if let Some(cell) = self
            .live_cache
            .lock()
            .expect("Live Cell cache")
            .records
            .get(point)
            .cloned()
        {
            return Ok(Some(cell));
        }
        let id = record::cell_key(&self.cells, point);
        let found = self
            .read(state_key(STATE_RECORD, Namespace::Cells, &id))
            .context(StateReadFault)?
            .map(|bytes| {
                let (stored_point, record) = record::decode_cell(&bytes)?;
                ensure!(
                    &stored_point == point,
                    "Live Cell key differs from its record"
                );
                Ok(record)
            })
            .transpose()
            .context(StateReadFault)?;
        if let Some(cell) = &found {
            self.live_cache
                .lock()
                .expect("Live Cell cache")
                .insert(point.clone(), cell.clone());
        }
        Ok(found)
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
        self.read(index_key(HEADER, &height.to_be_bytes()))?
            .map(|bytes| Header::decode(&bytes))
            .transpose()
    }

    pub fn execution(&self, height: u64) -> Result<Option<(Vec<Transaction>, Vec<Receipt>)>> {
        let Some(bytes) = self.read(index_key(EXECUTION, &height.to_be_bytes()))? else {
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
            .read(index_key(HEADER_ID, id.as_slice()))?
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
            .read(state_key(STATE_NODE, namespace, id))?
            .map(|value| value.to_vec()))
    }

    /// Resolve conflicts and protocol state in proposal order. Schedule immutable
    /// VM contexts in bounded waves and charge each result in proposal order.
    /// A failure discards the entire block overlay.
    pub fn prepare(
        &self,
        transactions: &[Transaction],
        timestamp_ms: u64,
        parallel: bool,
    ) -> Result<Prepared> {
        let height = self
            .tip
            .context
            .height
            .checked_add(1)
            .context("block height overflow")?;
        let due = self.due(height)?;
        ensure!(
            transactions.len().saturating_add(due.len()) <= self.limits.transaction_count as usize,
            "block transaction count exceeded"
        );
        let mut budget = Budget::new(self.limits);
        let mut overlay = self.overlay()?;
        let with_due =
            (!due.is_empty()).then(|| due.iter().chain(transactions).cloned().collect::<Vec<_>>());
        let execution = with_due.as_deref().unwrap_or(transactions);
        let total_bytes = execution
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
            Tree::transactions(self.network.clone(), execution)?.root(),
        )?;
        let origin = Origin::new(&self.network, &context);
        let mut receipts = self.run_keeper(&due, &context, &origin, &mut budget, &mut overlay)?;
        let mut fees = receipts
            .iter()
            .try_fold(0u64, |sum, receipt| sum.checked_add(receipt.fee))
            .context("Keeper fee overflow")?;
        receipts.reserve(transactions.len());
        let width = if parallel {
            rayon::current_num_threads()
        } else {
            1
        };
        // Account for previous validation before admitting a dynamic wave.
        // Disjoint commands may materialize together when each has its full limit.
        for segment in transactions
            .chunk_by(|left, right| left.has_lineage_command() == right.has_lineage_command())
        {
            for wave in segment.chunks(width) {
                let parallel_commands = wave.len() > 1
                    && wave[0].has_lineage_command()
                    && budget.cycles / wave.len() as u64 >= self.limits.transaction_cycles
                    && lineage::independent(self, wave, &overlay.lineages)?;
                let mut commands = if parallel_commands {
                    wave.par_iter()
                        .map(|tx| self.materialize_lineage(tx, &context, &overlay, budget))
                        .collect::<Result<Vec<_>>>()?
                        .into_iter()
                } else {
                    Vec::new().into_iter()
                };
                let mut jobs = Vec::with_capacity(wave.len());
                for tx in wave {
                    let position = receipts.len();
                    let receipt = self.stage(
                        tx,
                        &context,
                        &origin,
                        &mut budget,
                        &mut overlay,
                        commands.next().flatten(),
                        |resolved, limit| {
                            if width == 1 {
                                self.cache.verify(resolved, limit, false)
                            } else {
                                jobs.push((position, resolved, limit));
                                Ok(0)
                            }
                        },
                    )?;
                    fees = fees
                        .checked_add(receipt.fee)
                        .context("block fee overflow")?;
                    receipts.push(receipt);
                }

                let group_parallel = jobs.len() < width;
                let completed: Vec<_> = jobs
                    .into_par_iter()
                    .map(|(position, resolved, limit)| {
                        (position, self.cache.verify(resolved, limit, group_parallel))
                    })
                    .collect();
                for (position, verified) in completed {
                    let cycles = verified?;
                    charge(&mut budget.cycles, cycles, "block VM cycles")?;
                    receipts[position].cycles = receipts[position]
                        .cycles
                        .checked_add(cycles)
                        .context("receipt cycles overflow")?;
                }
            }
        }
        let mut batch = Batch::default();
        let mut cells = self.cells.edit(as_key(&self.tip.state.cell_root), |id| {
            self.node(Namespace::Cells, id)
        });
        let mut cell_changes = Vec::with_capacity(overlay.live.len());
        for (point, record) in overlay.live {
            let id = record::cell_key(&self.cells, &point);
            let value = record.map(|record| {
                let bytes = record::encode_cell(&point, &record);
                let hash = self.cells.value_hash(&bytes);
                batch.put(state_key(STATE_RECORD, Namespace::Cells, &id), bytes);
                hash
            });
            cell_changes.push((id, value));
        }
        cell_changes.sort_unstable_by_key(|(id, _)| *id);
        cells.set_batch(&cell_changes, |id, _, value| {
            if value.is_none() {
                batch.delete(state_key(STATE_RECORD, Namespace::Cells, &id));
            }
        })?;
        let cell_root = cells.root().pack();
        put_nodes(&mut batch, Namespace::Cells, cells.into_writes());
        let mut system = self.system.edit(as_key(&self.tip.state.system_root), |id| {
            self.node(Namespace::System, id)
        });
        let mut system_changes = Vec::new();
        let total_fees = if fees != 0 {
            let total = self
                .fee_surplus()?
                .checked_add(fees)
                .context("total extinguished Capacity overflow")?;
            let value = total.to_le_bytes();
            let id = self.system.record_key(FEE_KEY);
            system_changes.push((id, Some(self.system.value_hash(&value))));
            batch.put(state_key(STATE_RECORD, Namespace::System, &id), value);
            Some(total)
        } else {
            None
        };
        let mut validator_updates = Vec::new();
        let mut committee_change = None;
        if let Some((staking, stake)) = self.staking.as_ref().zip(overlay.stake) {
            let (records, updates) = staking.finish(stake, height)?;
            validator_updates = updates;
            for (name, value) in records {
                if name == COMMITTEE_RECORD {
                    committee_change = Some(CommitteeChange::decode(&value)?);
                }
                let id = self.system.record_key(&name);
                system_changes.push((id, Some(self.system.value_hash(&value))));
                batch.put(state_key(STATE_RECORD, Namespace::System, &id), value);
            }
        }
        if let Some(bridge) = overlay.bridge {
            for (name, value) in bridge.finish(self, fees, total_fees)? {
                let id = self.system.record_key(&name);
                let hash = value.map(|value| {
                    let hash = self.system.value_hash(&value);
                    batch.put(state_key(STATE_RECORD, Namespace::System, &id), value);
                    hash
                });
                system_changes.push((id, hash));
            }
        }
        for (id, head) in overlay.lineages {
            let id = self.system.record_key(&lineage::record_key(&id));
            let hash = head.map(|head| {
                let value = head.encode();
                let hash = self.system.value_hash(&value);
                batch.put(state_key(STATE_RECORD, Namespace::System, &id), value);
                hash
            });
            system_changes.push((id, hash));
        }
        for (name, value) in overlay.keeper.finish() {
            let id = self.system.record_key(&name);
            let hash = value.map(|bytes| {
                let hash = self.system.value_hash(&bytes);
                batch.put(state_key(STATE_RECORD, Namespace::System, &id), bytes);
                hash
            });
            system_changes.push((id, hash));
        }
        system_changes.sort_unstable_by_key(|(id, _)| *id);
        system.set_batch(&system_changes, |id, _, value| {
            if value.is_none() {
                batch.delete(state_key(STATE_RECORD, Namespace::System, &id));
            }
        })?;
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
            encode_execution(execution, &receipts)?,
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
            keeper_receipts: due.len(),
            validator_updates,
            committee_change,
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
        let mut overlay = self.overlay()?;
        let mut wire_bytes = max_bytes;
        let context =
            HeaderContext::after(&self.network, &self.tip, timestamp_ms, Byte32::default())?;
        let origin = Origin::new(&self.network, &context);
        let due = self.due(context.height)?;
        self.run_keeper(&due, &context, &origin, &mut budget, &mut overlay)?;
        let mut selected = Vec::new();
        for tx in transactions {
            if selected.len() + due.len() == self.limits.transaction_count as usize
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
            match self.stage(
                &tx,
                &context,
                &origin,
                &mut budget,
                &mut overlay,
                None,
                |resolved, limit| self.cache.verify(resolved, limit, true),
            ) {
                Ok(_) => {
                    wire_bytes = next_wire;
                    selected.push(tx);
                }
                Err(error) if error.is::<StateReadFault>() => return Err(error),
                Err(_) => {}
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
            &mut self.overlay()?,
            None,
            |resolved, limit| self.cache.verify(resolved, limit, false),
        )
    }

    fn overlay(&self) -> Result<Overlay> {
        Ok(Overlay {
            live: BTreeMap::new(),
            stake: self.stage_stake()?,
            bridge: self.bridge.as_deref().map(Bridge::stage),
            lineages: BTreeMap::new(),
            keeper: keeper::Stage::default(),
        })
    }

    fn resolve_live(
        &self,
        live: &BTreeMap<OutPoint, Option<LiveCell>>,
        point: &OutPoint,
    ) -> Result<LiveCell> {
        match live.get(point) {
            Some(staged) => staged.clone(),
            None => self.live_cell(point)?,
        }
        .context("input or dependency is not a Live Cell")
    }

    fn materialize_lineage(
        &self,
        tx: &Transaction,
        context: &HeaderContext,
        overlay: &Overlay,
        budget: Budget,
    ) -> Result<Option<lineage::Materialized>> {
        ensure!(
            tx.execute_at()
                .is_none_or(|height| overlay.keeper.running && context.height == height),
            "height-bound Lineage command requires its committed Keeper reservation"
        );
        tx.lineage_command()
            .map(|command| {
                ensure!(
                    context.height < tx.valid_before_height().expect("validated command expiry"),
                    "Lineage command has expired"
                );
                let block = BlockContext {
                    candidate: context,
                    parent: &self.tip,
                    capacity: self.capacity,
                };
                lineage::materialize(
                    self,
                    tx,
                    command,
                    block,
                    &overlay.lineages,
                    budget,
                    |point| self.resolve_live(&overlay.live, point),
                )
            })
            .transpose()
    }

    fn stage(
        &self,
        tx: &Transaction,
        context: &HeaderContext,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
        staged_command: Option<lineage::Materialized>,
        verify: impl FnOnce(Arc<ResolvedTransaction>, u64) -> Result<u64>,
    ) -> Result<Receipt> {
        let mut next = *budget;
        charge(
            &mut next.bytes,
            tx.as_slice().len() as u64,
            "block transaction bytes",
        )?;
        let materialized = if staged_command.is_some() {
            staged_command
        } else {
            self.materialize_lineage(tx, context, overlay, next)?
        };
        let Overlay {
            live,
            stake,
            bridge,
            lineages,
            keeper,
        } = overlay;
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
        if matches!(tx.operation(), Some(Operation::BridgeSettlement(_))) {
            let (bridge, stage) = self
                .bridge
                .as_ref()
                .zip(bridge.as_mut())
                .context("native bridge is not configured")?;
            let changes = bridge.settle(self, stage, tx)?;
            let protocol_result = changes.protocol_result.clone();
            stage.apply(changes);
            *budget = next;
            return Ok(Receipt {
                fee: 0,
                cycles: 0,
                protocol_result,
            });
        }

        let fresh = |point: &OutPoint| self.resolve_live(live, point);
        let header = |hash: &Byte32| self.resolve_header(hash, context.height);
        let block = BlockContext {
            candidate: context,
            parent: &self.tip,
            capacity: self.capacity,
        };
        let concrete = materialized
            .as_ref()
            .map_or(tx, |command| &command.transaction);
        let (resolved, changes) = if matches!(tx.operation(), Some(Operation::BridgeMint(_))) {
            let (bridge, stage) = self
                .bridge
                .as_ref()
                .zip(bridge.as_ref())
                .context("native bridge is not configured")?;
            let (grant, asset, changes) = bridge.mint(self, stage, context.height, tx)?;
            let resolved = ResolvedTransaction::resolve_mint(
                self.network.clone(),
                tx.clone(),
                block,
                &grant,
                asset,
                fresh,
                header,
            )?;
            (resolved, changes)
        } else {
            let resolved = ResolvedTransaction::resolve(
                self.network.clone(),
                concrete.clone(),
                block,
                fresh,
                header,
            )?;
            let changes = match self.bridge.as_ref().zip(bridge.as_ref()) {
                Some((bridge, stage)) => bridge.burn(self, stage, concrete)?,
                None => BridgeChanges::default(),
            };
            let resolved = if let Some(command) = &materialized {
                ensure!(
                    changes.omit_outputs.is_empty(),
                    "Lineage command cannot create a Bridge Burn"
                );
                resolved.authorize_materialized_lineage(tx, &command.fee_cell)?
            } else {
                resolved
            };
            (resolved, changes)
        };
        let keeper_change = if tx.keeper_action().is_some() {
            keeper::change(self, keeper, &resolved, block, &mut next, fresh)?
        } else {
            None
        };
        let keeper_cycles = keeper_change.as_ref().map_or(0, |change| change.cycles);
        let id = tx.id(&self.network);
        let lineage_changes = self
            .lineage
            .as_ref()
            .map(|code| {
                lineage::changes(self, code, lineages, &resolved, &id, &changes.omit_outputs)
            })
            .transpose()?
            .unwrap_or_default();
        let lineage_reads = lineage_changes.len() as u64;
        let reads = resolved.cell_reads() as u64
            + lineage_reads
            + materialized.as_ref().map_or(0, |command| command.reads);
        let read_bytes = resolved.resolved_cell_bytes() as u64
            + lineage_reads
                * (lineage::KEY_BYTES + stem_ledger::lineage::Head::MAX_ENCODED_BYTES as u64)
            + materialized
                .as_ref()
                .map_or(0, |command| command.read_bytes);
        charge(&mut next.reads, reads, "block Cell reads")?;
        charge(&mut next.read_bytes, read_bytes, "block Cell read bytes")?;
        let outputs = resolved
            .outputs()
            .iter()
            .enumerate()
            .filter(|(index, _)| !changes.omit_outputs.contains(index))
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
        let deletion_bytes = resolved.inputs().len() as u64 * OutPoint::TOTAL_SIZE as u64;
        charge(
            &mut next.write_bytes,
            deletion_bytes,
            "block Cell deletion bytes",
        )?;
        let lineage_bytes = lineage_changes
            .values()
            .map(|head| {
                lineage::KEY_BYTES + head.as_ref().map_or(0, |head| head.encode().len() as u64)
            })
            .sum();
        charge(
            &mut next.write_bytes,
            lineage_bytes,
            "block Lineage write bytes",
        )?;
        let cycle_limit = self
            .limits
            .transaction_cycles
            .checked_sub(keeper_cycles)
            .context("Keeper authorization exceeds transaction cycles")?
            .min(next.cycles);
        let cycle_limit = if tx.operation().is_none() {
            cycle_limit.min(
                self.fees
                    .cycle_budget(
                        &resolved,
                        tx.as_slice().len() as u64,
                        budget.reads - next.reads,
                        budget.read_bytes - next.read_bytes,
                        budget.write_bytes - next.write_bytes,
                        keeper_change.as_ref().map_or(0, |change| change.fee),
                    )?
                    .checked_sub(keeper_cycles)
                    .context("Keeper authorization exceeds paid VM cycles")?,
            )
        } else {
            cycle_limit
        };
        let cycles = if let Some(command) = &materialized {
            let remaining = cycle_limit
                .checked_sub(command.cycles)
                .context("Lineage execution exceeds its resource budget")?;
            let validation_cycles = if command.executed {
                let validation_limit = command.validation_cycles;
                ensure!(
                    validation_limit <= remaining,
                    "Lineage fee does not cover the signed validation budget"
                );
                match self
                    .cache
                    .verify(Arc::new(resolved), validation_limit, false)
                {
                    Ok(cycles) => cycles,
                    Err(_) => {
                        let reads_attempted = reads - command.reads;
                        let bytes_attempted = read_bytes - command.read_bytes;
                        let refund = materialized.expect("Lineage command").rollback(
                            validation_limit,
                            reads_attempted,
                            bytes_attempted,
                        )?;
                        return self.stage(
                            tx,
                            context,
                            origin,
                            budget,
                            overlay,
                            Some(refund),
                            verify,
                        );
                    }
                }
            } else {
                // The materializer has already verified this exact fee-only transaction.
                0
            };
            command.cycles + validation_cycles
        } else {
            verify(Arc::new(resolved), cycle_limit)?
        };
        charge(&mut next.cycles, cycles, "block VM cycles")?;

        live.extend(concrete.inputs().map(|input| (input.previous_output, None)));
        live.extend(outputs);
        lineages.extend(lineage_changes);
        if let Some(change) = keeper_change {
            keeper.apply(change);
        }
        let protocol_result = materialized.as_ref().map_or_else(
            || changes.protocol_result.clone(),
            |command| stem_ledger::lineage::Outcome::from_applied(command.executed).encode(),
        );
        if let Some(bridge) = bridge.as_mut() {
            bridge.apply(changes);
        }
        *budget = next;
        Ok(Receipt {
            fee,
            cycles: cycles + keeper_cycles,
            protocol_result,
        })
    }

    pub fn commit(&mut self, prepared: Prepared) -> Result<()> {
        self.commit_certified(&prepared, &[])
    }

    pub(crate) fn commit_certified(
        &mut self,
        prepared: &Prepared,
        records: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<()> {
        ensure!(self.branch.is_empty(), "cannot commit a speculative view");
        ensure!(
            prepared.parent == self.tip.id(&self.network),
            "stale prepared block"
        );
        sync_with_consensus(&self.db, &prepared.batch, records)?;
        self.history = prepared.history.clone();
        self.tip = prepared.next.clone();
        self.live_cache.get_mut().expect("Live Cell cache").clear();
        Ok(())
    }
}

fn decode_extinguished_fees(bytes: &[u8]) -> Result<u64> {
    let amount = u64::from_le_bytes(bytes.try_into()?);
    ensure!(amount != 0, "zero extinguished fee record");
    Ok(amount)
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
    fn empty() -> Self {
        Self {
            bytes: 0,
            cycles: 0,
            reads: 0,
            read_bytes: 0,
            write_bytes: 0,
        }
    }

    fn charge_slot(&mut self, slot: Self, remaining: Self) -> Result<()> {
        charge(
            &mut self.bytes,
            slot.bytes - remaining.bytes,
            "Keeper block bytes",
        )?;
        charge(
            &mut self.cycles,
            slot.cycles - remaining.cycles,
            "Keeper block cycles",
        )?;
        charge(
            &mut self.reads,
            slot.reads - remaining.reads,
            "Keeper block reads",
        )?;
        charge(
            &mut self.read_bytes,
            slot.read_bytes - remaining.read_bytes,
            "Keeper block read bytes",
        )?;
        charge(
            &mut self.write_bytes,
            slot.write_bytes - remaining.write_bytes,
            "Keeper block write bytes",
        )
    }

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

enum ReadValue<'a> {
    Committed(DBVector),
    Candidate(&'a [u8]),
}

impl AsRef<[u8]> for ReadValue<'_> {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Committed(value) => value.as_ref(),
            Self::Candidate(value) => value,
        }
    }
}

impl std::ops::Deref for ReadValue<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

type Write = (Vec<u8>, Option<Vec<u8>>);

#[derive(Default)]
struct Batch {
    writes: Vec<Write>,
    index: OnceLock<HashMap<Vec<u8>, usize>>,
}

impl Batch {
    fn lookup(&self, key: &[u8]) -> Option<&Option<Vec<u8>>> {
        let index = self.index.get_or_init(|| {
            self.writes
                .iter()
                .enumerate()
                .map(|(position, (key, _))| (key.clone(), position))
                .collect()
        });
        index.get(key).map(|&position| &self.writes[position].1)
    }

    fn put(&mut self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
        self.writes
            .push((key.as_ref().to_vec(), Some(value.as_ref().to_vec())));
    }

    fn delete(&mut self, key: impl AsRef<[u8]>) {
        self.writes.push((key.as_ref().to_vec(), None));
    }
}

fn sync(db: &DB, batch: &Batch) -> Result<()> {
    sync_with_consensus(db, batch, &[])
}

fn sync_with_consensus(db: &DB, batch: &Batch, records: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
    let mut raw = WriteBatch::default();
    for (key, entry) in &batch.writes {
        match entry {
            Some(value) => raw.put(key, value)?,
            None => raw.delete(key)?,
        }
    }
    for (key, value) in records {
        raw.put(index_key(b'j', key), value)?;
    }
    let mut options = WriteOptions::default();
    options.set_sync(true);
    db.write_opt(&raw, &options)?;
    Ok(())
}
