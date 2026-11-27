//! Atomic persistence for the native ledger. Both state trees share the same
//! column family and commit batch as the header, execution data, and MMR peaks.

use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet, HashMap},
    ops::Range,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{DB, DBVector, Options, ReadOptions, WriteBatch, WriteOptions, prelude::*};
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
    protocol::{Operation, l1_reference::VerifiedL1References},
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
pub use keeper::{KeeperPolicy, KeeperTimePolicy};
mod lineage;
pub use stem_ledger::lineage::LineageProof;
mod node_storage;
mod staking;
mod state_snapshot;
mod storage_metrics;
pub use storage_metrics::StorageSample;
use storage_metrics::{BatchBytes, Purpose, StorageMetrics};

use bridge::{Bridge, BridgeChanges, StagedBridge};
pub use bridge::{BridgeGenesis, LaneGenesis, RecipientLock, lane_id};
use staking::{StagedStake, StakeInputs, Staking};
pub use staking::{StakeGenesis, stake_registry_policy};
pub use state_snapshot::SnapshotReader;
pub use stem_ledger::snapshot::StateSnapshot;
pub use stem_ledger::protocol::{FeeFundDestination, FeeFundPolicy};

use stem_ledger::light::EXTINGUISHED_FEES_KEY as FEE_KEY;

const MARKER: &[u8] = b"\0stem/native-spec";
const STORAGE_LAYOUT: &[u8] = b"Stem Patricia prefix storage\0";
const TIP: &[u8] = b"\0stem/tip";
const HISTORY: &[u8] = b"\0stem/history";
const EXECUTION: u8 = b'e';
const HEADER: u8 = b'h';
const HEADER_ID: u8 = b'i';
const STATE_NODE: u8 = b'n';
const STATE_RECORD: u8 = b's';
#[derive(Debug)]
pub(crate) struct StateReadFault;

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

/// Millionths of a shannon per deterministic unit of transaction work.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceFees {
    pub encoded_byte_microshannons: u64,
    pub cycle_microshannons: u64,
    pub cell_read_microshannons: u64,
    pub cell_read_byte_microshannons: u64,
    pub cell_write_byte_microshannons: u64,
    pub cell_delete_microshannons: u64,
}

impl ResourceFees {
    pub const MICROSHANNONS_PER_SHANNON: u64 = 1_000_000;

    fn encode(self) -> [u8; 56] {
        let mut bytes = [0; 56];
        for (out, price) in bytes.chunks_exact_mut(8).zip([
            Self::MICROSHANNONS_PER_SHANNON,
            self.encoded_byte_microshannons,
            self.cycle_microshannons,
            self.cell_read_microshannons,
            self.cell_read_byte_microshannons,
            self.cell_write_byte_microshannons,
            self.cell_delete_microshannons,
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
            (self.encoded_byte_microshannons, encoded_bytes),
            (self.cell_read_microshannons, reads),
            (self.cell_read_byte_microshannons, read_bytes),
            (self.cell_write_byte_microshannons, write_bytes),
            (
                self.cell_delete_microshannons,
                resolved.inputs().len() as u64,
            ),
        ]
        .into_iter()
        .try_fold(0u128, |sum, (price, units)| {
            sum.checked_add(u128::from(price) * u128::from(units))
                .context("resource fee overflow")
        })?;
        let available = resolved
            .fee()
            .checked_sub(reserved_fee)
            .context("insufficient fee for transaction resources")?;
        let cycle_balance = (u128::from(available) * u128::from(Self::MICROSHANNONS_PER_SHANNON))
            .checked_sub(fixed)
            .context("insufficient fee for transaction resources")?;
        let cycles = cycle_balance.checked_div(u128::from(self.cycle_microshannons));
        Ok(cycles.map_or(u64::MAX, |cycles| u64::try_from(cycles).unwrap_or(u64::MAX)))
    }
}

pub struct Spec {
    pub vm_crypto_v1: bool,
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

impl Spec {
    pub fn validate_l1_policies(&self) -> Result<()> {
        use ckb_types::core::ScriptHashType;
        use stem_stake::finality::VaultPolicy;

        let stake = self
            .stake
            .as_ref()
            .map(|genesis| {
                let policy = stake_registry_policy(&genesis.registry_type)?;
                let domain = &genesis.statement.domain;
                ensure!(
                    policy.registry_id == domain.registry_id
                        && policy.network_id == domain.stem_network_id
                        && policy.ckb_genesis == domain.ckb_genesis,
                    "Stake Registry Type Script differs from the guardian-authenticated domain"
                );
                ensure!(
                    self.network.as_slice() == policy.network_id
                        && self.chain_id == policy.chain_id
                        && genesis.config.minimum_bond == policy.minimum_bond
                        && genesis.config.blocks_per_epoch == policy.blocks_per_epoch,
                    "Stem and CKB Stake Registry policies differ"
                );
                Ok(policy)
            })
            .transpose()?;
        if let Some(bridge) = &self.bridge {
            let mut checkpoint = stake.as_ref().map(|policy| policy.checkpoint);
            for lane in &bridge.lanes {
                ensure!(
                    ScriptHashType::try_from(lane.vault_type.hash_type())? == ScriptHashType::Data2
                        && lane.vault_type.code_hash() != Byte32::default(),
                    "bridge vault requires a Data2 Type Script"
                );
                let policy = VaultPolicy::decode(&lane.vault_type.args().raw_data())?;
                ensure!(
                    self.network.as_slice() == policy.network
                        && bridge.l1_genesis.as_slice() == policy.ckb_genesis
                        && stake
                            .as_ref()
                            .is_none_or(|stake| stake.ckb_genesis == policy.ckb_genesis),
                    "bridge vault, Stake Registry and Stem must use the same network identities"
                );
                ensure!(
                    *checkpoint.get_or_insert(policy.checkpoint) == policy.checkpoint,
                    "bridge vaults and Stake Registry must use the same CKB checkpoint"
                );
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub enum Retention {
    Recent(u64),
    Archive,
}

pub struct Store {
    db: Arc<DB>,
    read_options: Arc<ReadOptions>,
    metrics: Option<Arc<StorageMetrics>>,
    network: Byte32,
    chain_id: String,
    genesis_validators: Vec<([u8; 32], i64)>,
    staking: Option<Arc<Staking>>,
    stake_inputs: Arc<OnceLock<StakeInputs>>,
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
    live_cache: Arc<Mutex<LiveCache>>,
}

#[cfg(any(
    feature = "jolteon-admission-pipeline",
    feature = "jolteon-candidate-pipeline",
    feature = "jolteon-proposal-pipeline"
))]
pub(crate) struct PinnedView(Store);

#[cfg(any(
    feature = "jolteon-admission-pipeline",
    feature = "jolteon-candidate-pipeline",
    feature = "jolteon-proposal-pipeline"
))]
impl PinnedView {
    #[cfg(feature = "jolteon-admission-pipeline")]
    pub(crate) fn check(&self, raw: bytes::Bytes) -> Result<Option<Transaction>> {
        self.with_snapshot(|view| {
            let tx = Transaction::from_bytes(raw)?;
            view.check(&tx)?;
            Ok(tx)
        })
    }

    #[cfg(feature = "jolteon-candidate-pipeline")]
    pub(crate) fn prepare_candidate(
        &self,
        branch: &[Arc<Prepared>],
        raw: &[bytes::Bytes],
        time: u64,
    ) -> Result<Option<Prepared>> {
        self.with_snapshot(|view| {
            let transactions = raw
                .iter()
                .cloned()
                .map(Transaction::from_bytes)
                .collect::<Result<Vec<_>>>()?;
            view.on_branch(branch, |state| state.prepare(&transactions, time, true))
        })
    }

    #[cfg(feature = "jolteon-proposal-pipeline")]
    pub(crate) fn build_proposal(
        &self,
        branch: &[Arc<Prepared>],
        transactions: Vec<Transaction>,
        time: u64,
        max_bytes: u64,
    ) -> Result<Option<(Vec<Transaction>, Prepared)>> {
        self.with_snapshot(|view| {
            view.on_branch(branch, |state| state.build_proposal(transactions, time, max_bytes))
        })
    }

    fn with_snapshot<T>(&self, task: impl FnOnce(&Store) -> Result<T>) -> Result<Option<T>> {
        let snapshot = self.0.db.snapshot();
        let tip = snapshot.get(TIP)?.context("missing pinned snapshot tip")?;
        if tip.as_ref() != self.0.tip.encode() {
            return Ok(None);
        }
        let mut options = ReadOptions::default();
        options.set_snapshot(&snapshot);
        let mut view = self.0.read_view(&[]);
        // The private view and its options are dropped before the snapshot.
        // Its Live Cell cache and committed stake inputs are private to this view.
        view.read_options = Arc::new(options);
        task(&view).map(Some)
    }
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
        let bytes = Self::record_bytes(&cell);
        if self.bytes + bytes > MAX_BYTES || self.records.len() == MAX_ITEMS {
            self.clear();
        }
        self.bytes += bytes;
        self.records.insert(point, cell);
    }

    fn invalidate<'a>(&mut self, points: impl Iterator<Item = &'a OutPoint>) {
        for point in points {
            if let Some(cell) = self.records.remove(point) {
                self.bytes -= Self::record_bytes(&cell);
            }
        }
    }

    fn record_bytes(cell: &LiveCell) -> usize {
        cell.cell.as_slice().len() + Origin::LEN + OutPoint::TOTAL_SIZE
    }

    fn clear(&mut self) {
        self.records.clear();
        self.bytes = 0;
    }
}

type LiveChanges = BTreeMap<OutPoint, Option<LiveCell>>;

pub struct Prepared {
    parent: Byte32,
    next: Header,
    history: History,
    receipts: Vec<Receipt>,
    keeper_receipts: usize,
    validator_updates: Vec<stem_stake::Member>,
    committee_change: Option<CommitteeChange>,
    stake_inputs: Arc<OnceLock<StakeInputs>>,
    live: LiveChanges,
    batch: Batch,
}

struct Overlay {
    live: LiveChanges,
    stake: Option<StagedStake>,
    bridge: Option<StagedBridge>,
    lineages: lineage::Heads,
    keeper: keeper::Stage,
}

struct SystemCommitment {
    root: Byte32,
    validator_updates: Vec<stem_stake::Member>,
    committee_change: Option<CommitteeChange>,
    stake_inputs: Arc<OnceLock<StakeInputs>>,
}

struct ProposalSelection {
    transactions: Vec<Transaction>,
    context: HeaderContext,
    overlay: Overlay,
    receipts: Vec<Receipt>,
    reusable: bool,
}

#[derive(Default)]
struct LineageBatchJournal {
    live: OverlayUndo<OutPoint, LiveCell>,
    lineages: OverlayUndo<Byte32, stem_ledger::lineage::Head>,
}

struct OverlayUndo<K, V>(Vec<(K, Option<Option<V>>)>);

impl<K, V> Default for OverlayUndo<K, V> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<K: Clone + Ord, V> OverlayUndo<K, V> {
    fn write(
        &mut self,
        map: &mut BTreeMap<K, Option<V>>,
        changes: impl IntoIterator<Item = (K, Option<V>)>,
    ) {
        for (key, value) in changes {
            let previous = map.insert(key.clone(), value);
            self.0.push((key, previous));
        }
    }

    fn revert(self, map: &mut BTreeMap<K, Option<V>>) {
        for (key, previous) in self.0.into_iter().rev() {
            if let Some(value) = previous {
                map.insert(key, value);
            } else {
                map.remove(&key);
            }
        }
    }
}

struct LineageBatchMember<'a> {
    journal: &'a mut LineageBatchJournal,
    extra_bytes: u64,
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
        Self::open_with_storage_metrics(path, spec, retention, false)
    }

    pub fn open_with_storage_metrics(
        path: &Path,
        spec: &Spec,
        retention: Retention,
        measured: bool,
    ) -> Result<Self> {
        spec.validate_l1_policies()?;
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
        if measured {
            options.enable_statistics();
        }
        let db = DB::open(&options, path)?;
        let marker = db.get(MARKER)?;
        if let Some(marker) = &marker {
            ensure!(
                marker.starts_with(STORAGE_LAYOUT),
                "unsupported Stem database layout; open a fresh database"
            );
        }
        let metrics = measured.then(|| Arc::new(StorageMetrics::new(options)));
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
        let mut cell_edit = cells.edit(cells.empty_root(), |_| Ok(None::<Vec<u8>>));
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
                        .any(|script| stem_ledger::lineage::is_marker(code, &script))),
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
        let mut system_edit = system.edit(system.empty_root(), |_| Ok(None::<Vec<u8>>));
        let params = stem_ledger::protocol::genesis::network_parameters(
            &spec.limits.encode(), &spec.fees.encode(), spec.capacity.bond_per_byte,
            spec.capacity.state_overhead, &spec.chain_id, &validators,
        )?;
        if spec.vm_crypto_v1 {
            let id = system.record_key(b"vm/crypto-abi");
            system_edit.set(id, Some(system.value_hash(&[1])))?;
            batch.put(state_key(STATE_RECORD, Namespace::System, &id), [1]);
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
            let genesis = spec.stake.as_ref().context("missing stake genesis")?;
            for (name, value) in state
                .genesis_records(&genesis.statement.domain.registry_id)
                .into_iter()
                .chain([
                    (staking::CONFIG.to_vec(), config),
                    (staking::SNAPSHOT.to_vec(), snapshot),
                ])
            {
                let id = system.record_key(&name);
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
            let (bridge, records) = Bridge::initialize(config, &spec.network)?;
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
        let tip = if let Some(marker) = marker {
            ensure!(
                &marker[STORAGE_LAYOUT.len()..] == genesis_id.as_slice(),
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
            batch.put(MARKER, [STORAGE_LAYOUT, genesis_id.as_slice()].concat());
            put_tip(&mut batch, &network, &genesis, &history);
            sync_with_consensus(
                &db,
                &batch,
                None,
                &[],
                &[],
                metrics.as_deref().map(|m| (m, Purpose::Bootstrap)),
            )?;
            genesis
        };
        Ok(Self {
            db: Arc::new(db),
            read_options: Arc::new(ReadOptions::default()),
            metrics,
            network,
            chain_id: spec.chain_id.clone(),
            genesis_validators: validators,
            staking: staking.map(Arc::new),
            stake_inputs: Arc::default(),
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
            live_cache: Arc::default(),
            cache: Arc::new(VerificationCache::for_genesis(4096, spec.vm_crypto_v1)),
            branch: Vec::new(),
        })
    }

    pub fn storage_metrics(&self) -> Result<Option<StorageSample>> {
        self.metrics
            .as_ref()
            .map(|metrics| metrics.sample())
            .transpose()
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

    pub(crate) fn history_window(&self) -> Option<u64> {
        match self.retention {
            Retention::Recent(window) => Some(window),
            Retention::Archive => None,
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
        task(&self.read_view(branch))
    }

    fn read_view(&self, branch: &[Arc<Prepared>]) -> Self {
        let (tip, history, stake_inputs) = branch
            .last()
            .map_or((&self.tip, &self.history, &self.stake_inputs), |last| {
                (&last.next, &last.history, &last.stake_inputs)
            });
        Self {
            db: self.db.clone(),
            read_options: self.read_options.clone(),
            metrics: self.metrics.clone(),
            network: self.network.clone(),
            chain_id: self.chain_id.clone(),
            genesis_validators: self.genesis_validators.clone(),
            staking: self.staking.clone(),
            stake_inputs: Arc::clone(stake_inputs),
            bridge: self.bridge.clone(),
            lineage: self.lineage.clone(),
            keeper: self.keeper,
            genesis_id: self.genesis_id.clone(),
            tip: tip.clone(),
            history: history.clone(),
            cells: self.cells.clone(),
            system: self.system.clone(),
            retention: self.retention,
            capacity: self.capacity,
            limits: self.limits,
            fees: self.fees,
            cache: self.cache.clone(),
            branch: branch.to_vec(),
            live_cache: Arc::clone(&self.live_cache),
        }
    }

    #[cfg(any(
        feature = "jolteon-admission-pipeline",
        feature = "jolteon-candidate-pipeline",
        feature = "jolteon-proposal-pipeline"
    ))]
    pub(crate) fn pinned_view(&self) -> PinnedView {
        let mut view = self.read_view(&[]);
        view.live_cache = Arc::default();
        view.stake_inputs = Arc::default();
        PinnedView(view)
    }

    fn read(&self, key: impl AsRef<[u8]>) -> Result<Option<ReadValue<'_>>> {
        let key = key.as_ref();
        for prepared in self.branch.iter().rev() {
            if let Some(value) = prepared.batch.lookup(key) {
                return Ok(value.as_deref().map(ReadValue::Candidate));
            }
        }
        Ok(self
            .db
            .get_opt(key, &self.read_options)?
            .map(ReadValue::Committed))
    }

    pub(crate) fn consensus_record(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.read(index_key(b'j', key))?.map(|value| value.to_vec()))
    }

    pub(crate) fn persist_consensus(&self, records: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
        ensure!(self.branch.is_empty(), "cannot persist a speculative view");
        sync_with_consensus(
            &self.db,
            &Batch::default(),
            None,
            records,
            &[],
            self.metrics.as_deref().map(|m| (m, Purpose::Journal)),
        )
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

    pub fn prove_registry_application(
        &self,
        registry: &stem_stake::Hash32,
        sequence: u64,
    ) -> Result<(Option<stem_stake::l1::RegistryApplication>, Proof)> {
        use stem_stake::l1::registry_application_key;
        let (record, proof) = self.prove_system(&registry_application_key(registry, sequence))?;
        Ok((
            record
                .as_deref()
                .map(stem_ledger::stake_proof::decode_registry_application)
                .transpose()?,
            proof,
        ))
    }

    pub fn prove_reward_epoch(
        &self,
        epoch: u64,
    ) -> Result<(Option<stem_stake::rewards::EpochRewards>, Proof)> {
        use stem_stake::rewards::epoch_key;
        let (record, proof) = self.prove_system(&epoch_key(epoch))?;
        Ok((
            record
                .as_deref()
                .map(stem_ledger::stake_proof::decode_epoch_rewards)
                .transpose()?,
            proof,
        ))
    }

    pub fn verify_reward_epoch(
        network: &Byte32,
        header: &Header,
        record: &stem_stake::rewards::EpochRewards,
        proof: &Proof,
    ) -> bool {
        record.end_height != 0
            && record.end_height <= header.context.height
            && Self::verify_system_record(
                network,
                header,
                &stem_stake::rewards::epoch_key(record.epoch),
                Some(&record.encode()),
                proof,
            )
    }

    pub fn prove_stake_reward_claim(
        &self,
        epoch: u64,
        kind: u8,
        id: &[u8; 32],
    ) -> Result<(Option<Vec<u8>>, Proof)> {
        ensure!(kind <= 1, "invalid stake reward position kind");
        self.prove_system(&stem_ledger::protocol::reward_claim_key(epoch, kind, id))
    }

    pub fn verify_stake_reward_claim(
        network: &Byte32,
        header: &Header,
        epoch: u64,
        kind: u8,
        id: &[u8; 32],
        record: Option<&[u8; 40]>,
        proof: &Proof,
    ) -> bool {
        kind <= 1
            && Self::verify_system_record(
                network,
                header,
                &stem_ledger::protocol::reward_claim_key(epoch, kind, id),
                record.map(|bytes| bytes.as_slice()),
                proof,
            )
    }

    pub(crate) fn announced_committee(&self) -> Result<Option<CommitteeChange>> {
        Ok(self
            .system_record(COMMITTEE_RECORD)?
            .as_deref()
            .map(stem_ledger::stake_proof::decode_committee)
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
        stem_ledger::light::verify_system_record(network, header, name, record, proof)
    }

    fn stage_stake(&self) -> Result<Option<StagedStake>> {
        self.staking
            .as_ref()
            .map(|staking| staking.stage(self))
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
        for prepared in self.branch.iter().rev() {
            if let Some(cell) = prepared.live.get(point) {
                return Ok(cell.clone());
            }
        }
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
        // Only committed records enter this shared cache. The newest candidate
        // above is authoritative even when its value is a deletion.
        let found = self
            .db
            .get_opt(
                state_key(STATE_RECORD, Namespace::Cells, &id),
                &self.read_options,
            )
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

    fn node(&self, namespace: Namespace, id: &Key) -> Result<Option<node_storage::CanonicalNode>> {
        self.read(state_key(STATE_NODE, namespace, id))?
            .as_deref()
            .map(node_storage::canonical)
            .transpose()
    }

    /// Resolve conflicts and protocol state in proposal order. Schedule immutable
    /// VM contexts in bounded windows and charge each result in proposal order.
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
        let mut budget = Budget::new(self.limits);
        let due = self.due(height, &mut budget)?;
        ensure!(
            transactions.len().saturating_add(due.commands.len())
                <= self.limits.transaction_count as usize,
            "block transaction count exceeded"
        );
        let mut overlay = self.overlay(self.stage_stake()?);
        let with_due = (!due.commands.is_empty()).then(|| {
            due.commands
                .iter()
                .chain(transactions)
                .cloned()
                .collect::<Vec<_>>()
        });
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
        let block = BlockContext::after(
            &self.network,
            &self.tip,
            timestamp_ms,
            Tree::transactions(self.network.clone(), execution)?.root(),
            self.capacity,
        )?;
        let origin = Origin::new(&self.network, block.candidate());
        let mut receipts = self.run_keeper(&due, &block, &origin, &mut budget, &mut overlay)?;
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

        let work_stealing = cfg!(feature = "native-work-stealing") && width > 1;
        // Account for previous validation before admitting a dynamic wave.
        // Disjoint commands may materialize together when each has its full limit.
        for mut segment in transactions
            .chunk_by(|left, right| left.has_lineage_command() == right.has_lineage_command())
        {
            let lineage = segment[0].has_lineage_command();
            while !segment.is_empty() {
                let mut count = width.min(segment.len());
                if work_stealing && !lineage && segment.len() > width {
                    // Admit at most four waves, with the full cycle limit for
                    // each job. Keep the original wave boundaries around
                    // protocol operations and inline cycle charges.
                    let funded_waves =
                        budget.cycles / self.limits.transaction_cycles.max(1) / width as u64;
                    if funded_waves >= 2 {
                        let end = width.saturating_mul(funded_waves.min(4) as usize);
                        let ready = segment[..end.min(segment.len())]
                            .chunks(width)
                            .take_while(|wave| {
                                wave.iter().all(|tx| {
                                    tx.operation().is_none()
                                        && tx.keeper_action().is_none()
                                        && tx.l1_reference_certificate().is_none()
                                })
                            })
                            .map(<[Transaction]>::len)
                            .sum();
                        count = count.max(ready);
                    }
                }
                let (wave, rest) = segment.split_at(count);
                segment = rest;
                let parallel_commands = wave.len() > 1
                    && lineage
                    && budget.cycles / wave.len() as u64 >= self.limits.transaction_cycles
                    && lineage::independent(self, wave, &overlay.lineages)?;
                let mut commands = if parallel_commands {
                    wave.par_iter()
                        .map(|tx| self.materialize_lineage(tx, &block, &overlay, budget))
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
                        &block,
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
        self.finish_block(
            block.into_candidate(),
            overlay,
            execution,
            receipts,
            due.commands.len(),
            fees,
            parallel,
        )
    }

    fn finish_block(
        &self,
        context: HeaderContext,
        mut overlay: Overlay,
        execution: &[Transaction],
        receipts: Vec<Receipt>,
        keeper_receipts: usize,
        fees: u64,
        parallel: bool,
    ) -> Result<Prepared> {
        let height = context.height;
        let live = std::mem::take(&mut overlay.live);
        let mut batch = Batch::default();
        let build_execution = || encode_execution(&self.network, execution, &receipts);
        let (cell_root, system, (execution_bytes, receipt_root)) =
            if cfg!(feature = "native-commitment-pipeline")
                && parallel
                && execution.len() >= 256
                && rayon::current_num_threads() > 1
            {
                // Isolate each namespace's writes and append them in the
                // original order before producing the single atomic batch.
                let mut system_batch = Batch::default();
                let (cell, (system, execution)) = rayon::join(
                    || self.finish_cells(&mut batch, &live),
                    || {
                        rayon::join(
                            || self.finish_system(&mut system_batch, overlay, height, fees),
                            build_execution,
                        )
                    },
                );
                let result = (cell?, system?, execution?);
                batch.writes.append(&mut system_batch.writes);
                result
            } else {
                (
                    self.finish_cells(&mut batch, &live)?,
                    self.finish_system(&mut batch, overlay, height, fees)?,
                    build_execution()?,
                )
            };
        let mut history = self.history.clone();
        let next = Header::next(
            &self.network,
            &self.tip,
            &mut history,
            context,
            cell_root,
            system.root,
            receipt_root,
        )?;
        put_tip(&mut batch, &self.network, &next, &history);
        batch.put_owned(index_key(EXECUTION, &height.to_be_bytes()), execution_bytes);
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
            keeper_receipts,
            validator_updates: system.validator_updates,
            committee_change: system.committee_change,
            stake_inputs: system.stake_inputs,
            live,
            batch,
        })
    }

    fn finish_cells(&self, batch: &mut Batch, live: &LiveChanges) -> Result<Byte32> {
        let mut cell_changes = Vec::with_capacity(live.len());
        for (point, record) in live {
            let id = record::cell_key(&self.cells, point);
            let value = record
                .as_ref()
                .map(|record| record::cell_value(&self.cells, point, record));
            cell_changes.push((id, value));
        }
        cell_changes.sort_unstable_by_key(|(id, _)| *id);
        let (cell_root, nodes) = self.cells.batch_update(
            as_key(&self.tip.state.cell_root),
            |id| self.node(Namespace::Cells, id),
            &cell_changes,
            |id, _, value| {
                if value.is_none() {
                    batch.delete(state_key(STATE_RECORD, Namespace::Cells, &id));
                }
            },
        )?;
        put_nodes(batch, Namespace::Cells, nodes);
        Ok(cell_root.pack())
    }

    fn finish_system(
        &self,
        batch: &mut Batch,
        overlay: Overlay,
        height: u64,
        fees: u64,
    ) -> Result<SystemCommitment> {
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
            let (records, updates) = staking.finish(self, stake, height, total_fees)?;
            validator_updates = updates;
            for (name, value) in records {
                if name == COMMITTEE_RECORD {
                    committee_change = Some(stem_ledger::stake_proof::decode_committee(&value)?);
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
            let id = self.system.record_key(&stem_ledger::lineage::head_key(&id));
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
        // Carry the parent decode across writes to unrelated system records.
        let stake_inputs = if self
            .staking
            .as_ref()
            .is_some_and(|staking| staking.inputs_changed(&system_changes))
        {
            Arc::default()
        } else {
            Arc::clone(&self.stake_inputs)
        };
        system.set_batch(&system_changes, |id, _, value| {
            if value.is_none() {
                batch.delete(state_key(STATE_RECORD, Namespace::System, &id));
            }
        })?;
        let root = system.root().pack();
        put_nodes(batch, Namespace::System, system.into_writes());
        Ok(SystemCommitment {
            root,
            validator_updates,
            committee_change,
            stake_inputs,
        })
    }

    /// The proposer filters stale entries and conflicts in the supplied order.
    /// Final proposal verification binds creation records to its actual transaction root.
    pub fn select<T: Borrow<Transaction>>(
        &self,
        transactions: impl IntoIterator<Item = T>,
        timestamp_ms: u64,
        max_bytes: u64,
    ) -> Result<Vec<Transaction>> {
        Ok(self
            .stage_proposal(transactions, timestamp_ms, max_bytes)?
            .transactions)
    }

    /// Construct the same execution and commitments a follower derives with
    /// prepare. A proposal's transaction root is known only after selection.
    pub fn build_proposal<T: Borrow<Transaction>>(
        &self,
        transactions: impl IntoIterator<Item = T>,
        timestamp_ms: u64,
        max_bytes: u64,
    ) -> Result<(Vec<Transaction>, Prepared)> {
        let ProposalSelection {
            transactions,
            mut context,
            mut overlay,
            receipts,
            reusable,
        } = self.stage_proposal(transactions, timestamp_ms, max_bytes)?;
        let prepared = if reusable {
            context.transaction_root =
                Tree::transactions(self.network.clone(), &transactions)?.root();
            let origin = Origin::new(&self.network, &context);
            for record in overlay.live.values_mut().flatten() {
                record.origin = origin.clone();
            }
            let fees = receipts
                .iter()
                .try_fold(0u64, |sum, receipt| sum.checked_add(receipt.fee))
                .context("block fee overflow")?;
            self.finish_block(context, overlay, &transactions, receipts, 0, fees, true)?
        } else {
            self.prepare(&transactions, timestamp_ms, true)?
        };
        Ok((transactions, prepared))
    }

    fn stage_proposal<T: Borrow<Transaction>>(
        &self,
        transactions: impl IntoIterator<Item = T>,
        timestamp_ms: u64,
        max_bytes: u64,
    ) -> Result<ProposalSelection> {
        let mut budget = Budget::new(self.limits);
        let mut overlay = self.overlay(self.stage_stake()?);
        let mut wire_bytes = max_bytes;
        let block = BlockContext::after(
            &self.network,
            &self.tip,
            timestamp_ms,
            Byte32::default(),
            self.capacity,
        )?;
        let origin = Origin::new(&self.network, block.candidate());
        let due = self.due(block.candidate().height, &mut budget)?;
        let mut receipts = self.run_keeper(&due, &block, &origin, &mut budget, &mut overlay)?;
        let mut reusable = due.commands.is_empty();
        let mut selected = Vec::new();
        for item in transactions {
            if selected.len() + due.commands.len() == self.limits.transaction_count as usize
                || budget.bytes == 0
                || wire_bytes == 0
            {
                break;
            }
            let tx = item.borrow();
            let len = tx.as_slice().len();
            if len as u64 > budget.bytes || lineage::minimum_reads(tx) > budget.reads {
                continue;
            }
            let entry_bytes = (1 + prost::length_delimiter_len(len) + len) as u64;
            let Some(next_wire) = wire_bytes.checked_sub(entry_bytes) else {
                continue;
            };
            // Without Header Deps the VM has no origin-based Header to load.
            // Ordinary calls and single Lineage commands see the same height
            // and parent throughout. Keep the full replay for other inspected
            // operations, including any of them that failed selection.
            reusable &= tx.header_deps().len() == 0
                && tx.operation().is_none()
                && tx.keeper_action().is_none()
                && tx.keeper_schedule().is_none()
                && tx.lineage_batch().is_none();
            match self.stage(
                tx,
                &block,
                &origin,
                &mut budget,
                &mut overlay,
                None,
                |resolved, limit| self.cache.verify(resolved, limit, true),
            ) {
                Ok(receipt) => {
                    wire_bytes = next_wire;
                    receipts.push(receipt);
                    selected.push(tx.clone());
                }
                Err(error) if error.is::<StateReadFault>() => return Err(error),
                Err(_) => {}
            }
        }
        Ok(ProposalSelection {
            transactions: selected,
            context: block.into_candidate(),
            overlay,
            receipts,
            reusable,
        })
    }

    pub fn check(&self, tx: &Transaction) -> Result<Receipt> {
        let block = BlockContext::after(
            &self.network,
            &self.tip,
            self.tip.context.timestamp_ms,
            Byte32::default(),
            self.capacity,
        )?;
        self.stage(
            tx,
            &block,
            &Origin::new(&self.network, block.candidate()),
            &mut Budget::new(self.limits),
            &mut self.overlay(None),
            None,
            |resolved, limit| self.cache.verify(resolved, limit, false),
        )
    }

    fn overlay(&self, stake: Option<StagedStake>) -> Overlay {
        Overlay {
            live: BTreeMap::new(),
            stake,
            bridge: self.bridge.as_deref().map(Bridge::stage),
            lineages: BTreeMap::new(),
            keeper: keeper::Stage::default(),
        }
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
        block: &BlockContext<'_>,
        overlay: &Overlay,
        budget: Budget,
    ) -> Result<Option<lineage::Materialized>> {
        let height = block.candidate().height;
        ensure!(
            tx.keeper_schedule().is_none_or(|schedule| {
                overlay.keeper.running
                    && schedule.ready(height, block.parent().context.timestamp_ms)
            }),
            "trigger-bound Lineage command requires its committed Keeper reservation"
        );
        tx.lineage_command()
            .map(|command| {
                ensure!(
                    height < tx.valid_before_height().expect("validated command expiry"),
                    "Lineage command has expired"
                );
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
        block: &BlockContext<'_>,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
        staged_command: Option<lineage::Materialized>,
        verify: impl FnOnce(Arc<ResolvedTransaction>, u64) -> Result<u64>,
    ) -> Result<Receipt> {
        if let Some(members) = tx.lineage_batch() {
            Transaction::verify_lineage_batch_plan(&self.network, members)?;
            return self.stage_atomic_lineage(tx, members, block, origin, budget, overlay);
        }
        ensure!(
            tx.lineage_intent()
                .is_none_or(|intent| intent.batch.is_none()),
            "batch-bound Lineage intent cannot execute alone"
        );
        if tx
            .keeper_schedule()
            .is_some_and(|schedule| schedule.retry.is_some())
        {
            return self.stage_atomic_lineage(
                tx,
                std::slice::from_ref(tx),
                block,
                origin,
                budget,
                overlay,
            );
        }
        self.stage_transaction(
            tx,
            block,
            origin,
            budget,
            overlay,
            staged_command,
            verify,
            None,
        )
    }

    fn stage_atomic_lineage(
        &self,
        tx: &Transaction,
        members: &[Transaction],
        block: &BlockContext<'_>,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
    ) -> Result<Receipt> {
        let payload_bytes = members
            .iter()
            .map(|member| member.as_slice().len() as u64)
            .sum::<u64>();
        let extra_bytes = (tx.as_slice().len() as u64)
            .checked_sub(payload_bytes)
            .context("invalid Lineage batch byte accounting")?;
        let mut next = *budget;
        next.cycles = next.cycles.min(self.limits.transaction_cycles);
        let mut journal = LineageBatchJournal::default();
        let result = (|| {
            let mut receipt = Receipt {
                fee: 0,
                cycles: 0,
                protocol_result: stem_ledger::lineage::Outcome::Applied.encode(),
            };
            for (index, member) in members.iter().enumerate() {
                let item = self
                    .stage_transaction(
                        member,
                        block,
                        origin,
                        &mut next,
                        overlay,
                        None,
                        |resolved, limit| self.cache.verify(resolved, limit, false),
                        Some(LineageBatchMember {
                            journal: &mut journal,
                            extra_bytes: if index == 0 { extra_bytes } else { 0 },
                        }),
                    )
                    .with_context(|| format!("Lineage batch member {index}"))?;
                receipt.fee = receipt
                    .fee
                    .checked_add(item.fee)
                    .context("Lineage batch fee overflow")?;
                receipt.cycles = receipt
                    .cycles
                    .checked_add(item.cycles)
                    .context("Lineage batch cycles overflow")?;
            }
            next.cycles = budget
                .cycles
                .checked_sub(receipt.cycles)
                .context("Lineage batch exceeds the block cycle budget")?;
            Ok(receipt)
        })();
        if result.is_ok() {
            *budget = next;
        } else {
            journal.live.revert(&mut overlay.live);
            journal.lineages.revert(&mut overlay.lineages);
        }
        result
    }

    fn stage_transaction(
        &self,
        tx: &Transaction,
        block: &BlockContext<'_>,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
        staged_command: Option<lineage::Materialized>,
        verify: impl FnOnce(Arc<ResolvedTransaction>, u64) -> Result<u64>,
        batch: Option<LineageBatchMember<'_>>,
    ) -> Result<Receipt> {
        let context = block.candidate();
        let tx_bytes =
            tx.as_slice().len() as u64 + batch.as_ref().map_or(0, |item| item.extra_bytes);
        let mut next = *budget;
        charge(&mut next.bytes, tx_bytes, "block transaction bytes")?;
        let materialized = if staged_command.is_some() {
            staged_command
        } else {
            self.materialize_lineage(tx, block, overlay, next)?
        };
        ensure!(
            batch.is_none()
                || materialized
                    .as_ref()
                    .is_some_and(|command| command.executed),
            "Lineage Executor rejected the batch member"
        );
        let Overlay {
            live,
            stake,
            bridge,
            lineages,
            keeper,
        } = overlay;
        let operation = tx.operation();
        if matches!(operation, Some(Operation::StakeSnapshot(_))) {
            let staking = self
                .staking
                .as_ref()
                .context("native staking is not configured")?;
            let stake = match stake {
                Some(staged) => staged,
                // Individual admission starts with no staged registry state.
                None => stake.insert(staking.stage(self)?),
            };
            let receipt = staking.verify(self, context.height, tx, stake)?;
            *budget = next;
            return Ok(receipt);
        }
        if matches!(
            operation,
            Some(
                Operation::BridgeSettlement(_)
                    | Operation::FeeClaim(_)
                    | Operation::RewardClaim(_)
                    | Operation::L1AnchorUpdate(_)
            )
        ) {
            let (bridge, stage) = self
                .bridge
                .as_ref()
                .zip(bridge.as_mut())
                .context("native bridge is not configured")?;
            let changes = match operation {
                Some(Operation::FeeClaim(claim)) => bridge.claim_fee(self, stage, tx, claim)?,
                Some(Operation::RewardClaim(claim)) => {
                    bridge.claim_reward(self, stage, tx, claim)?
                }
                Some(Operation::L1AnchorUpdate(update)) => {
                    bridge.advance_anchor(self, stage, tx, update, context.height)?
                }
                _ => bridge.settle(self, stage, tx, context.height)?,
            };
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
        let concrete = materialized
            .as_ref()
            .map_or(tx, |command| &command.transaction);
        let reference_cycles = VerifiedL1References::cycles(concrete);
        ensure!(
            reference_cycles <= self.limits.transaction_cycles,
            "L1 reference verification exceeds transaction cycles"
        );
        charge(
            &mut next.cycles,
            reference_cycles,
            "block L1 reference verification cycles",
        )?;
        let (resolved, mut changes) = if matches!(operation, Some(Operation::BridgeMint(_))) {
            let (bridge, stage) = self
                .bridge
                .as_ref()
                .zip(bridge.as_ref())
                .context("native bridge is not configured")?;
            let (grant, asset, changes) = bridge.mint(self, stage, context.height, tx)?;
            let resolved =
                ResolvedTransaction::resolve_mint(tx.clone(), block, &grant, asset, fresh, header)?;
            (resolved, changes)
        } else {
            let resolved = if reference_cycles != 0 {
                let bridge = self
                    .bridge
                    .as_ref()
                    .context("L1 references require a configured bridge")?;
                let grant = bridge.verify_l1_references(self, concrete, context.height)?;
                ResolvedTransaction::resolve_referenced(
                    concrete.clone(),
                    block,
                    grant,
                    fresh,
                    header,
                )?
            } else {
                ResolvedTransaction::resolve(concrete.clone(), block, fresh, header)?
            };
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
        let (message_reads, message_read_bytes, message_write_bytes) = bridge::intercell::apply(
            self,
            self.bridge.as_deref().zip(bridge.as_ref()),
            &resolved,
            &mut changes,
        )?;
        let keeper_change = if tx.keeper_action().is_some() {
            keeper::change(self, keeper, &resolved, block, &mut next, fresh)?
        } else {
            None
        };
        let keeper_cycles = keeper_change.as_ref().map_or(0, |change| change.cycles);
        let id = resolved.execution_id();
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
            + resolved.l1_references().len() as u64
            + lineage_reads
            + message_reads
            + materialized.as_ref().map_or(0, |command| command.reads);
        let read_bytes = resolved.resolved_cell_bytes() as u64
            + resolved
                .l1_references()
                .iter()
                .map(|record| record.as_slice().len() as u64)
                .sum::<u64>()
            + message_read_bytes
            + lineage_reads
                * (stem_ledger::lineage::HEAD_KEY_BYTES as u64
                    + stem_ledger::lineage::Head::MAX_ENCODED_BYTES as u64)
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
                stem_ledger::lineage::HEAD_KEY_BYTES as u64
                    + head.as_ref().map_or(0, |head| head.encode().len() as u64)
            })
            .sum();
        charge(
            &mut next.write_bytes,
            lineage_bytes,
            "block Lineage write bytes",
        )?;
        charge(
            &mut next.write_bytes,
            message_write_bytes,
            "block Intercell write bytes",
        )?;
        let cycle_limit = self
            .limits
            .transaction_cycles
            .checked_sub(keeper_cycles + reference_cycles)
            .context("authorization exceeds transaction cycles")?
            .min(next.cycles);
        let cycle_limit = if tx.operation().is_none() {
            cycle_limit.min(
                self.fees
                    .cycle_budget(
                        &resolved,
                        tx_bytes,
                        budget.reads - next.reads,
                        budget.read_bytes - next.read_bytes,
                        budget.write_bytes - next.write_bytes,
                        keeper_change.as_ref().map_or(0, |change| change.fee),
                    )?
                    .checked_sub(keeper_cycles + reference_cycles)
                    .context("authorization exceeds paid VM cycles")?,
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
                    Err(error) if batch.is_some() => return Err(error),
                    Err(_) => {
                        let reads_attempted = reads - command.reads;
                        let bytes_attempted = read_bytes - command.read_bytes;
                        let refund = materialized.expect("Lineage command").rollback(
                            validation_limit,
                            reads_attempted,
                            bytes_attempted,
                        )?;
                        return self.stage_transaction(
                            tx,
                            block,
                            origin,
                            budget,
                            overlay,
                            Some(refund),
                            verify,
                            None,
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

        let protocol_result = materialized.as_ref().map_or_else(
            || changes.protocol_result.clone(),
            |command| stem_ledger::lineage::Outcome::from_applied(command.executed).encode(),
        );
        let cell_changes = concrete
            .inputs()
            .map(|input| (input.previous_output, None))
            .chain(outputs);
        if let Some(member) = batch {
            member.journal.live.write(live, cell_changes);
            member.journal.lineages.write(lineages, lineage_changes);
        } else {
            live.extend(cell_changes);
            lineages.extend(lineage_changes);
            if let Some(change) = keeper_change {
                keeper.apply(change);
            }
            if let Some(bridge) = bridge.as_mut() {
                bridge.apply(changes);
            }
        }
        *budget = next;
        Ok(Receipt {
            fee,
            cycles: cycles + keeper_cycles + reference_cycles,
            protocol_result,
        })
    }

    pub fn commit(&mut self, prepared: Prepared) -> Result<()> {
        self.commit_certified(&prepared, &[], &[])
    }

    pub(crate) fn commit_certified(
        &mut self,
        prepared: &Prepared,
        records: &[(Vec<u8>, Vec<u8>)],
        retired: &[Range<Vec<u8>>],
    ) -> Result<()> {
        ensure!(self.branch.is_empty(), "cannot commit a speculative view");
        ensure!(
            prepared.parent == self.tip.id(&self.network),
            "stale prepared block"
        );
        sync_with_consensus(
            &self.db,
            &prepared.batch,
            Some((&self.cells, &prepared.live)),
            records,
            retired,
            self.metrics.as_deref().map(|m| (m, Purpose::Finality)),
        )?;
        self.history = prepared.history.clone();
        self.tip = prepared.next.clone();
        self.stake_inputs = Arc::clone(&prepared.stake_inputs);
        if !prepared.live.is_empty() {
            self.live_cache
                .lock()
                .expect("Live Cell cache")
                .invalidate(prepared.live.keys());
        }
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
            Some(value) => batch.put_owned(key, node_storage::compact(value)),
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

fn encode_execution(
    network: &Byte32,
    transactions: &[Transaction],
    receipts: &[Receipt],
) -> Result<(Vec<u8>, Byte32)> {
    ensure!(
        transactions.len() == receipts.len(),
        "execution item counts differ"
    );
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&u32::try_from(transactions.len())?.to_le_bytes());
    let tree = Tree::receipts_with(network.clone(), receipts, |position, receipt| {
        for item in [transactions[position].as_slice(), receipt] {
            bytes.extend_from_slice(&u32::try_from(item.len())?.to_le_bytes());
            bytes.extend_from_slice(item);
        }
        Ok(())
    })?;
    Ok((bytes, tree.root()))
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
        self.put_owned(key, value.as_ref().to_vec());
    }

    fn put_owned(&mut self, key: impl AsRef<[u8]>, value: Vec<u8>) {
        self.writes.push((key.as_ref().to_vec(), Some(value)));
    }

    fn delete(&mut self, key: impl AsRef<[u8]>) {
        self.writes.push((key.as_ref().to_vec(), None));
    }
}

fn sync_with_consensus(
    db: &DB,
    batch: &Batch,
    live: Option<(&Patricia, &LiveChanges)>,
    records: &[(Vec<u8>, Vec<u8>)],
    retired: &[Range<Vec<u8>>],
    measured: Option<(&StorageMetrics, Purpose)>,
) -> Result<()> {
    let mut raw = WriteBatch::default();
    let mut bytes = measured.map(|_| BatchBytes::new(raw.size_in_bytes()));
    // Finality keeps created records first, in the original batch order.
    if let Some((tree, changes)) = live {
        let mut encoded = Vec::new();
        for (point, cell) in changes {
            // The Patricia edit already scheduled actual record deletions.
            if let Some(cell) = cell {
                let id = record::cell_key(tree, point);
                let key = state_key(STATE_RECORD, Namespace::Cells, &id);
                record::encode_cell_into(point, cell, &mut encoded);
                raw.put(key, &encoded)?;
                if let Some(bytes) = &mut bytes {
                    bytes.add(&key, raw.size_in_bytes());
                }
            }
        }
    }
    let mut proposals = BTreeSet::new();
    for (key, entry) in &batch.writes {
        if key.starts_with(b"jbody") {
            proposals.insert(key[1..].as_ref());
        }
        match entry {
            Some(value) => raw.put(key, value)?,
            None => raw.delete(key)?,
        }
        if let Some(bytes) = &mut bytes {
            bytes.add(key, raw.size_in_bytes());
        }
    }
    for (key, value) in records {
        let storage_key = index_key(b'j', key);
        // A prior synchronous write already made this body durable. Preserve
        // repeated-key ordering within this batch, and always write journal state.
        if key.starts_with(b"body")
            && proposals.insert(key.as_slice())
            && db.get(&storage_key)?.as_deref() == Some(value.as_slice())
        {
            continue;
        }
        let key = storage_key;
        raw.put(&key, value)?;
        if let Some(bytes) = &mut bytes {
            bytes.add(&key, raw.size_in_bytes());
        }
    }
    for range in retired {
        let start = index_key(b'j', &range.start);
        raw.delete_range(&start, &index_key(b'j', &range.end))?;
        if let Some(bytes) = &mut bytes {
            bytes.add(&start, raw.size_in_bytes());
        }
    }
    let mut options = WriteOptions::default();
    options.set_sync(true);
    let started = measured.map(|_| Instant::now());
    db.write_opt(&raw, &options)?;
    if let (Some((metrics, purpose)), Some(bytes), Some(started)) = (measured, bytes, started) {
        metrics.commit(purpose, bytes, started.elapsed());
    }
    Ok(())
}

#[cfg(all(
    test,
    any(
        feature = "jolteon-admission-pipeline",
        feature = "jolteon-candidate-pipeline",
        feature = "jolteon-proposal-pipeline",
        feature = "native-commitment-pipeline"
    )
))]
mod pipeline_store_tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            vm_crypto_v1: false,
            chain_id: "admission-snapshot-test".into(),
            development_validators: vec![],
            stake: None,
            bridge: None,
            lineage_type: None,
            keeper: None,
            network: [19; 32].pack(),
            genesis_time_ms: 1000,
            capacity: CapacityPolicy {
                bond_per_byte: 1_000_000,
                state_overhead: 80,
            },
            fees: Default::default(),
            limits: Limits {
                transaction_count: 8,
                transaction_bytes: 1024 * 1024,
                transaction_cycles: 100_000_000,
                block_cycles: 300_000_000,
                cell_reads: 64,
                cell_read_bytes: 8 * 1024 * 1024,
                cell_write_bytes: 32 * 1024,
                header_dep_age: 2,
            },
            development_cells: Vec::new(),
        }
    }

    #[cfg(any(
        feature = "jolteon-admission-pipeline",
        feature = "jolteon-candidate-pipeline",
        feature = "jolteon-proposal-pipeline"
    ))]
    #[test]
    fn snapshot_reads_old_tip_during_commit_and_rejects_stale_acquisition() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut store = Store::open(home.path(), &spec(), Retention::Archive)?;
        let old = store.pinned_view();
        let expected = store.tip().clone();
        let next = store.prepare(&[], 1001, false)?;
        #[cfg(feature = "jolteon-proposal-pipeline")]
        {
            let ancestor = Arc::new(store.prepare(&[], 1001, false)?);
            let (transactions, proposal) = old
                .build_proposal(&[ancestor.clone()], Vec::new(), 1002, 1024)?
                .context("missing proposal snapshot")?;
            let reference = store.on_branch(&[ancestor], |view| view.prepare(&[], 1002, false))?;
            assert!(transactions.is_empty());
            assert_eq!(proposal.header(), reference.header());
            assert_eq!(proposal.receipts(), reference.receipts());
        }
        let observed = old.with_snapshot(|read| {
            store.commit(next)?;
            let bytes = read.read(TIP)?.context("missing snapshot tip")?;
            Header::decode(&bytes)
        })?;
        assert_eq!(observed.as_ref(), Some(&expected));
        assert_eq!(store.tip().context.height, 1);
        assert!(old.with_snapshot(|_| Ok(()))?.is_none());
        #[cfg(feature = "jolteon-proposal-pipeline")]
        assert!(old.build_proposal(&[], Vec::new(), 1002, 1024)?.is_none());
        let fresh = store.pinned_view();
        assert_eq!(
            fresh.with_snapshot(|read| Ok(read.tip().clone()))?.as_ref(),
            Some(store.tip())
        );
        Ok(())
    }

    #[cfg(feature = "native-commitment-pipeline")]
    #[test]
    fn namespace_commitments_keep_the_ordered_write_batch() -> Result<()> {
        use bytes::Bytes;
        use ckb_types::packed::Script;
        use stem_ledger::CellInput;

        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let spent = OutPoint::new([41; 32].pack(), 0);
        let created = OutPoint::new([42; 32].pack(), 0);
        let lock = Script::default();
        let capacity = Cell::minimum_capacity(&lock, &[], 0)? + 1_000_000_000;
        let cell = Cell::new(capacity, lock, vec![], Bytes::new())?;
        spec.development_cells.push((spent.clone(), cell.clone()));
        let store = Store::open(home.path(), &spec, Retention::Archive)?;
        let execution = (0..256)
            .map(|index| {
                Transaction::new(
                    vec![],
                    vec![],
                    vec![CellInput::new(OutPoint::new([43; 32].pack(), index), None)],
                    vec![cell.clone()],
                    vec![],
                )
            })
            .collect::<Result<Vec<_>>>()?;
        let context = HeaderContext::after(
            store.network(),
            store.tip(),
            1001,
            Tree::transactions(store.network().clone(), &execution)?.root(),
        )?;
        let build = |parallel| {
            // This fixture supplies an already staged overlay. The integration
            // test covers real signed execution on both sides of the cutoff.
            let mut overlay = store.overlay(None);
            overlay.live.insert(spent.clone(), None);
            overlay.live.insert(
                created.clone(),
                Some(LiveCell {
                    cell: cell.clone(),
                    origin: Origin::new(store.network(), &context),
                }),
            );
            let receipts = vec![
                Receipt {
                    fee: 100,
                    cycles: 200,
                    protocol_result: Bytes::new(),
                };
                execution.len()
            ];
            store.finish_block(
                context.clone(),
                overlay,
                &execution,
                receipts,
                0,
                25_600,
                parallel,
            )
        };
        let serial = build(false)?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
        let parallel = pool.install(|| build(true))?;
        assert!(!serial.batch.writes.is_empty());
        assert_eq!(serial.batch.writes, parallel.batch.writes);
        assert_eq!(serial.header().encode(), parallel.header().encode());
        Ok(())
    }
}
