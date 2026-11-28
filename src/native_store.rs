//! Atomic persistence for the native ledger. Both state trees share the same
//! column family and commit batch as the header, execution data, and MMR peaks.

use std::{
    borrow::Borrow,
    collections::{BTreeMap, BTreeSet, HashMap},
    num::NonZeroUsize,
    ops::Range,
    path::Path,
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{
    BlockBasedOptions, Cache, DB, DBVector, Options, ReadOptions, WriteBatch, WriteOptions, prelude::*,
};
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
mod cell_execution;
#[cfg(feature = "native-batched-cell-proof-records")]
mod cell_proof_records;
mod cell_query;
#[cfg(feature = "native-owned-live-cell-reads")]
mod cell_read;
mod cell_write;
pub use cell_query::{CellPage, ScriptQuery, find_cells};
mod history_index;
pub use history_index::HistoryIndex;
mod keeper;
pub use keeper::{KeeperPolicy, KeeperTimePolicy};
mod lineage;
pub use stem_ledger::lineage::LineageProof;
mod live_cache;
mod live_index;
use live_cache::LiveCache;
pub use live_index::{LiveCellIndexLayout, LiveCellIndexStatus};
use live_index::IndexView;
mod node_index;
pub use node_index::CellNodeIndexStatus;
#[cfg(feature = "native-hot-cell-node-cache")]
pub use node_index::HotCellNodeStatus;
use node_index::NodeIndex;
mod node_storage;
#[cfg(feature = "native-shared-pinned-cell-cache")]
mod pinned_cell_cache;
#[cfg(feature = "jolteon-cancel-stale-execution")]
mod preparation;
#[cfg(feature = "native-compact-prepared-cell-delta")]
mod prepared_cells;
#[cfg(feature = "native-compact-prepared-cell-delta")]
use prepared_cells::PreparedCells;
#[cfg(feature = "jolteon-cancel-stale-execution")]
pub(crate) use preparation::{PreparationCancellation, PreparationCancelled};
mod staking;
mod state_snapshot;
mod storage_metrics;
#[cfg(any(
    feature = "native-rolling-cell-execution",
    feature = "native-rolling-leader-execution",
    feature = "native-rolling-lineage-execution"
))]
mod worker_completion;
pub use storage_metrics::StorageSample;
use storage_metrics::{BatchBytes, Purpose, StorageMetrics};
#[cfg(feature = "native-compact-write-journal")]
mod write_journal;
#[cfg(feature = "native-compact-write-journal")]
use write_journal::{JournalIndex, JournalKey};
#[cfg(all(test, feature = "jolteon-proposal-pipeline"))]
#[path = "../contracts/test_support.rs"]
mod test_contract;

use bridge::{Bridge, BridgeChanges, StagedBridge};
use cell_execution::{MAX_SPECULATIVE_READ_BYTES, PrecedingCells, ProposalWindow};
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
const MAX_CELL_PROOF_BATCH: usize = 1024;
const DEFAULT_VERIFICATION_CACHE_ENTRIES: usize = 4096;
pub(crate) const JOLTEON_CYCLE_SCAN_LIMIT: Option<u64> =
    if cfg!(feature = "jolteon-bounded-cycle-scan") {
        Some(64)
    } else {
        None
    };
#[derive(Debug)]
pub(crate) struct StateReadFault;

#[derive(Debug)]
struct MissingLiveCell(OutPoint);

#[derive(Debug)]
struct BlockVmBudgetExceeded;

impl std::fmt::Display for BlockVmBudgetExceeded {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("remaining block VM budget exceeded")
    }
}

impl std::fmt::Display for MissingLiveCell {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("input or dependency is not a Live Cell")
    }
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct ProposalScan {
    pub inspected: u64,
    pub selected: u64,
    /// Time spent selecting and staging the provisional proposal.
    pub selection_us: u64,
    /// Explicit inputs rejected because a speculative ancestor consumed them.
    pub spent_in_parent: u64,
    pub spent_in_parent_us: u64,
    /// Explicit inputs rejected after an earlier action in this proposal consumed them.
    pub spent_in_proposal: u64,
    pub block_cycle_rejections: u64,
    pub block_cycle_rejection_us: u64,
    pub cycle_scan_stops: u64,
    /// Capped age-priority prefix offered to the proposal builder.
    pub priority_candidates: u64,
    /// Number of proposal plans whose age-priority prefix reached the cap.
    pub priority_at_limit: u64,
    pub priority_inspected: u64,
    pub priority_selected: u64,
    pub priority_spent_in_parent: u64,
    pub priority_spent_in_proposal: u64,
    pub priority_cycle_rejections: u64,
    /// Workers may run ahead of the canonical scan. Count jobs separately
    /// from results used or candidates that required ordered staging.
    pub parallel_cell_jobs: u64,
    pub parallel_cell_reused: u64,
    pub parallel_cell_fallback: u64,
    pub parallel_parent_rejections_reused: u64,
    pub parallel_cell_us: u64,
    pub rolling_cells: CellStreamStats,
    pub lineage: LineageSchedule,
    pub dependency_reads: DependencyReadStats,
    pub keeper_wakes: KeeperWakeMetrics,
    /// Due commands reused from selection when finishing a Leader proposal.
    pub keeper_prefix_reused: u64,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct LineageSchedule {
    pub access_analysis_us: u64,
    pub parallel_materialize_us: u64,
    pub parallel_waves: u64,
    pub parallel_commands: u64,
    pub serial_commands: u64,
    pub lookahead_commands: u64,
    pub lookahead_reused: u64,
    pub lookahead_fallbacks: u64,
    pub rolling_windows: u64,
    pub rolling_wait_us: u64,
    pub rolling_worker_us: u64,
    pub rolling_peak_outstanding: u64,
    pub keeper_wakes: KeeperWakeMetrics,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct KeeperWakeMetrics {
    pub access_us: u64,
    pub materialize_us: u64,
    pub jobs: u64,
    pub reused: u64,
    pub fallbacks: u64,
}

impl KeeperWakeMetrics {
    fn record(&mut self, other: Self) {
        self.access_us = self.access_us.saturating_add(other.access_us);
        self.materialize_us = self.materialize_us.saturating_add(other.materialize_us);
        self.jobs = self.jobs.saturating_add(other.jobs);
        self.reused = self.reused.saturating_add(other.reused);
        self.fallbacks = self.fallbacks.saturating_add(other.fallbacks);
    }
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct CellStreamStats {
    pub runs: u64,
    pub jobs: u64,
    pub reused: u64,
    pub fallbacks: u64,
    pub ordered_declarations: u64,
    pub projected_transactions: u64,
    pub projection_us: u64,
    pub peak_projection_edits: u64,
    pub retired_projection_edits: u64,
    pub masked_projection_positions: u64,
    pub projection_reclaim_us: u64,
    pub projection_barriers: u64,
    pub first_submit_us: u64,
    pub refilled_jobs: u64,
    pub tail_cycle_share_jobs: u64,
    pub tail_cycle_share_reused: u64,
    pub discarded_jobs: u64,
    pub peak_outstanding: u64,
    pub elapsed_us: u64,
    pub dependency_reads: DependencyReadStats,
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct DependencyReadStats {
    pub planned: u64,
    pub cached: u64,
    pub bytes: u64,
    pub hits: u64,
    pub build_us: u64,
    pub rolling_samples: u64,
    pub rolling_rebuilds: u64,
    pub rolling_carried: u64,
}

impl DependencyReadStats {
    pub(crate) fn record(&mut self, other: Self) {
        self.planned = self.planned.saturating_add(other.planned);
        self.cached = self.cached.saturating_add(other.cached);
        self.bytes = self.bytes.saturating_add(other.bytes);
        self.hits = self.hits.saturating_add(other.hits);
        self.build_us = self.build_us.saturating_add(other.build_us);
        self.rolling_samples = self.rolling_samples.saturating_add(other.rolling_samples);
        self.rolling_rebuilds = self.rolling_rebuilds.saturating_add(other.rolling_rebuilds);
        self.rolling_carried = self.rolling_carried.saturating_add(other.rolling_carried);
    }
}

impl CellStreamStats {
    #[cfg(any(feature = "native-rolling-cell-execution", feature = "native-rolling-leader-execution"))]
    fn record_projection(&mut self, transactions: usize, elapsed: Duration) {
        self.projected_transactions = self.projected_transactions.saturating_add(transactions as u64);
        self.projection_us = self.projection_us.saturating_add(
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
        );
    }

    pub(crate) fn record(&mut self, other: Self) {
        self.runs = self.runs.saturating_add(other.runs);
        self.jobs = self.jobs.saturating_add(other.jobs);
        self.reused = self.reused.saturating_add(other.reused);
        self.fallbacks = self.fallbacks.saturating_add(other.fallbacks);
        self.ordered_declarations = self
            .ordered_declarations
            .saturating_add(other.ordered_declarations);
        self.projected_transactions = self.projected_transactions
            .saturating_add(other.projected_transactions);
        self.projection_us = self.projection_us.saturating_add(other.projection_us);
        self.peak_projection_edits = self.peak_projection_edits.max(other.peak_projection_edits);
        self.retired_projection_edits = self.retired_projection_edits
            .saturating_add(other.retired_projection_edits);
        self.masked_projection_positions = self
            .masked_projection_positions
            .saturating_add(other.masked_projection_positions);
        self.projection_reclaim_us = self.projection_reclaim_us
            .saturating_add(other.projection_reclaim_us);
        self.projection_barriers = self.projection_barriers.saturating_add(other.projection_barriers);
        self.first_submit_us = self.first_submit_us.saturating_add(other.first_submit_us);
        self.refilled_jobs = self.refilled_jobs.saturating_add(other.refilled_jobs);
        self.tail_cycle_share_jobs = self
            .tail_cycle_share_jobs
            .saturating_add(other.tail_cycle_share_jobs);
        self.tail_cycle_share_reused = self
            .tail_cycle_share_reused
            .saturating_add(other.tail_cycle_share_reused);
        self.discarded_jobs = self.discarded_jobs.saturating_add(other.discarded_jobs);
        self.peak_outstanding = self.peak_outstanding.max(other.peak_outstanding);
        self.elapsed_us = self.elapsed_us.saturating_add(other.elapsed_us);
        self.dependency_reads.record(other.dependency_reads);
    }
}

impl LineageSchedule {
    pub(crate) fn record(&mut self, other: Self) {
        self.access_analysis_us = self.access_analysis_us.saturating_add(other.access_analysis_us);
        self.parallel_materialize_us = self
            .parallel_materialize_us
            .saturating_add(other.parallel_materialize_us);
        self.parallel_waves = self.parallel_waves.saturating_add(other.parallel_waves);
        self.parallel_commands = self.parallel_commands.saturating_add(other.parallel_commands);
        self.serial_commands = self.serial_commands.saturating_add(other.serial_commands);
        self.lookahead_commands = self.lookahead_commands.saturating_add(other.lookahead_commands);
        self.lookahead_reused = self.lookahead_reused.saturating_add(other.lookahead_reused);
        self.lookahead_fallbacks = self.lookahead_fallbacks.saturating_add(other.lookahead_fallbacks);
        self.rolling_windows = self.rolling_windows.saturating_add(other.rolling_windows);
        self.rolling_wait_us = self.rolling_wait_us.saturating_add(other.rolling_wait_us);
        self.rolling_worker_us = self.rolling_worker_us.saturating_add(other.rolling_worker_us);
        self.rolling_peak_outstanding = self.rolling_peak_outstanding.max(other.rolling_peak_outstanding);
        self.keeper_wakes.record(other.keeper_wakes);
    }
}

impl ProposalScan {
    pub(crate) fn record(&mut self, scan: Self) {
        self.inspected = self.inspected.saturating_add(scan.inspected);
        self.selected = self.selected.saturating_add(scan.selected);
        self.selection_us = self.selection_us.saturating_add(scan.selection_us);
        self.spent_in_parent = self.spent_in_parent.saturating_add(scan.spent_in_parent);
        self.spent_in_parent_us = self.spent_in_parent_us.saturating_add(scan.spent_in_parent_us);
        self.spent_in_proposal = self.spent_in_proposal.saturating_add(scan.spent_in_proposal);
        self.block_cycle_rejections = self
            .block_cycle_rejections
            .saturating_add(scan.block_cycle_rejections);
        self.block_cycle_rejection_us = self
            .block_cycle_rejection_us
            .saturating_add(scan.block_cycle_rejection_us);
        self.cycle_scan_stops = self.cycle_scan_stops.saturating_add(scan.cycle_scan_stops);
        self.priority_candidates = self.priority_candidates.saturating_add(scan.priority_candidates);
        self.priority_at_limit = self.priority_at_limit.saturating_add(scan.priority_at_limit);
        self.priority_inspected = self.priority_inspected.saturating_add(scan.priority_inspected);
        self.priority_selected = self.priority_selected.saturating_add(scan.priority_selected);
        self.priority_spent_in_parent = self.priority_spent_in_parent.saturating_add(scan.priority_spent_in_parent);
        self.priority_spent_in_proposal = self.priority_spent_in_proposal.saturating_add(scan.priority_spent_in_proposal);
        self.priority_cycle_rejections = self.priority_cycle_rejections.saturating_add(scan.priority_cycle_rejections);
        self.parallel_cell_jobs = self.parallel_cell_jobs.saturating_add(scan.parallel_cell_jobs);
        self.parallel_cell_reused = self.parallel_cell_reused.saturating_add(scan.parallel_cell_reused);
        self.parallel_cell_fallback = self.parallel_cell_fallback.saturating_add(scan.parallel_cell_fallback);
        self.parallel_parent_rejections_reused = self
            .parallel_parent_rejections_reused
            .saturating_add(scan.parallel_parent_rejections_reused);
        self.parallel_cell_us = self.parallel_cell_us.saturating_add(scan.parallel_cell_us);
        self.rolling_cells.record(scan.rolling_cells);
        self.lineage.record(scan.lineage);
        self.dependency_reads.record(scan.dependency_reads);
        self.keeper_wakes.record(scan.keeper_wakes);
        self.keeper_prefix_reused = self
            .keeper_prefix_reused
            .saturating_add(scan.keeper_prefix_reused);
    }

    fn record_parent_spend(&mut self, priority: u64, elapsed: Duration) {
        self.spent_in_parent = self.spent_in_parent.saturating_add(1);
        self.priority_spent_in_parent = self.priority_spent_in_parent.saturating_add(priority);
        self.spent_in_parent_us = self.spent_in_parent_us.saturating_add(
            u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
        );
    }
}

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
    live_cache: Arc<LiveCache>,
    #[cfg(feature = "native-shared-pinned-cell-cache")]
    pinned_cell_cache: Arc<pinned_cell_cache::PinnedCellCache>,
    live_index: Option<IndexView>,
    cell_nodes: Option<Arc<NodeIndex>>,
    #[cfg(feature = "native-hot-cell-node-cache")]
    cell_node_header: Key,
    #[cfg(feature = "jolteon-cancel-stale-execution")]
    cancellation: Option<PreparationCancellation>,
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
    #[cfg(feature = "jolteon-cancel-stale-execution")]
    pub(crate) fn with_cancellation(mut self, cancellation: PreparationCancellation) -> Self {
        self.0.cancellation = Some(cancellation);
        self
    }

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
        committing: bool,
    ) -> Result<Option<Prepared>> {
        self.with_branch_snapshot(branch, committing, |view, branch| {
            let transactions = raw
                .iter()
                .cloned()
                .map(Transaction::from_bytes)
                .collect::<Result<Vec<_>>>()?;
            view.on_branch(branch, |state| state.prepare(&transactions, time, true))
        })
    }

    #[cfg(feature = "jolteon-proposal-pipeline")]
    pub(crate) fn build_proposal<T: Borrow<Transaction>>(
        &self,
        branch: &[Arc<Prepared>],
        transactions: impl IntoIterator<Item = T>,
        time: u64,
        max_bytes: u64,
        scan: &mut ProposalScan,
        committing: bool,
    ) -> Result<Option<(Vec<Transaction>, Prepared)>> {
        self.with_branch_snapshot(branch, committing, |view, branch| {
            view.on_branch(branch, |state| {
                state.build_proposal_measured(
                    transactions,
                    time,
                    max_bytes,
                    scan,
                    JOLTEON_CYCLE_SCAN_LIMIT,
                )
            })
        })
    }

    #[cfg(any(feature = "jolteon-admission-pipeline", test))]
    fn with_snapshot<T>(&self, task: impl FnOnce(&Store) -> Result<T>) -> Result<Option<T>> {
        self.with_branch_snapshot(&[], false, |view, _| task(view))
    }

    fn with_branch_snapshot<T>(
        &self,
        branch: &[Arc<Prepared>],
        committing: bool,
        task: impl FnOnce(&Store, &[Arc<Prepared>]) -> Result<T>,
    ) -> Result<Option<T>> {
        self.0.check_preparation_cancelled()?;
        let snapshot = self.0.db.snapshot();
        let tip = snapshot
            .get(TIP)
            .context(StateReadFault)?
            .context("missing pinned snapshot tip")
            .context(StateReadFault)?;
        let skip = if tip.as_ref() == self.0.tip.encode() {
            0
        } else if cfg!(feature = "jolteon-exact-prefix-pipeline") {
            // Match the complete durable execution Header in the contiguous
            // chain of prepared ancestors tied to this pin.
            let mut previous = &self.0.tip;
            let mut found = None;
            for (index, ancestor) in branch.iter().enumerate() {
                let next = ancestor.header();
                if ancestor.parent != previous.id(&self.0.network)
                    || previous.context.height.checked_add(1) != Some(next.context.height)
                {
                    return Ok(None);
                }
                if tip.as_ref() == next.encode() {
                    found = Some(index + 1);
                    break;
                }
                previous = next;
            }
            let Some(skip) = found else {
                return Ok(None);
            };
            skip
        } else if committing
            && branch.first().is_some_and(|first| {
                first.parent == self.0.tip.id(&self.0.network)
                    && tip.as_ref() == first.header().encode()
            })
        {
            1
        } else {
            return Ok(None);
        };
        let mut options = ReadOptions::default();
        options.set_snapshot(&snapshot);
        let mut view = self.0.read_view(&branch[..skip]);
        // The accepted prefix is already in the snapshot.
        view.branch.clear();
        #[cfg(feature = "native-shared-pinned-cell-cache")]
        {
            view.live_cache = view.pinned_cell_cache.acquire(&view.tip, &view.network);
        }
        if skip != 0 {
            // Rebased reads use a different committed Cell image from the
            // original pin, even if the same PinnedView is opened again.
            #[cfg(not(feature = "native-shared-pinned-cell-cache"))]
            {
                view.live_cache = Arc::default();
            }
            if let Some(index) = &mut view.live_index {
                index.at_header(view.tip.id(&view.network).unpack());
            }
            #[cfg(feature = "native-hot-cell-node-cache")]
            {
                view.cell_node_header = view.tip.id(&view.network).unpack();
            }
        }
        // The private view and its options are dropped before the snapshot.
        // Cell caches belong to the verified snapshot's committed Header.
        view.read_options = Arc::new(options);
        #[cfg(feature = "native-pinned-index-try-read")]
        if let Some(index) = &mut view.live_index {
            index.allow_pinned_snapshot_fallback();
        }
        let result = task(&view, &branch[skip..])?;
        view.check_preparation_cancelled()?;
        Ok(Some(result))
    }
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct PinnedCellCacheStatus {
    pub retained_height: Option<u64>,
    pub entries: usize,
    pub record_bytes: usize,
    pub shared_acquisitions: u64,
    pub private_acquisitions: u64,
    pub generations: u64,
    pub carried_forward: u64,
    #[serde(default)]
    pub shards: usize,
    #[serde(default)]
    pub capacity_flushes: u64,
}

type CellEdit = (OutPoint, Option<LiveCell>);
type LiveChanges = BTreeMap<OutPoint, Option<LiveCell>>;
#[cfg(not(feature = "native-compact-prepared-cell-delta"))]
type PreparedCells = LiveChanges;
#[cfg(feature = "native-compact-cell-trial-edits")]
type CellOutputEdits = smallvec::SmallVec<[CellEdit; 2]>;
#[cfg(not(feature = "native-compact-cell-trial-edits"))]
type CellOutputEdits = Vec<CellEdit>;

pub struct Prepared {
    parent: Byte32,
    next: Header,
    history: History,
    receipts: Vec<Receipt>,
    keeper_receipts: usize,
    validator_updates: Vec<stem_stake::Member>,
    committee_change: Option<CommitteeChange>,
    stake_inputs: Arc<OnceLock<StakeInputs>>,
    live: PreparedCells,
    #[cfg(feature = "native-prepared-live-cell-ownership")]
    detached_live_records: bool,
    batch: Batch,
    commitments: CommitmentTiming,
    lineage_schedule: LineageSchedule,
    cell_stream: CellStreamStats,
}

pub(crate) struct CommitmentTiming {
    pub(crate) total: Duration,
    pub(crate) cells: Duration,
    pub(crate) cell_phases: CellCommitmentTiming,
    pub(crate) system: Duration,
    pub(crate) execution: Duration,
}

pub(crate) struct CellCommitmentTiming {
    pub(crate) keys: Duration,
    pub(crate) tree: Duration,
    pub(crate) batch: Duration,
}

#[cfg(feature = "jolteon-commit-pipeline")]
pub(crate) struct CertifiedWriteTiming {
    pub(crate) persistence: Duration,
    pub(crate) cell_node_publication: Option<Duration>,
    #[cfg(feature = "jolteon-resident-cell-publication")]
    pub(crate) live_cell_publication: Option<Duration>,
    #[cfg(feature = "jolteon-prebuilt-state-write")]
    pub(crate) reused_state_build: Option<Duration>,
    #[cfg(feature = "jolteon-reuse-write-buffer")]
    pub(crate) reused_write_buffer: bool,
}

#[cfg(feature = "jolteon-commit-pipeline")]
#[derive(Default)]
pub(crate) struct CertifiedWriteCache {
    #[cfg(feature = "jolteon-prebuilt-state-write")]
    state: Option<CachedStateWrite>,
    #[cfg(feature = "jolteon-reuse-write-buffer")]
    spare: Option<WriteBatch>,
}

#[cfg(feature = "jolteon-prebuilt-state-write")]
struct CachedStateWrite {
    candidate: std::sync::Weak<Prepared>,
    prefix: WritePrefix,
    build: Duration,
}

#[cfg(feature = "jolteon-commit-pipeline")]
impl CertifiedWriteCache {
    #[cfg(feature = "jolteon-prebuilt-state-write")]
    fn take(&mut self, prepared: &Arc<Prepared>) -> Option<CachedStateWrite> {
        let cached = self.state.take()?;
        if cached.candidate.ptr_eq(&Arc::downgrade(prepared)) {
            return Some(cached);
        }
        #[cfg(feature = "jolteon-reuse-write-buffer")]
        self.recycle(cached.prefix.raw);
        None
    }

    fn take_buffer(&mut self) -> Option<WriteBatch> {
        #[cfg(feature = "jolteon-reuse-write-buffer")]
        return self.spare.take();
        #[cfg(not(feature = "jolteon-reuse-write-buffer"))]
        None
    }

    #[cfg(feature = "jolteon-reuse-write-buffer")]
    fn recycle(&mut self, mut raw: WriteBatch) {
        const MAX_RECYCLED_BATCH_BYTES: usize = 8 * 1024 * 1024;
        // Large batches may leave a larger backing allocation after clearing.
        if raw.size_in_bytes() <= MAX_RECYCLED_BATCH_BYTES && raw.clear().is_ok() {
            self.spare = Some(raw);
        }
    }
}

struct Overlay {
    live: LiveChanges,
    prefetched_cells: HashMap<OutPoint, Option<LiveCell>>,
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

struct ProposalProgress {
    transactions: Vec<Transaction>,
    receipts: Vec<Receipt>,
    reusable: bool,
    wire_bytes: u64,
    cycle_rejections: u64,
    cycle_scan_limit: Option<u64>,
}

enum ProposalStep {
    Reused,
    Ordered,
    Skipped,
    #[cfg(feature = "native-reuse-parent-spend-rejections")]
    RejectedParentSpend,
    Stop,
}

#[cfg(feature = "native-mask-rejected-cell-projections")]
impl ProposalStep {
    fn rejected(&self) -> bool {
        match self {
            Self::Skipped => true,
            #[cfg(feature = "native-reuse-parent-spend-rejections")]
            Self::RejectedParentSpend => true,
            _ => false,
        }
    }
}

enum ProposalTrial<T> {
    Ordered,
    Recheck,
    Ready(T),
    #[cfg(feature = "native-leader-lineage-execution")]
    Lineage(Option<lineage::Materialized>, bool),
    #[cfg(feature = "native-reuse-parent-spend-rejections")]
    Missing(OutPoint),
}

impl<T> ProposalTrial<T> {
    fn failed(_error: anyhow::Error) -> Self {
        #[cfg(feature = "native-reuse-parent-spend-rejections")]
        if let Ok(MissingLiveCell(point)) = _error.downcast() {
            return Self::Missing(point);
        }
        Self::Recheck
    }

    #[cfg(feature = "native-rolling-leader-execution")]
    fn map<U>(self, ready: impl FnOnce(T) -> U) -> ProposalTrial<U> {
        match self {
            Self::Ordered => ProposalTrial::Ordered,
            Self::Recheck => ProposalTrial::Recheck,
            Self::Ready(result) => ProposalTrial::Ready(ready(result)),
            #[cfg(feature = "native-leader-lineage-execution")]
            Self::Lineage(material, attempted) => ProposalTrial::Lineage(material, attempted),
            #[cfg(feature = "native-reuse-parent-spend-rejections")]
            Self::Missing(point) => ProposalTrial::Missing(point),
        }
    }
}

struct ProposalSelection {
    transactions: Vec<Transaction>,
    keeper_commands: Vec<Transaction>,
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
    fn live_records_detached(&self) -> bool {
        #[cfg(feature = "native-prepared-live-cell-ownership")]
        return self.detached_live_records;
        #[cfg(not(feature = "native-prepared-live-cell-ownership"))]
        false
    }

    pub fn header(&self) -> &Header {
        &self.next
    }

    pub fn receipts(&self) -> &[Receipt] {
        &self.receipts
    }

    pub(crate) fn commitment_timing(&self) -> &CommitmentTiming {
        &self.commitments
    }

    pub(crate) fn lineage_schedule(&self) -> LineageSchedule {
        self.lineage_schedule
    }

    pub(crate) fn cell_stream(&self) -> CellStreamStats {
        self.cell_stream
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

#[cfg(feature = "jolteon-share-proposal-bytes")]
pub(crate) type ConsensusValue = bytes::Bytes;
#[cfg(not(feature = "jolteon-share-proposal-bytes"))]
pub(crate) type ConsensusValue = Vec<u8>;
pub(crate) type ConsensusRecord = (Vec<u8>, ConsensusValue);

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
        Self::open_with_block_cache(path, spec, retention, measured, None)
    }

    pub fn open_with_block_cache(
        path: &Path,
        spec: &Spec,
        retention: Retention,
        measured: bool,
        cache_mib: Option<NonZeroUsize>,
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
        if let Some(capacity) = cache_mib {
            let capacity = capacity
                .get()
                .checked_mul(1 << 20)
                .context("block cache capacity overflow")?;
            let cache = Cache::new_lru_cache(capacity);
            let mut table = BlockBasedOptions::default();
            table.set_block_cache(&cache);
            options.set_block_based_table_factory(&table);
        }
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
            sync_with_consensus::<ConsensusValue>(
                &db,
                &batch,
                None,
                &[],
                &[],
                metrics.as_deref().map(|m| (m, Purpose::Bootstrap)),
            )?;
            genesis
        };
        #[cfg(feature = "native-hot-cell-node-cache")]
        let cell_node_header = tip.id(&network).unpack();
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
            #[cfg(feature = "native-shared-pinned-cell-cache")]
            pinned_cell_cache: Arc::default(),
            live_index: None,
            cell_nodes: None,
            #[cfg(feature = "native-hot-cell-node-cache")]
            cell_node_header,
            cache: Arc::new(VerificationCache::for_genesis(
                DEFAULT_VERIFICATION_CACHE_ENTRIES,
                spec.vm_crypto_v1,
            )),
            branch: Vec::new(),
            #[cfg(feature = "jolteon-cancel-stale-execution")]
            cancellation: None,
        })
    }

    pub(crate) fn configure_verification_cache(
        &mut self,
        entries: Option<usize>,
        shards: usize,
        sample_locks: bool,
    ) -> Result<()> {
        if shards != 1 || sample_locks {
            self.cache = Arc::new(VerificationCache::for_genesis_with_layout(
                DEFAULT_VERIFICATION_CACHE_ENTRIES,
                self.cache.crypto_v1_enabled(),
                shards,
                sample_locks,
            )?);
        }
        if let Some(entries) = entries {
            self.cache.resize_verified(entries);
        }
        Ok(())
    }

    pub(crate) fn verification_cache_stats(&self) -> stem_vm::VerificationCacheStats {
        self.cache.stats()
    }

    pub fn storage_metrics(&self) -> Result<Option<StorageSample>> {
        self.metrics
            .as_ref()
            .map(|metrics| metrics.sample())
            .transpose()
    }

    pub(crate) fn pinned_cell_cache_status(&self) -> Option<PinnedCellCacheStatus> {
        #[cfg(feature = "native-shared-pinned-cell-cache")]
        return Some(self.pinned_cell_cache.status());
        #[cfg(not(feature = "native-shared-pinned-cell-cache"))]
        None
    }

    /// Recover the optional, bounded resident index against the durable root.
    pub fn enable_live_cell_index(&mut self, limit_mib: NonZeroUsize) -> Result<()> {
        self.enable_live_cell_index_with_layout(limit_mib, LiveCellIndexLayout::Inline)
    }

    pub fn enable_live_cell_index_with_layout(
        &mut self,
        limit_mib: NonZeroUsize,
        layout: LiveCellIndexLayout,
    ) -> Result<()> {
        self.configure_live_cell_index(limit_mib, layout, 1)
    }

    /// Recover the resident index with a fixed local worker pool.
    pub fn configure_live_cell_index(
        &mut self,
        limit_mib: NonZeroUsize,
        layout: LiveCellIndexLayout,
        recovery_workers: usize,
    ) -> Result<()> {
        ensure!(self.branch.is_empty() && self.live_index.is_none(), "Live Cell index already configured or view is speculative");
        let limit = limit_mib.get().checked_mul(1 << 20).context("Live Cell index budget overflow")?;
        self.live_index = Some(IndexView::recover(self, limit, layout, recovery_workers)?);
        Ok(())
    }

    pub fn live_cell_index_status(&self) -> Option<LiveCellIndexStatus> {
        self.live_index.as_ref().map(IndexView::status)
    }

    #[cfg(feature = "jolteon-resident-cell-publication")]
    pub(crate) fn try_live_cell_index_status(&self) -> Option<LiveCellIndexStatus> {
        self.live_index.as_ref().and_then(IndexView::try_current_status)
    }

    /// Enable the optional bounded Cell Patricia node index.
    pub fn enable_cell_node_index(&mut self, limit_mib: NonZeroUsize) -> Result<()> {
        ensure!(
            self.branch.is_empty() && self.cell_nodes.is_none(),
            "Cell node index already configured or view is speculative"
        );
        let bytes = limit_mib.get().checked_mul(1 << 20).context("Cell node index budget overflow")?;
        self.cell_nodes = Some(Arc::new(NodeIndex::recover(self, bytes)?));
        Ok(())
    }

    pub fn cell_node_index_status(&self) -> Option<CellNodeIndexStatus> {
        self.cell_nodes.as_deref().map(NodeIndex::status)
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
            #[cfg(feature = "native-shared-pinned-cell-cache")]
            pinned_cell_cache: Arc::clone(&self.pinned_cell_cache),
            live_index: self.live_index.clone(),
            cell_nodes: self.cell_nodes.clone(),
            #[cfg(feature = "native-hot-cell-node-cache")]
            cell_node_header: self.cell_node_header,
            #[cfg(feature = "jolteon-cancel-stale-execution")]
            cancellation: self.cancellation.clone(),
        }
    }

    #[cfg(any(
        feature = "jolteon-admission-pipeline",
        feature = "jolteon-candidate-pipeline",
        feature = "jolteon-proposal-pipeline"
    ))]
    pub(crate) fn pinned_view(&self) -> PinnedView {
        let mut view = self.read_view(&[]);
        #[cfg(not(feature = "native-shared-pinned-cell-cache"))]
        {
            view.live_cache = Arc::default();
        }
        view.stake_inputs = Arc::default();
        PinnedView(view)
    }

    fn candidate_value(&self, key: &[u8]) -> Option<Option<&[u8]>> {
        self.branch
            .iter()
            .rev()
            .find_map(|prepared| prepared.batch.lookup(key).map(Option::as_deref))
    }

    fn read(&self, key: impl AsRef<[u8]>) -> Result<Option<ReadValue<'_>>> {
        let key = key.as_ref();
        if let Some(value) = self.candidate_value(key) {
            return Ok(value.map(ReadValue::Candidate));
        }
        Ok(self
            .db
            .get_opt(key, &self.read_options)?
            .map(ReadValue::Committed))
    }

    pub(crate) fn consensus_record(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        Ok(self.read(index_key(b'j', key))?.map(|value| value.to_vec()))
    }

    pub(crate) fn persist_consensus(&self, records: &[ConsensusRecord]) -> Result<()> {
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
        if let Some(record) = self.live_cell_in_memory(point)? {
            return Ok(record);
        }
        let record = self.read_stored_live_cell(point, &self.live_cell_key(point))?;
        self.remember_committed_live_cell(point, record.as_ref());
        Ok(record)
    }

    // Outer None requests a database lookup. An inner None is authoritative.
    fn live_cell_in_memory(&self, point: &OutPoint) -> Result<Option<Option<LiveCell>>> {
        if let Some(cell) = self.branch_cell(point) {
            return Ok(Some(cell.clone()));
        }
        if let Some(index) = &self.live_index
            && let Some(cell) = index.lookup(point).context(StateReadFault)?
        {
            return Ok(Some(cell));
        }
        Ok(self.cached_live_cell(point).map(Some))
    }

    fn read_stored_live_cell(&self, point: &OutPoint, key: &[u8]) -> Result<Option<LiveCell>> {
        self.db
            .get_opt(key, &self.read_options)
            .context(StateReadFault)?
            .map(|bytes| Self::decode_live_cell_value(point, bytes))
            .transpose()
    }

    fn remember_committed_live_cell(&self, point: &OutPoint, cell: Option<&LiveCell>) {
        // Candidate rows must never enter the shared committed cache.
        if let Some(cell) = cell {
            self.live_cache.insert(point.clone(), cell.clone());
        }
    }

    fn branch_cell(&self, point: &OutPoint) -> Option<&Option<LiveCell>> {
        #[cfg(feature = "native-compact-prepared-cell-delta")]
        let mut hash = None;
        self.branch.iter().rev().find_map(|prepared| {
            #[cfg(feature = "native-compact-prepared-cell-delta")]
            return prepared.live.get_hashed(point, &mut hash);
            #[cfg(not(feature = "native-compact-prepared-cell-delta"))]
            prepared.live.get(point)
        })
    }

    fn cached_live_cell(&self, point: &OutPoint) -> Option<LiveCell> {
        self.live_cache.get(point)
    }

    fn live_cell_key(&self, point: &OutPoint) -> [u8; 34] {
        let id = record::cell_key(&self.cells, point);
        state_key(STATE_RECORD, Namespace::Cells, &id)
    }

    fn decode_live_cell_value(point: &OutPoint, bytes: DBVector) -> Result<LiveCell> {
        #[cfg(feature = "native-owned-live-cell-reads")]
        return Self::check_live_cell_record(point, cell_read::decode_owned(bytes));
        #[cfg(not(feature = "native-owned-live-cell-reads"))]
        Self::decode_live_cell(point, &bytes)
    }

    fn decode_live_cell(point: &OutPoint, bytes: &[u8]) -> Result<LiveCell> {
        Self::check_live_cell_record(point, record::decode_cell(bytes))
    }

    fn check_live_cell_record(
        point: &OutPoint,
        decoded: Result<(OutPoint, LiveCell)>,
    ) -> Result<LiveCell> {
        let (stored_point, record) = decoded.context(StateReadFault)?;
        if &stored_point != point {
            return Err(
                anyhow::anyhow!("Live Cell key differs from its record").context(StateReadFault)
            );
        }
        Ok(record)
    }

    pub fn prove_cell(&self, point: &OutPoint) -> Result<(Option<LiveCell>, Proof)> {
        let key = record::cell_key(&self.cells, point);
        let mut tree = self.cells.edit(as_key(&self.tip.state.cell_root), |id| {
            self.node(Namespace::Cells, id)
        });
        let (actual, proof) = tree.prove(&key)?;
        Ok((self.live_cell_for_proof(point, actual)?, proof))
    }

    pub fn prove_cells(&self, points: &[OutPoint]) -> Result<Vec<(Option<LiveCell>, Proof)>> {
        ensure!(
            points.len() <= MAX_CELL_PROOF_BATCH,
            "too many Live Cell proofs in one batch"
        );
        let keys: Vec<_> = points
            .iter()
            .map(|point| record::cell_key(&self.cells, point))
            .collect();
        let root = as_key(&self.tip.state.cell_root);
        #[cfg(feature = "native-batched-patricia-proofs")]
        let proofs = if keys.len() > 1 {
            self.cells.prove_many_batched(root, &keys, |ids| self.cell_nodes_many(ids))?
        } else {
            self.cells.edit(root, |id| self.node(Namespace::Cells, id)).prove_many(&keys)?
        };
        #[cfg(not(feature = "native-batched-patricia-proofs"))]
        let proofs = self.cells.edit(root, |id| self.node(Namespace::Cells, id)).prove_many(&keys)?;
        #[cfg(feature = "native-batched-cell-proof-records")]
        if points.len() > 1 {
            return self.bind_cell_proof_records(points, &keys, proofs);
        }
        points
            .iter()
            .zip(proofs)
            .map(|(point, (actual, proof))| Ok((self.live_cell_for_proof(point, actual)?, proof)))
            .collect()
    }

    fn live_cell_for_proof(&self, point: &OutPoint, actual: Option<Key>) -> Result<Option<LiveCell>> {
        let record = self.live_cell(point)?;
        self.verify_live_cell_for_proof(point, actual, record.as_ref())?;
        Ok(record)
    }

    fn verify_live_cell_for_proof(
        &self,
        point: &OutPoint,
        actual: Option<Key>,
        record: Option<&LiveCell>,
    ) -> Result<()> {
        ensure!(
            actual == record.map(|record| record::cell_value(&self.cells, point, record)),
            "Live Cell record differs from its authenticated value"
        );
        Ok(())
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

    fn node(&self, namespace: Namespace, id: &Key) -> Result<Option<node_storage::NodeRead>> {
        let key = state_key(STATE_NODE, namespace, id);
        if let Some(row) = self.node_in_memory(namespace, id, &key)? {
            return Ok(row);
        }
        self.read_stored_node((namespace == Namespace::Cells).then_some(id), &key)
    }

    fn read_stored_node(
        &self,
        cell_id: Option<&Key>,
        key: &[u8],
    ) -> Result<Option<node_storage::NodeRead>> {
        self.db.get_opt(key, &self.read_options)?
            .as_deref()
            .map(|stored| self.decode_stored_node(cell_id, stored))
            .transpose()
    }

    fn decode_stored_node(&self, _cell_id: Option<&Key>, stored: &[u8]) -> Result<node_storage::NodeRead> {
        #[cfg(feature = "native-hot-cell-node-cache")]
        if let Some(id) = _cell_id
            && let Some(index) = &self.cell_nodes
        {
            return index.remember_stored(&self.cell_node_header, id, stored);
        }
        node_storage::read(stored)
    }

    // Outer None requests a database read. An inner None is a branch tombstone.
    fn node_in_memory(
        &self,
        namespace: Namespace,
        id: &Key,
        key: &[u8],
    ) -> Result<Option<Option<node_storage::NodeRead>>> {
        for prepared in self.branch.iter().rev() {
            #[cfg(feature = "native-verified-cell-journal")]
            if namespace == Namespace::Cells
                && let Some((_, changes)) = &prepared.batch.cell_nodes
            {
                if let Some(value) = changes.get(id) {
                    return Ok(Some(value));
                }
                continue;
            }
            if let Some(value) = prepared.batch.lookup(key) {
                return value.as_deref().map(node_storage::read).transpose().map(Some);
            }
        }
        if namespace == Namespace::Cells
            && let Some(index) = &self.cell_nodes
        {
            let node = index.lookup(id);
            #[cfg(feature = "native-hot-cell-node-cache")]
            let node = node.or_else(|| index.lookup_hot(&self.cell_node_header, id));
            if let Some(node) = node {
                return Ok(Some(Some(node)));
            }
        }
        Ok(None)
    }

    #[cfg(any(feature = "native-batched-patricia-proofs", feature = "native-patricia-frontier-edit"))]
    fn cell_nodes_many(&self, ids: &[Key]) -> Result<Vec<Option<node_storage::NodeRead>>> {
        let mut nodes: Vec<_> = (0..ids.len()).map(|_| None).collect();
        let mut missing = Vec::new();
        let mut keys = Vec::new();
        for (position, id) in ids.iter().enumerate() {
            let key = state_key(STATE_NODE, Namespace::Cells, id);
            if let Some(row) = self.node_in_memory(Namespace::Cells, id, &key)? {
                nodes[position] = row;
            } else {
                missing.push(position);
                keys.push(key);
            }
        }
        if keys.len() < 4 {
            for (position, key) in missing.into_iter().zip(keys) {
                nodes[position] = self.read_stored_node(Some(&ids[position]), &key)?;
            }
        } else {
            let values = self.db.multi_get_opt(keys, &self.read_options);
            for (position, value) in missing.into_iter().zip(values) {
                nodes[position] = value?.as_deref()
                    .map(|stored| self.decode_stored_node(Some(&ids[position]), stored))
                    .transpose()?;
            }
        }
        Ok(nodes)
    }

    #[inline]
    fn check_preparation_cancelled(&self) -> Result<()> {
        #[cfg(feature = "jolteon-cancel-stale-execution")]
        if self.cancellation.as_ref().is_some_and(PreparationCancellation::is_cancelled) {
            return Err(PreparationCancelled.into());
        }
        Ok(())
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
        self.check_preparation_cancelled()?;
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
        let total_bytes = due
            .commands
            .iter()
            .chain(transactions)
            .try_fold(0u64, |total, tx| {
                total.checked_add(tx.as_slice().len() as u64)
            })
            .context("block transaction size overflow")?;
        ensure!(
            total_bytes <= budget.bytes,
            "block transaction bytes exceeded"
        );
        #[cfg(feature = "native-execution-material-cache")]
        let materialized: Vec<_> = transactions
            .iter()
            .map(|tx| tx.with_execution_material(&self.network))
            .collect();
        #[cfg(feature = "native-execution-material-cache")]
        let transactions = materialized.as_slice();
        let with_due = (!due.commands.is_empty()).then(|| {
            due.commands
                .iter()
                .chain(transactions)
                .cloned()
                .collect::<Vec<_>>()
        });
        let execution = with_due.as_deref().unwrap_or(transactions);
        let block = BlockContext::after(
            &self.network,
            &self.tip,
            timestamp_ms,
            Tree::transactions(self.network.clone(), execution)?.root(),
            self.capacity,
        )?;
        let origin = Origin::new(&self.network, block.candidate());
        let (mut receipts, keeper_wakes) =
            self.run_keeper(&due, &block, &origin, &mut budget, &mut overlay, parallel)?;
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
        let prefetch_cells = cfg!(feature = "native-candidate-cell-prefetch")
            && width > 1
            && !self.live_index.as_ref().is_some_and(IndexView::resident);
        let mut lineage_schedule = LineageSchedule { keeper_wakes, ..LineageSchedule::default() };
        #[cfg(any(feature = "native-rolling-cell-execution", feature = "native-shared-cell-deps"))]
        let mut cell_stream = CellStreamStats::default();
        #[cfg(feature = "native-shared-cell-deps")]
        let mut dependency_reads = None;
        // Account for previous validation before admitting a dynamic wave.
        // Disjoint commands may materialize together when each has its full limit.
        for mut segment in transactions
            .chunk_by(|left, right| left.has_lineage_command() == right.has_lineage_command())
        {
            let lineage = segment[0].has_lineage_command();
            let mut prefetch_remaining = 0usize;
            while !segment.is_empty() {
                self.check_preparation_cancelled()?;
                #[cfg(feature = "native-lineage-dependency-lookahead")]
                if lineage && width > 1 {
                    let count = self.prepare_lineage_lookahead(
                        segment, &block, &origin, width, &mut budget, &mut overlay,
                        &mut receipts, &mut fees, &mut lineage_schedule,
                    )?;
                    if count != 0 {
                        segment = &segment[count..];
                        continue;
                    }
                }
                if cfg!(feature = "native-dependency-execution") && width > 1 && !lineage {
                    #[cfg(feature = "native-rolling-cell-execution")]
                    let count = self.prepare_cell_run(
                        segment, &block, &origin, width, &mut budget, &mut overlay,
                        &mut receipts, &mut fees, &mut cell_stream,
                        #[cfg(feature = "native-shared-cell-deps")]
                        &mut dependency_reads,
                    )?;
                    #[cfg(not(feature = "native-rolling-cell-execution"))]
                    let count = 0;
                    let count = if count == 0 {
                        self.prepare_cell_window(
                            segment, &block, &origin, width, &mut budget, &mut overlay,
                            &mut receipts, &mut fees,
                            #[cfg(feature = "native-shared-cell-deps")]
                            &mut dependency_reads,
                        )?
                    } else {
                        count
                    };
                    if count != 0 {
                        segment = &segment[count..];
                        prefetch_remaining = prefetch_remaining.saturating_sub(count);
                        continue;
                    }
                }
                if prefetch_cells && !lineage && prefetch_remaining == 0 {
                    prefetch_remaining = segment.len().min(32);
                    self.prefetch_candidate_cells(&segment[..prefetch_remaining], &mut overlay);
                }
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
                let mut parallel_commands = false;
                if count > 1
                    && lineage
                    && budget.cycles / count as u64 >= self.limits.transaction_cycles
                {
                    let started = Instant::now();
                    let ready =
                        lineage::independent_prefix(self, &segment[..count], &overlay.lineages)?;
                    lineage_schedule.access_analysis_us += started.elapsed().as_micros() as u64;
                    if cfg!(feature = "native-lineage-ready-prefix") {
                        count = ready.max(1);
                    }
                    parallel_commands = count > 1 && ready == count;
                }
                let (wave, rest) = segment.split_at(count);
                segment = rest;
                prefetch_remaining = prefetch_remaining.saturating_sub(count);
                let mut commands = if parallel_commands {
                    let started = Instant::now();
                    let materials = wave
                        .par_iter()
                        .map(|tx| self.materialize_lineage(tx, &block, &overlay, budget))
                        .collect::<Result<Vec<_>>>()?;
                    lineage_schedule.parallel_materialize_us += started.elapsed().as_micros() as u64;
                    lineage_schedule.parallel_waves += 1;
                    lineage_schedule.parallel_commands += wave.len() as u64;
                    materials.into_iter()
                } else {
                    if lineage {
                        lineage_schedule.serial_commands += wave.len() as u64;
                    }
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
                        let verified = self.check_preparation_cancelled()
                            .and_then(|()| self.cache.verify(resolved, limit, group_parallel));
                        (position, verified)
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
            overlay.prefetched_cells.clear();
        }
        let mut prepared = self.finish_block(
            block.into_candidate(),
            overlay,
            execution,
            receipts,
            due.commands.len(),
            fees,
            parallel,
        )?;
        prepared.lineage_schedule = lineage_schedule;
        #[cfg(feature = "native-shared-cell-deps")]
        if let Some(reads) = dependency_reads {
            cell_stream.dependency_reads = reads.stats();
        }
        #[cfg(any(feature = "native-rolling-cell-execution", feature = "native-shared-cell-deps"))]
        {
            prepared.cell_stream = cell_stream;
        }
        Ok(prepared)
    }

    fn prefetch_candidate_cells(&self, transactions: &[Transaction], overlay: &mut Overlay) {
        overlay.prefetched_cells.clear();
        if transactions.len() < 8 {
            return;
        }
        let mut points = Vec::with_capacity(transactions.len() * 2);
        for tx in transactions {
            let mut inputs = tx.inputs();
            if inputs.len() == 1
                && tx.outputs().len() == 1
                && tx.operation().is_none()
                && tx.keeper_action().is_none()
                && tx.l1_reference_certificate().is_none()
            {
                points.push(inputs.next().expect("one input").previous_output);
                points.push(OutPoint::new(tx.id(&self.network), 0));
            }
        }
        points.sort_unstable();
        points.dedup();
        points.retain(|point| !overlay.live.contains_key(point));
        let reads: Vec<_> = points
            .into_par_iter()
            .filter_map(|point| self.prefetch_cell(&point, 16 * 1024).map(|cell| (point, cell)))
            .collect();
        overlay.prefetched_cells.extend(reads);
    }

    fn prefetch_cell(&self, point: &OutPoint, max_record_bytes: usize) -> Option<Option<LiveCell>> {
        if let Some(cell) = self.branch_cell(point) {
            return cell
                .as_ref()
                .is_none_or(|cell| LiveCache::record_bytes(cell) <= max_record_bytes)
                .then(|| cell.clone());
        }
        // A failed resident read must reach the canonical resolver. Do not
        // turn a lower-tier result into a hint that could mask that error.
        if let Some(index) = &self.live_index
            && let Some(cell) = index.lookup(point).ok()?
        {
            return cell
                .as_ref()
                .is_none_or(|cell| LiveCache::record_bytes(cell) <= max_record_bytes)
                .then_some(cell);
        }
        if let Some(cell) = self.cached_live_cell(point) {
            return (LiveCache::record_bytes(&cell) <= max_record_bytes).then_some(Some(cell));
        }
        // Keep tentative records local to the candidate and skip oversized
        // values before decoding. An I/O error is retried in canonical order.
        let bytes = self
            .db
            .get_pinned_opt(self.live_cell_key(point), &self.read_options)
            .ok()?;
        match bytes {
            None => Some(None),
            Some(bytes) if bytes.len() <= max_record_bytes => {
                Self::decode_live_cell(point, &bytes).ok().map(Some)
            }
            Some(_) => None,
        }
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
        self.check_preparation_cancelled()?;
        let started = Instant::now();
        let height = context.height;
        let live = std::mem::take(&mut overlay.live);
        let mut batch = Batch::default();
        let build_execution = || {
            timed(|| {
                self.check_preparation_cancelled()?;
                encode_execution(&self.network, execution, &receipts)
            })
        };
        let (cells, system, execution) =
            if cfg!(feature = "native-commitment-pipeline")
                && parallel
                && execution.len() >= 256
                && rayon::current_num_threads() > 1
            {
                // Isolate each namespace's writes and append them in the
                // original order before producing the single atomic batch.
                let mut system_batch = Batch::default();
                let (cell, (system, execution)) = rayon::join(
                    || timed(|| self.finish_cells(&mut batch, live, parallel)),
                    || {
                        rayon::join(
                            || timed(|| self.finish_system(&mut system_batch, overlay, height, fees)),
                            build_execution,
                        )
                    },
                );
                let result = (cell?, system?, execution?);
                batch.writes.append(&mut system_batch.writes);
                result
            } else {
                (
                    timed(|| self.finish_cells(&mut batch, live, parallel))?,
                    timed(|| self.finish_system(&mut batch, overlay, height, fees))?,
                    build_execution()?,
                )
            };
        let ((live, cell_root, cell_phases), cells_time) = cells;
        let (system, system_time) = system;
        let ((execution_bytes, receipt_root), execution_time) = execution;
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
        #[cfg(feature = "native-prepared-live-cell-ownership")]
        let (live, detached_live_records) = {
            let mut live = live;
            if self.live_index.is_some() {
                self.check_preparation_cancelled()?;
            }
            let detached = self
                .live_index
                .as_ref()
                .is_some_and(|index| index.detach_inline_records(&mut live));
            (live, detached)
        };
        #[cfg(feature = "native-compact-prepared-cell-delta")]
        let live = {
            self.check_preparation_cancelled()?;
            live.with_index()
        };
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
            #[cfg(feature = "native-prepared-live-cell-ownership")]
            detached_live_records,
            batch,
            commitments: CommitmentTiming {
                total: started.elapsed(),
                cells: cells_time,
                cell_phases,
                system: system_time,
                execution: execution_time,
            },
            lineage_schedule: LineageSchedule::default(),
            cell_stream: CellStreamStats::default(),
        })
    }

    fn finish_cells(
        &self,
        batch: &mut Batch,
        live: LiveChanges,
        parallel: bool,
    ) -> Result<(PreparedCells, Byte32, CellCommitmentTiming)> {
        self.check_preparation_cancelled()?;
        let started = Instant::now();
        let live = PreparedCells::from(live);
        let hash_change = |(point, cell): (&OutPoint, &Option<LiveCell>)| {
            let id = record::cell_key(&self.cells, point);
            let value = cell.as_ref().map(|cell| record::cell_value(&self.cells, point, cell));
            (id, value)
        };
        let mut cell_changes = Vec::with_capacity(live.len());
        const HASH_GRAIN: usize = 128;
        if cfg!(feature = "native-parallel-cell-record-hashes")
            && parallel
            && live.len() >= 2 * HASH_GRAIN
            && rayon::current_num_threads() > 1
        {
            // Created IDs must follow the same OutPoint order as persistence.
            #[cfg(feature = "native-compact-prepared-cell-delta")]
            let entries = live.par_iter();
            #[cfg(not(feature = "native-compact-prepared-cell-delta"))]
            let entries = live.iter().collect::<Vec<_>>().into_par_iter();
            entries
                .with_min_len(HASH_GRAIN)
                .map(hash_change)
                .collect_into_vec(&mut cell_changes);
            self.check_preparation_cancelled()?;
            #[cfg(feature = "native-prepared-cell-record-keys")]
            batch.created_cell_ids.extend(
                cell_changes.iter().filter_map(|(id, value)| value.as_ref().map(|_| *id)),
            );
        } else {
            for change in live.iter() {
                let (id, value) = hash_change(change);
                #[cfg(feature = "native-prepared-cell-record-keys")]
                if value.is_some() {
                    batch.created_cell_ids.push(id);
                }
                cell_changes.push((id, value));
            }
        }
        cell_changes.sort_unstable_by_key(|(id, _)| *id);
        let keyed = Instant::now();
        let root = as_key(&self.tip.state.cell_root);
        let reader = |id: &Key| self.node(Namespace::Cells, id);
        let previous = |id: Key, _, replacement: Option<Key>| {
            if replacement.is_none() {
                batch.delete(state_key(STATE_RECORD, Namespace::Cells, &id));
            }
        };
        #[cfg(feature = "native-verified-cell-journal")]
        let (cell_root, tree_done) = {
            #[cfg(feature = "native-patricia-frontier-edit")]
            let updated = if parallel {
                self.cells.batch_update_verified_frontier(
                    root,
                    reader,
                    |ids| {
                        self.check_preparation_cancelled()?;
                        self.cell_nodes_many(ids)
                    },
                    &cell_changes,
                    previous,
                )
            } else {
                self.cells.batch_update_verified(root, reader, &cell_changes, previous, false)
            };
            #[cfg(not(feature = "native-patricia-frontier-edit"))]
            let updated = self.cells.batch_update_verified(
                root, reader, &cell_changes, previous, parallel,
            );
            let (root, journal) = updated?;
            let tree_done = Instant::now();
            batch.cell_nodes = Some((batch.writes.len(), journal));
            (root, tree_done)
        };
        #[cfg(not(feature = "native-verified-cell-journal"))]
        let (cell_root, tree_done) = {
            #[cfg(feature = "native-cell-subtree-parallel")]
            let updated = if parallel {
                self.cells.batch_update_parallel(root, reader, &cell_changes, previous)
            } else {
                self.cells.batch_update(root, reader, &cell_changes, previous)
            };
            #[cfg(not(feature = "native-cell-subtree-parallel"))]
            let updated = {
                let _ = parallel;
                self.cells.batch_update(root, reader, &cell_changes, previous)
            };
            let (root, nodes) = updated?;
            let tree_done = Instant::now();
            put_nodes(batch, Namespace::Cells, nodes);
            (root, tree_done)
        };
        Ok((
            live,
            cell_root.pack(),
            CellCommitmentTiming {
                keys: keyed.duration_since(started),
                tree: tree_done.duration_since(keyed),
                batch: tree_done.elapsed(),
            },
        ))
    }

    fn finish_system(
        &self,
        batch: &mut Batch,
        overlay: Overlay,
        height: u64,
        fees: u64,
    ) -> Result<SystemCommitment> {
        self.check_preparation_cancelled()?;
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
            .stage_proposal(
                transactions,
                timestamp_ms,
                max_bytes,
                &mut ProposalScan::default(),
                None,
            )?
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
        self.build_proposal_measured(
            transactions,
            timestamp_ms,
            max_bytes,
            &mut ProposalScan::default(),
            None,
        )
    }

    pub(crate) fn build_proposal_measured<T: Borrow<Transaction>>(
        &self,
        transactions: impl IntoIterator<Item = T>,
        timestamp_ms: u64,
        max_bytes: u64,
        scan: &mut ProposalScan,
        cycle_scan_limit: Option<u64>,
    ) -> Result<(Vec<Transaction>, Prepared)> {
        let started = Instant::now();
        let selection =
            self.stage_proposal(transactions, timestamp_ms, max_bytes, scan, cycle_scan_limit);
        scan.selection_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let ProposalSelection {
            transactions,
            mut keeper_commands,
            mut context,
            mut overlay,
            receipts,
            reusable,
        } = selection?;
        let prepared = if reusable {
            let keeper_count = keeper_commands.len();
            let execution = if keeper_count == 0 {
                &transactions
            } else {
                keeper_commands.extend_from_slice(&transactions);
                &keeper_commands
            };
            context.transaction_root = Tree::transactions(self.network.clone(), execution)?.root();
            let origin = Origin::new(&self.network, &context);
            for record in overlay.live.values_mut().flatten() {
                record.origin = origin.clone();
            }
            let fees = receipts
                .iter()
                .try_fold(0u64, |sum, receipt| sum.checked_add(receipt.fee))
                .context("block fee overflow")?;
            let mut prepared =
                self.finish_block(context, overlay, execution, receipts, keeper_count, fees, true)?;
            prepared.lineage_schedule.keeper_wakes = scan.keeper_wakes;
            scan.keeper_prefix_reused = keeper_count as u64;
            prepared
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
        scan: &mut ProposalScan,
        cycle_scan_limit: Option<u64>,
    ) -> Result<ProposalSelection> {
        let mut budget = Budget::new(self.limits);
        let mut overlay = self.overlay(self.stage_stake()?);
        let block = BlockContext::after(
            &self.network,
            &self.tip,
            timestamp_ms,
            Byte32::default(),
            self.capacity,
        )?;
        let origin = Origin::new(&self.network, block.candidate());
        let due = self.due(block.candidate().height, &mut budget)?;
        let (receipts, keeper_wakes) =
            self.run_keeper(&due, &block, &origin, &mut budget, &mut overlay, true)?;
        scan.keeper_wakes = keeper_wakes;
        let mut progress = ProposalProgress {
            transactions: Vec::new(),
            receipts,
            reusable: due.can_rebind_proposal(),
            wire_bytes: max_bytes,
            cycle_rejections: 0,
            cycle_scan_limit,
        };
        let width = if cfg!(feature = "native-leader-dependency-execution") {
            rayon::current_num_threads()
        } else {
            1
        };
        #[cfg(feature = "native-execution-material-cache")]
        let transactions = transactions
            .into_iter()
            .map(|item| item.borrow().with_execution_material(&self.network));
        let mut transactions = transactions.into_iter().peekable();
        let mut window = ProposalWindow::new();
        loop {
            self.check_preparation_cancelled()?;
            if progress.transactions.len() + due.commands.len() == self.limits.transaction_count as usize
                || budget.bytes == 0
                || progress.wire_bytes == 0
            {
                break;
            }
            #[cfg(feature = "native-leader-lineage-execution")]
            if window.is_empty()
                && let Some(stopped) = self.stage_lineage_proposal(
                    &mut transactions, &block, &origin, &mut budget, &mut overlay,
                    &mut progress, scan, due.commands.len(),
                )?
            {
                if stopped {
                    break;
                }
                continue;
            }
            #[cfg(feature = "native-rolling-leader-execution")]
            if width > 1
                && let Some(stopped) = self.stage_rolling_proposal(
                    &mut transactions, &mut window, &block, &origin, &mut budget, &mut overlay,
                    &mut progress, scan, width, due.commands.len(),
                )?
            {
                if stopped {
                    break;
                }
                continue;
            }
            let next = if width > 1 {
                let available =
                    self.limits.transaction_count as usize - progress.transactions.len() - due.commands.len();
                window.next(
                    &mut transactions,
                    self,
                    &block,
                    &origin,
                    &overlay,
                    budget,
                    progress.wire_bytes,
                    available,
                    width,
                    scan,
                )?
            } else {
                transactions.next().map(|item| (item, None))
            };
            let Some((item, trial)) = next else {
                break;
            };
            let fixed = &mut window;
            let trial = match trial {
                Some(Ok(work)) => {
                    ProposalTrial::Ready(move |budget: &mut Budget, overlay: &mut Overlay| {
                        fixed.adopt(work, budget, overlay)
                    })
                }
                Some(Err(error)) => ProposalTrial::failed(error),
                None => ProposalTrial::Ordered,
            };
            let step = self.stage_proposal_entry(
                item.borrow(), &block, &origin, &mut budget, &mut overlay, &mut progress,
                scan, trial, None,
            )?;
            if matches!(step, ProposalStep::Stop) {
                break;
            }
        }
        #[cfg(feature = "native-shared-cell-deps")]
        scan.dependency_reads.record(window.dependency_stats());
        Ok(ProposalSelection {
            transactions: progress.transactions,
            keeper_commands: due.commands,
            context: block.into_candidate(),
            overlay,
            receipts: progress.receipts,
            reusable: progress.reusable,
        })
    }

    fn stage_proposal_entry(
        &self,
        tx: &Transaction,
        block: &BlockContext<'_>,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
        progress: &mut ProposalProgress,
        scan: &mut ProposalScan,
        trial: ProposalTrial<impl FnOnce(&mut Budget, &mut Overlay) -> Option<Receipt>>,
        canonical: Option<&PrecedingCells<'_>>,
    ) -> Result<ProposalStep> {
        self.check_preparation_cancelled()?;
        let priority = u64::from(
            cfg!(feature = "jolteon-aged-proposal-selection")
                && scan.inspected < scan.priority_candidates,
        );
        scan.inspected = scan.inspected.saturating_add(1);
        scan.priority_inspected = scan.priority_inspected.saturating_add(priority);
        let started = Instant::now();
        let len = tx.as_slice().len();
        if len as u64 > budget.bytes || lineage::minimum_reads(tx) > budget.reads {
            return Ok(ProposalStep::Skipped);
        }
        let Some(next_wire) = progress.wire_bytes.checked_sub(proposal_entry_bytes(len)) else {
            return Ok(ProposalStep::Skipped);
        };
        // Without Header Deps the VM has no origin-based Header to load.
        // Ordinary calls and single Lineage commands see the same height
        // and parent throughout. Keep the full replay for other inspected
        // operations, including any of them that failed selection.
        progress.reusable &= tx.header_deps().len() == 0
            && tx.operation().is_none()
            && tx.keeper_action().is_none()
            && tx.keeper_schedule().is_none()
            && tx.lineage_batch().is_none();
        // For ordinary worker trials without Header Deps, the first input
        // precedes any other state read. Reuse its rejection only when the
        // canonical prefix leaves the exact parent tombstone authoritative.
        #[cfg(feature = "native-reuse-parent-spend-rejections")]
        if let ProposalTrial::Missing(point) = &trial
            && tx.header_deps().len() == 0
            && tx.inputs().next().is_some_and(|input| &input.previous_output == point)
            && !overlay.live.contains_key(point)
            && canonical.is_none_or(|view| view.get(point, |_| ()).is_none())
            && matches!(self.branch_cell(point), Some(None))
        {
            scan.record_parent_spend(priority, started.elapsed());
            scan.parallel_parent_rejections_reused =
                scan.parallel_parent_rejections_reused.saturating_add(1);
            return Ok(ProposalStep::RejectedParentSpend);
        }
        #[cfg(feature = "native-leader-lineage-execution")]
        let (trial, material) = match trial {
            ProposalTrial::Lineage(material, attempted) => (
                ProposalTrial::Ordered,
                self.reuse_lineage_materialization(material, *budget, attempted, &mut scan.lineage),
            ),
            ordinary => (ordinary, None),
        };
        #[cfg(not(feature = "native-leader-lineage-execution"))]
        let material = None;
        let had_trial = !matches!(trial, ProposalTrial::Ordered);
        let receipt = match trial {
            ProposalTrial::Ready(adopt) => adopt(budget, overlay),
            _ => None,
        };
        if had_trial {
            if receipt.is_some() {
                scan.parallel_cell_reused = scan.parallel_cell_reused.saturating_add(1);
            } else {
                scan.parallel_cell_fallback = scan.parallel_cell_fallback.saturating_add(1);
            }
        }
        let reused = receipt.is_some();
        let verify = |resolved, limit| self.cache.verify(resolved, limit, true);
        let staged = match (receipt, canonical) {
            (Some(receipt), _) => Ok(receipt),
            (None, Some(view)) => self.stage_transaction(
                tx, block, origin, budget, overlay, None, verify, None, Some(view),
                #[cfg(feature = "native-compact-cell-trial-edits")]
                None,
            ),
            (None, None) => self.stage(tx, block, origin, budget, overlay, material, verify),
        };
        match staged {
            Ok(receipt) => {
                scan.selected = scan.selected.saturating_add(1);
                scan.priority_selected = scan.priority_selected.saturating_add(priority);
                progress.wire_bytes = next_wire;
                progress.receipts.push(receipt);
                progress.transactions.push(tx.clone());
            }
            Err(error) if error.is::<StateReadFault>() => return Err(error),
            #[cfg(feature = "jolteon-cancel-stale-execution")]
            Err(error) if error.is::<PreparationCancelled>() => return Err(error),
            Err(error) => {
                if error.is::<BlockVmBudgetExceeded>() {
                    progress.cycle_rejections += 1;
                    scan.block_cycle_rejections = scan.block_cycle_rejections.saturating_add(1);
                    scan.priority_cycle_rejections = scan.priority_cycle_rejections.saturating_add(priority);
                    scan.block_cycle_rejection_us = scan.block_cycle_rejection_us.saturating_add(
                        u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                    );
                    if progress.cycle_scan_limit == Some(progress.cycle_rejections) {
                        scan.cycle_scan_stops = scan.cycle_scan_stops.saturating_add(1);
                        return Ok(ProposalStep::Stop);
                    }
                }
                if let Some(MissingLiveCell(point)) = error.downcast_ref()
                    && tx.inputs().any(|input| &input.previous_output == point)
                {
                    let spent = overlay.live.get(point).map(Option::is_none)
                        .or_else(|| canonical.and_then(|view| view.get(point, |record| record.is_none())));
                    if spent == Some(true) {
                        scan.spent_in_proposal = scan.spent_in_proposal.saturating_add(1);
                        scan.priority_spent_in_proposal = scan.priority_spent_in_proposal.saturating_add(priority);
                    } else if matches!(self.branch_cell(point), Some(None)) {
                        scan.record_parent_spend(priority, started.elapsed());
                    }
                }
                return Ok(ProposalStep::Skipped);
            }
        }
        Ok(if reused { ProposalStep::Reused } else { ProposalStep::Ordered })
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
            prefetched_cells: HashMap::new(),
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
        .with_context(|| MissingLiveCell(point.clone()))
    }

    fn materialize_lineage(
        &self,
        tx: &Transaction,
        block: &BlockContext<'_>,
        overlay: &Overlay,
        budget: Budget,
    ) -> Result<Option<lineage::Materialized>> {
        self.materialize_lineage_with_reader(tx, block, overlay, budget, |point| {
            self.resolve_live(&overlay.live, point)
        })
    }

    fn materialize_lineage_with_reader(
        &self,
        tx: &Transaction,
        block: &BlockContext<'_>,
        overlay: &Overlay,
        budget: Budget,
        read: impl FnMut(&OutPoint) -> Result<LiveCell>,
    ) -> Result<Option<lineage::Materialized>> {
        let height = block.candidate().height;
        ensure!(
            tx.keeper_schedule().is_none_or(|schedule| {
                overlay.keeper.running
                    && schedule.ready(height, block.parent().context.timestamp_ms)
            }),
            "trigger-bound Lineage command requires its committed Keeper reservation"
        );
        self.materialize_lineage_from_view(tx, block, &overlay.lineages, budget, read)
    }

    fn materialize_lineage_from_view(
        &self,
        tx: &Transaction,
        block: &BlockContext<'_>,
        heads: &lineage::Heads,
        budget: Budget,
        read: impl FnMut(&OutPoint) -> Result<LiveCell>,
    ) -> Result<Option<lineage::Materialized>> {
        tx.lineage_command()
            .map(|command| {
                ensure!(
                    block.candidate().height < tx.valid_before_height().expect("validated command expiry"),
                    "Lineage command has expired"
                );
                lineage::materialize(
                    self,
                    tx,
                    command,
                    block,
                    heads,
                    budget,
                    read,
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
            None,
            #[cfg(feature = "native-compact-cell-trial-edits")]
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
                        None,
                        #[cfg(feature = "native-compact-cell-trial-edits")]
                        None,
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
        preceding: Option<&PrecedingCells<'_>>,
        #[cfg(feature = "native-compact-cell-trial-edits")]
        trial_edits: Option<&mut cell_execution::TrialCellEdits>,
    ) -> Result<Receipt> {
        self.check_preparation_cancelled()?;
        let context = block.candidate();
        let tx_bytes =
            tx.as_slice().len() as u64 + batch.as_ref().map_or(0, |item| item.extra_bytes);
        let mut next = *budget;
        charge(&mut next.bytes, tx_bytes, "block transaction bytes")?;
        #[cfg(feature = "native-reuse-lineage-cell-reads")]
        let mut materialized_reads = (staged_command.is_none()
            && preceding.is_none()
            && tx.has_lineage_command())
            .then(lineage::ReadCache::default);
        let materialized = if staged_command.is_some() {
            staged_command
        } else {
            #[cfg(feature = "native-reuse-lineage-cell-reads")]
            let materialized = if let Some(reads) = &mut materialized_reads {
                // This cache lives inside one ordered staging call. Reads from
                // a background materialization are never substituted here.
                self.materialize_lineage_with_reader(tx, block, overlay, next, |point| {
                    reads.load(point, |point| self.resolve_live(&overlay.live, point))
                })?
            } else {
                self.materialize_lineage(tx, block, overlay, next)?
            };
            #[cfg(not(feature = "native-reuse-lineage-cell-reads"))]
            let materialized = self.materialize_lineage(tx, block, overlay, next)?;
            materialized
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
            prefetched_cells,
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

        let preceding = if cfg!(any(
            feature = "native-dependency-execution",
            feature = "native-leader-dependency-execution"
        )) {
            preceding
        } else {
            None
        };
        let lookup = |point: &OutPoint| {
            #[cfg(feature = "native-shared-cell-deps")]
            if let Some(record) = preceding.and_then(|view| view.parent_hint(point)) {
                return Ok(record.clone());
            }
            match prefetched_cells.get(point) {
                Some(cell) => Ok(cell.clone()),
                None => self.live_cell(point),
            }
        };
        let speculative = preceding.is_some_and(PrecedingCells::is_speculative);
        // Current candidate writes, including deletions, supersede all hints.
        let staged = |point: &OutPoint| {
            live.get(point)
                .cloned()
                .or_else(|| preceding.and_then(|view| view.get(point, |record| record.cloned())))
        };
        let speculative_bytes = std::cell::Cell::new(0usize);
        let fresh = |point: &OutPoint| {
            let record = staged(point);
            #[cfg(feature = "native-reuse-lineage-cell-reads")]
            let record = record.or_else(|| {
                let reads = materialized_reads.as_ref()?;
                reads.get(point).cloned().map(Some)
            });
            let record = match record {
                Some(record) => record,
                None => lookup(point)?,
            }
            .with_context(|| MissingLiveCell(point.clone()))?;
            if speculative {
                let used = speculative_bytes.get().saturating_add(record.cell.as_slice().len());
                ensure!(used <= MAX_SPECULATIVE_READ_BYTES, "large Cell reads need ordered execution");
                speculative_bytes.set(used);
            }
            Ok(record)
        };
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
        let lineage_changes = match &self.lineage {
            Some(code) if speculative => {
                ensure!(
                    !resolved.groups().iter().any(|group| {
                        group.kind == stem_ledger::ScriptGroupKind::Type
                            && stem_ledger::lineage::is_marker(code, &group.script)
                    }),
                    "Lineage registry changes need ordered execution"
                );
                lineage::Heads::new()
            }
            Some(code) => {
                lineage::changes(self, code, lineages, &resolved, &id, &changes.omit_outputs)?
            }
            None => lineage::Heads::new(),
        };
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
                let edited = live.contains_key(&point)
                    || preceding.is_some_and(|view| view.get(&point, |_| ()).is_some());
                ensure!(!edited && lookup(&point)?.is_none(), "duplicate output OutPoint");
                Ok((
                    point,
                    Some(LiveCell {
                        cell: cell.clone(),
                        origin: origin.clone(),
                    }),
                ))
            })
            .collect::<Result<CellOutputEdits>>()?;
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
        let transaction_cycle_limit = self
            .limits
            .transaction_cycles
            .checked_sub(keeper_cycles + reference_cycles)
            .context("authorization exceeds transaction cycles")?;
        let transaction_cycle_limit = if tx.operation().is_none() {
            transaction_cycle_limit.min(
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
            transaction_cycle_limit
        };
        let cycle_limit = transaction_cycle_limit.min(next.cycles);
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
                            preceding,
                            #[cfg(feature = "native-compact-cell-trial-edits")]
                            trial_edits,
                        );
                    }
                }
            } else {
                // The materializer has already verified this exact fee-only transaction.
                0
            };
            command.cycles + validation_cycles
        } else {
            verify(Arc::new(resolved), cycle_limit).map_err(|error| {
                if next.cycles < transaction_cycle_limit && stem_vm::is_cycle_limit(&error) {
                    error.context(BlockVmBudgetExceeded)
                } else {
                    error
                }
            })?
        };
        charge(&mut next.cycles, cycles, "block VM cycles")?;

        let protocol_result = materialized.as_ref().map_or_else(
            || changes.protocol_result.clone(),
            |command| stem_ledger::lineage::Outcome::from_applied(command.executed).encode(),
        );
        #[cfg(feature = "native-reuse-ordered-cell-writes")]
        let published_declaration = preceding
            .and_then(|view| view.ordered_writes)
            .filter(|_| {
                // Ordinary resolution takes the original inputs and outputs
                // and derives the output ID from that same transaction. No
                // protocol stage may substitute or suppress a declared edit.
                materialized.is_none()
                    && operation.is_none()
                    && batch.is_none()
                    && reference_cycles == 0
                    && keeper_change.is_none()
                    && tx.keeper_schedule().is_none()
                    && tx.messages().len() == 0
                    && lineage_changes.is_empty()
                    && changes.omit_outputs.is_empty()
            });
        let cell_changes = concrete
            .inputs()
            .map(|input| (input.previous_output, None))
            .chain(outputs);
        if let Some(member) = batch {
            member.journal.live.write(live, cell_changes);
            member.journal.lineages.write(lineages, lineage_changes);
        } else {
            #[cfg(feature = "native-compact-cell-trial-edits")]
            if let Some(edits) = trial_edits {
                // Trials have a private overlay and only publish after adoption.
                edits.extend(cell_changes);
            } else {
                live.extend(cell_changes);
            }
            #[cfg(not(feature = "native-compact-cell-trial-edits"))]
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
        #[cfg(feature = "native-reuse-ordered-cell-writes")]
        if let Some(published) = published_declaration {
            published.set(true);
        }
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
        records: &[ConsensusRecord],
        retired: &[Range<Vec<u8>>],
    ) -> Result<()> {
        self.check_commit_parent(prepared)?;
        sync_with_consensus(
            &self.db,
            &prepared.batch,
            Some((&self.cells, &prepared.live)),
            records,
            retired,
            self.metrics.as_deref().map(|m| (m, Purpose::Finality)),
        )?;
        self.install_durable_tip(prepared, false);
        Ok(())
    }

    fn check_commit_parent(&self, prepared: &Prepared) -> Result<()> {
        ensure!(self.branch.is_empty(), "cannot commit a speculative view");
        ensure!(
            prepared.parent == self.tip.id(&self.network),
            "stale prepared block"
        );
        Ok(())
    }

    #[cfg(feature = "jolteon-prebuilt-state-write")]
    pub(crate) fn prebuild_certified_state(
        &self,
        prepared: Arc<Prepared>,
    ) -> Option<impl FnOnce(&mut CertifiedWriteCache) + Send + 'static> {
        self.check_commit_parent(&prepared).ok()?;
        let cells = self.cells.clone();
        let measured = self.metrics.is_some();
        Some(move |cache: &mut CertifiedWriteCache| {
            if let Some(previous) = cache.state.take() {
                #[cfg(feature = "jolteon-reuse-write-buffer")]
                cache.recycle(previous.prefix.raw);
                #[cfg(not(feature = "jolteon-reuse-write-buffer"))]
                drop(previous);
            }
            let started = Instant::now();
            if let Ok((prefix, proposals)) = assemble_write_prefix(
                &prepared.batch, Some((&cells, &prepared.live)), measured, cache.take_buffer(),
            )
                && proposals.is_empty()
            {
                cache.state = Some(CachedStateWrite {
                    candidate: Arc::downgrade(&prepared),
                    prefix,
                    build: started.elapsed(),
                });
            }
        })
    }

    #[cfg(feature = "jolteon-commit-pipeline")]
    pub(crate) fn certified_write(
        &self,
        prepared: Arc<Prepared>,
        records: Vec<ConsensusRecord>,
        retired: Vec<Range<Vec<u8>>>,
    ) -> Result<impl FnOnce(&mut CertifiedWriteCache) -> Result<CertifiedWriteTiming> + Send + 'static> {
        self.check_commit_parent(&prepared)?;
        let db = Arc::clone(&self.db);
        let cells = self.cells.clone();
        let metrics = self.metrics.clone();
        let cell_nodes = if cfg!(feature = "jolteon-resident-node-publication") {
            self.cell_nodes.clone()
        } else {
            None
        };
        #[cfg(feature = "jolteon-resident-cell-publication")]
        let live_index = self.live_index.clone().map(|index| {
            index.retain_writer_status();
            (index, prepared.next.id(&self.network).unpack())
        });
        Ok(move |_cache: &mut CertifiedWriteCache| {
            let started = Instant::now();
            let measured = metrics.as_deref().map(|m| (m, Purpose::Finality));
            #[cfg(feature = "jolteon-prebuilt-state-write")]
            let cached = _cache.take(&prepared);
            #[cfg(feature = "jolteon-prebuilt-state-write")]
            let reused_state_build = cached.as_ref().map(|state| state.build);
            #[cfg(feature = "jolteon-prebuilt-state-write")]
            let prefix = cached.map(|state| (state.prefix, BTreeSet::new()));
            #[cfg(not(feature = "jolteon-prebuilt-state-write"))]
            let prefix = None;
            let (prefix, proposals) = match prefix {
                Some(parts) => parts,
                None => assemble_write_prefix(
                    &prepared.batch, Some((&cells, &prepared.live)), measured.is_some(),
                    _cache.take_buffer(),
                )?,
            };
            #[cfg(feature = "jolteon-reuse-write-buffer")]
            let reused_write_buffer = prefix.reused_buffer;
            let raw = persist_write(&db, prefix, proposals, &records, &retired, measured)?;
            #[cfg(feature = "jolteon-reuse-write-buffer")]
            _cache.recycle(raw);
            #[cfg(not(feature = "jolteon-reuse-write-buffer"))]
            drop(raw);
            let persistence = started.elapsed();
            let cell_node_publication = cell_nodes.as_ref().map(|index| {
                let started = Instant::now();
                // Readers of old node IDs still have their RocksDB snapshots.
                index.advance(&prepared.batch);
                started.elapsed()
            });
            #[cfg(feature = "jolteon-resident-cell-publication")]
            let live_cell_publication = live_index.map(|(mut index, next)| {
                let started = Instant::now();
                index.advance(next, &prepared.live, prepared.live_records_detached());
                started.elapsed()
            });
            Ok(CertifiedWriteTiming {
                persistence,
                cell_node_publication,
                #[cfg(feature = "jolteon-resident-cell-publication")]
                live_cell_publication,
                #[cfg(feature = "jolteon-prebuilt-state-write")]
                reused_state_build,
                #[cfg(feature = "jolteon-reuse-write-buffer")]
                reused_write_buffer,
            })
        })
    }

    pub(crate) fn install_durable_tip(&mut self, prepared: &Prepared, node_publication_done: bool) {
        #[cfg(feature = "native-shared-pinned-cell-cache")]
        self.pinned_cell_cache.advance(prepared, &self.network);
        if let Some(index) = &mut self.live_index {
            index.advance(
                prepared.next.id(&self.network).unpack(),
                &prepared.live,
                prepared.live_records_detached(),
            );
        }
        if !node_publication_done && let Some(index) = &self.cell_nodes {
            index.advance(&prepared.batch);
        }
        #[cfg(feature = "native-hot-cell-node-cache")]
        {
            let next = prepared.next.id(&self.network).unpack();
            if let Some(index) = &self.cell_nodes {
                index.advance_hot(&self.cells, &prepared.batch, self.cell_node_header, next);
            }
            self.cell_node_header = next;
        }
        self.history = prepared.history.clone();
        self.tip = prepared.next.clone();
        self.stake_inputs = Arc::clone(&prepared.stake_inputs);
        if !prepared.live.is_empty() {
            self.live_cache.invalidate(prepared.live.keys());
        }
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

fn proposal_entry_bytes(len: usize) -> u64 {
    (1 + prost::length_delimiter_len(len) + len) as u64
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

fn timed<T>(run: impl FnOnce() -> Result<T>) -> Result<(T, Duration)> {
    let started = Instant::now();
    let result = run()?;
    Ok((result, started.elapsed()))
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

#[cfg(not(feature = "native-compact-write-journal"))]
type JournalKey = Vec<u8>;
#[cfg(not(feature = "native-compact-write-journal"))]
type JournalIndex = HashMap<Vec<u8>, usize>;

type Write = (JournalKey, Option<Vec<u8>>);

#[derive(Default)]
struct Batch {
    writes: Vec<Write>,
    index: OnceLock<JournalIndex>,
    /// IDs of present Cell rows in OutPoint order, before sorting the tree edits.
    #[cfg(feature = "native-prepared-cell-record-keys")]
    created_cell_ids: Vec<Key>,
    #[cfg(feature = "native-verified-cell-journal")]
    cell_nodes: Option<(usize, stem_ledger::patricia::VerifiedChanges)>,
}

impl Batch {
    fn lookup(&self, key: &[u8]) -> Option<&Option<Vec<u8>>> {
        #[cfg(feature = "native-compact-write-journal")]
        let position = self
            .index
            .get_or_init(|| JournalIndex::build(&self.writes))
            .position(&self.writes, key);
        #[cfg(not(feature = "native-compact-write-journal"))]
        let position = self
            .index
            .get_or_init(|| {
                self.writes
                    .iter()
                    .enumerate()
                    .map(|(position, (key, _))| (key.clone(), position))
                    .collect()
            })
            .get(key)
            .copied();
        position.map(|position| &self.writes[position].1)
    }

    fn put(&mut self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) {
        self.put_owned(key, value.as_ref().to_vec());
    }

    fn put_owned(&mut self, key: impl AsRef<[u8]>, value: Vec<u8>) {
        self.writes.push((JournalKey::from(key.as_ref()), Some(value)));
    }

    fn delete(&mut self, key: impl AsRef<[u8]>) {
        self.writes.push((JournalKey::from(key.as_ref()), None));
    }
}

struct WritePrefix {
    raw: WriteBatch,
    bytes: Option<BatchBytes>,
    #[cfg(feature = "jolteon-reuse-write-buffer")]
    reused_buffer: bool,
}

fn sync_with_consensus<V: AsRef<[u8]>>(
    db: &DB,
    batch: &Batch,
    live: Option<(&Patricia, &PreparedCells)>,
    records: &[(Vec<u8>, V)],
    retired: &[Range<Vec<u8>>],
    measured: Option<(&StorageMetrics, Purpose)>,
) -> Result<()> {
    let (prefix, proposals) = assemble_write_prefix(batch, live, measured.is_some(), None)?;
    persist_write(db, prefix, proposals, records, retired, measured).map(drop)
}

fn assemble_write_prefix<'a>(
    batch: &'a Batch,
    live: Option<(&Patricia, &PreparedCells)>,
    measured: bool,
    recycled: Option<WriteBatch>,
) -> Result<(WritePrefix, BTreeSet<&'a [u8]>)> {
    #[cfg(feature = "jolteon-reuse-write-buffer")]
    let reused_buffer = recycled.is_some();
    let mut raw = recycled.unwrap_or_default();
    debug_assert!(raw.is_empty());
    let mut bytes = measured.then(|| BatchBytes::new(raw.size_in_bytes()));
    // Finality keeps created records first, in the original batch order.
    if let Some((_tree, changes)) = live {
        #[cfg(feature = "native-prepared-cell-record-keys")]
        let mut ids = batch.created_cell_ids.iter();
        let mut encoded = Vec::new();
        for (point, cell) in changes.iter() {
            // The Patricia edit already scheduled actual record deletions.
            if let Some(cell) = cell {
                #[cfg(not(feature = "native-prepared-cell-record-keys"))]
                let id = record::cell_key(_tree, point);
                #[cfg(feature = "native-prepared-cell-record-keys")]
                let id = *ids.next().context("missing prepared Cell record key")?;
                let key = state_key(STATE_RECORD, Namespace::Cells, &id);
                cell_write::put_record(&mut raw, &key, point, cell, &mut encoded)?;
                if let Some(bytes) = &mut bytes {
                    bytes.add(&key, raw.size_in_bytes());
                }
            }
        }
        #[cfg(feature = "native-prepared-cell-record-keys")]
        ensure!(ids.next().is_none(), "unused prepared Cell record keys");
    }
    let mut proposals = BTreeSet::new();
    #[cfg(feature = "native-verified-cell-journal")]
    let remaining = if let Some((position, journal)) = &batch.cell_nodes {
        let (prefix, tail) = batch.writes.split_at(*position);
        append_journal_rows(&mut raw, &mut bytes, prefix, &mut proposals)?;
        let mut buffer = [0; stem_stake::patricia::MAX_NODE_BYTES];
        for (id, node) in journal.iter() {
            let key = state_key(STATE_NODE, Namespace::Cells, id);
            let value = match node {
                Some(node) => Some(node_storage::compact_bytes(node.encode_into(&mut buffer))),
                None => None,
            };
            append_journal_row(&mut raw, &mut bytes, &key, value)?;
        }
        tail
    } else {
        batch.writes.as_slice()
    };
    #[cfg(not(feature = "native-verified-cell-journal"))]
    let remaining = batch.writes.as_slice();
    append_journal_rows(&mut raw, &mut bytes, remaining, &mut proposals)?;
    Ok((
        WritePrefix {
            raw,
            bytes,
            #[cfg(feature = "jolteon-reuse-write-buffer")]
            reused_buffer,
        },
        proposals,
    ))
}

fn persist_write<'a, V: AsRef<[u8]>>(
    db: &DB,
    prefix: WritePrefix,
    mut proposals: BTreeSet<&'a [u8]>,
    records: &'a [(Vec<u8>, V)],
    retired: &[Range<Vec<u8>>],
    measured: Option<(&StorageMetrics, Purpose)>,
) -> Result<WriteBatch> {
    let WritePrefix { mut raw, mut bytes, .. } = prefix;
    for (key, value) in records {
        let storage_key = index_key(b'j', key);
        // A prior synchronous write already made this body durable. Preserve
        // repeated-key ordering within this batch, and always write journal state.
        if key.starts_with(b"body")
            && proposals.insert(key.as_slice())
            && db.get(&storage_key)?.as_deref() == Some(value.as_ref())
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
    Ok(raw)
}

fn append_journal_rows<'a>(
    batch: &mut WriteBatch,
    measured: &mut Option<BatchBytes>,
    rows: &'a [Write],
    proposals: &mut BTreeSet<&'a [u8]>,
) -> Result<()> {
    for (key, value) in rows {
        let key: &[u8] = key.as_ref();
        if key.starts_with(b"jbody") {
            proposals.insert(&key[1..]);
        }
        append_journal_row(batch, measured, key, value.as_deref())?;
    }
    Ok(())
}

fn append_journal_row(
    batch: &mut WriteBatch,
    measured: &mut Option<BatchBytes>,
    key: &[u8],
    value: Option<&[u8]>,
) -> Result<()> {
    match value {
        Some(value) => batch.put(key, value)?,
        None => batch.delete(key)?,
    }
    if let Some(bytes) = measured {
        bytes.add(key, batch.size_in_bytes());
    }
    Ok(())
}

#[cfg(all(
    test,
    any(
        feature = "jolteon-admission-pipeline",
        feature = "jolteon-candidate-pipeline",
        feature = "jolteon-proposal-pipeline",
        feature = "native-commitment-pipeline",
        feature = "native-cell-subtree-parallel",
        feature = "native-parallel-cell-record-hashes",
        feature = "native-compact-prepared-cell-delta"
    )
))]
mod pipeline_store_tests {
    use super::*;

    fn assert_same_batch_writes(left: &Batch, right: &Batch) {
        assert_eq!(left.writes, right.writes);
        #[cfg(feature = "native-prepared-cell-record-keys")]
        assert_eq!(left.created_cell_ids, right.created_cell_ids);
        #[cfg(feature = "native-verified-cell-journal")]
        assert_eq!(left.cell_nodes, right.cell_nodes);
    }

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
                .build_proposal(
                    &[ancestor.clone()],
                    std::iter::empty::<Transaction>(),
                    1002,
                    1024,
                    &mut ProposalScan::default(),
                    false,
                )?
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
        assert!(
            old.build_proposal(
                &[], std::iter::empty::<Transaction>(), 1002, 1024, &mut ProposalScan::default(), false,
            )?
                .is_none()
        );
        let fresh = store.pinned_view();
        assert_eq!(
            fresh.with_snapshot(|read| Ok(read.tip().clone()))?.as_ref(),
            Some(store.tip())
        );
        Ok(())
    }

    #[cfg(feature = "native-shared-pinned-cell-cache")]
    #[test]
    fn pinned_cache_shares_one_header_and_keeps_overlapping_snapshots_isolated() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let points = [0, 1, 2].map(|index| OutPoint::new([61; 32].pack(), index));
        let cell = Cell::new(
            20_000_000_000,
            ckb_types::packed::Script::default(),
            vec![],
            bytes::Bytes::new(),
        )?;
        spec.development_cells.extend(
            points.iter().map(|point| (point.clone(), cell.clone())),
        );
        let mut store = Store::open(home.path(), &spec, Retention::Archive)?;
        let delete = |store: &Store, point: &OutPoint, time| {
            let mut overlay = store.overlay(None);
            overlay.live.insert(point.clone(), None);
            let context = HeaderContext::after(
                &spec.network,
                store.tip(),
                time,
                Tree::transactions(spec.network.clone(), &[])?.root(),
            )?;
            store.finish_block(context, overlay, &[], vec![], 0, 0, false)
        };

        let first = delete(&store, &points[0], 1001)?;
        let pin = store.pinned_view();
        pin.with_snapshot(|old| {
            assert!(old.live_cell(&points[0])?.is_some());
            store.pinned_view().with_snapshot(|peer| {
                assert!(Arc::ptr_eq(&old.live_cache, &peer.live_cache));
                assert!(peer.cached_live_cell(&points[0]).is_some());
                Ok(())
            })?.context("missing shared snapshot")?;

            store.commit(first)?;
            store.pinned_view().with_snapshot(|fresh| {
                assert!(!Arc::ptr_eq(&old.live_cache, &fresh.live_cache));
                assert!(fresh.live_cell(&points[0])?.is_none());
                assert!(fresh.live_cell(&points[1])?.is_some());
                assert!(fresh.live_cell(&points[2])?.is_some());
                Ok(())
            })?.context("missing fresh snapshot")?;
            assert!(old.live_cell(&points[0])?.is_some());
            Ok(())
        })?.context("missing original snapshot")?;

        let second = delete(&store, &points[1], 1002)?;
        store.commit(second)?;
        store.pinned_view().with_snapshot(|fresh| {
            assert!(fresh.cached_live_cell(&points[1]).is_none());
            assert!(fresh.live_cell(&points[1])?.is_none());
            assert!(fresh.cached_live_cell(&points[2]).is_some());
            Ok(())
        })?.context("missing advanced snapshot")?;
        let status = store.pinned_cell_cache_status().expect("enabled cache");
        assert_eq!(status.retained_height, Some(2));
        assert_eq!(status.shared_acquisitions, 2);
        assert_eq!(status.generations, 2);
        assert_eq!(status.carried_forward, 1);
        assert_eq!(status.entries, 1);
        Ok(())
    }

    #[cfg(feature = "jolteon-cross-block-pipeline")]
    #[test]
    fn resident_index_falls_back_past_its_available_headers() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let point = OutPoint::new([24; 32].pack(), 0);
        let cell = Cell::new(20_000_000_000, ckb_types::packed::Script::default(), vec![], bytes::Bytes::new())?;
        spec.development_cells.push((point.clone(), cell.clone()));
        let mut store = Store::open(home.path(), &spec, Retention::Archive)?;
        store.enable_live_cell_index(NonZeroUsize::new(2).unwrap())?;
        let pinned = store.pinned_view();
        let next = store.prepare(&[], 1001, false)?;
        pinned.with_snapshot(|view| {
            assert!(view.live_index.as_ref().unwrap().resident());
            store.commit(next)?;
            assert_eq!(
                view.live_index.as_ref().unwrap().resident(),
                cfg!(feature = "native-live-cell-parent-index")
            );
            let next = store.prepare(&[], 1002, false)?;
            store.commit(next)?;
            assert!(!view.live_index.as_ref().unwrap().resident());
            assert_eq!(view.live_cell(&point)?.context("old Cell missing")?.cell.as_slice(), cell.as_slice());
            Ok(())
        })?.context("old snapshot missing")?;
        let status = store.live_cell_index_status().unwrap();
        assert_eq!(status.fallbacks, 1);
        assert_eq!(status.entries, 1);
        Ok(())
    }

    #[cfg(all(feature = "jolteon-cross-block-pipeline", feature = "native-live-cell-parent-index"))]
    #[test]
    fn resident_parent_reads_spends_creations_and_absences_at_the_pinned_header() -> Result<()> {
        let mut spec = spec();
        let txid = [41; 32].pack();
        let spent = OutPoint::new(txid.clone(), 1);
        let created = OutPoint::new(txid.clone(), 257);
        let unchanged = OutPoint::new(txid.clone(), 513);
        let absent = OutPoint::new(txid, 769);
        let cell = Cell::new(
            20_000_000_000,
            ckb_types::packed::Script::default(),
            vec![],
            bytes::Bytes::new(),
        )?;
        spec.development_cells = vec![(spent.clone(), cell.clone()), (unchanged.clone(), cell.clone())];
        let points = [spent.clone(), created.clone(), unchanged.clone(), absent];
        let leaf_proofs = |view: &Store| -> Result<Vec<_>> {
            view.prove_cells(&points)?.into_iter().zip(&points).map(|((row, proof), point)| {
                let root = as_key(&view.tip.state.cell_root);
                assert!(record::verify_cell(&view.cells, &root, point, row.as_ref(), &proof));
                Ok((row.map(|row| record::encode_cell(point, &row)), proof))
            }).collect()
        };

        for layout in [LiveCellIndexLayout::Inline, LiveCellIndexLayout::InternedScripts] {
            let home = tempfile::tempdir()?;
            let mut store = Store::open(home.path(), &spec, Retention::Archive)?;
            store.enable_live_cell_index_with_layout(NonZeroUsize::new(2).unwrap(), layout)?;
            let before = leaf_proofs(&store)?;
            let pinned = store.pinned_view();
            let context = HeaderContext::after(
                &spec.network, store.tip(), 1001, Tree::transactions(spec.network.clone(), &[])?.root(),
            )?;
            let mut overlay = store.overlay(None);
            overlay.live.insert(spent.clone(), None);
            overlay.live.insert(created.clone(), Some(LiveCell {
                cell: cell.clone(),
                origin: Origin::new(&spec.network, &context),
            }));
            let next = Arc::new(store.finish_block(context, overlay, &[], vec![], 0, 0, false)?);
            let after = store.on_branch(std::slice::from_ref(&next), leaf_proofs)?;
            assert_eq!(after.iter().map(|(row, _)| row.is_some()).collect::<Vec<_>>(), [false, true, true, false]);

            pinned.with_snapshot(|view| {
                store.commit_certified(&next, &[], &[])?;
                assert_eq!(leaf_proofs(view)?, before);
                assert_eq!(leaf_proofs(&store)?, after);
                let status = store.live_cell_index_status().unwrap();
                assert_eq!(status.previous_header_lookups, points.len() as u64);
                assert!(status.previous_header_bytes > 0 && status.accounted_bytes <= status.limit_bytes);
                assert_eq!(status.fallbacks, 0);
                Ok(())
            })?.context("missing previous snapshot")?;

            // A reopened index comes entirely from the authenticated disk
            // image, with no before-images from the old process.
            drop(pinned);
            drop(store);
            let mut reopened = Store::open(home.path(), &spec, Retention::Archive)?;
            reopened.configure_live_cell_index(NonZeroUsize::new(2).unwrap(), layout, 2)?;
            assert_eq!(leaf_proofs(&reopened)?, after);
            assert_eq!(reopened.live_cell_index_status().unwrap().previous_header_bytes, 0);
        }
        Ok(())
    }

    #[cfg(all(feature = "jolteon-cross-block-pipeline", feature = "native-live-cell-parent-index"))]
    #[test]
    fn resident_parent_drops_its_before_image_to_keep_current_rows() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let source = OutPoint::new([42; 32].pack(), 0);
        let target = OutPoint::new([42; 32].pack(), 1);
        let make_cell = |data_bytes| Cell::new(
            20_000_000_000,
            ckb_types::packed::Script::default(),
            vec![],
            bytes::Bytes::from(vec![1; data_bytes]),
        );
        spec.development_cells.push((source.clone(), make_cell(8)?));
        let store = Store::open(home.path(), &spec, Retention::Archive)?;
        let sized = IndexView::recover(&store, 2 << 20, LiveCellIndexLayout::Inline, 2)?;
        let room = 16 << 10;
        let limit = sized.status().accounted_bytes + room;
        let mut index = IndexView::recover(&store, limit, LiveCellIndexLayout::Inline, 2)?;
        let old = index.clone();
        let new_cell = LiveCell {
            // Fits without the before-image, but its small remaining headroom
            // cannot retain two edit entries and the old Cell payload.
            cell: make_cell(room - 32)?,
            origin: Origin::new(&spec.network, &store.tip.context),
        };
        let mut changes = LiveChanges::new();
        changes.insert(source.clone(), None);
        changes.insert(target.clone(), Some(new_cell));
        index.advance([7; 32], &changes.into(), false);
        assert!(index.resident());
        assert!(matches!(index.lookup(&source)?, Some(None)));
        assert!(matches!(index.lookup(&target)?, Some(Some(_))));
        assert!(old.lookup(&source)?.is_none());
        assert_eq!(index.status().previous_header_bytes, 0);
        assert!(index.status().accounted_bytes <= limit);
        Ok(())
    }

    #[test]
    fn cell_node_index_recovers_checked_bytes_and_preserves_pinned_reads() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let point = OutPoint::new([37; 32].pack(), 0);
        spec.development_cells.push((
            point,
            Cell::new(20_000_000_000, ckb_types::packed::Script::default(), vec![], bytes::Bytes::new())?,
        ));
        let mut store = Store::open(home.path(), &spec, Retention::Archive)?;
        let root: Key = store.tip.state.cell_root.unpack();
        let key = state_key(STATE_NODE, Namespace::Cells, &root);
        let original = store.db.get(key)?.context("missing test root")?.to_vec();
        let mut corrupt = original.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        store.db.put(key, corrupt)?;
        let budget = NonZeroUsize::new(1).unwrap();
        assert!(store.enable_cell_node_index(budget).unwrap_err().to_string().contains("corrupt committed Cell node"));
        store.db.put(key, original)?;
        store.enable_cell_node_index(budget)?;
        let index = store.cell_nodes.as_ref().unwrap();
        let snapshot = store.db.snapshot();
        let mut options = ReadOptions::default();
        options.set_snapshot(&snapshot);
        let mut pinned = store.read_view(&[]);
        pinned.read_options = Arc::new(options);
        let query = record::cell_key(&pinned.cells, &OutPoint::new([37; 32].pack(), 0));
        let prove = || pinned.cells.edit(root, |id| pinned.node(Namespace::Cells, id)).prove(&query);
        let cached = prove()?;
        let mut retired = Batch::default();
        retired.delete(key);
        store.db.delete(key)?;
        index.advance(&retired);
        assert!(store.node(Namespace::Cells, &root)?.is_none());
        assert_eq!(cached, prove()?);
        let status = store.cell_node_index_status().unwrap();
        assert_eq!((status.hits, status.misses), (1, 2));
        Ok(())
    }

    #[test]
    fn cell_node_index_budget_preserves_authenticated_database_reads() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let cell = Cell::new(
            20_000_000_000,
            ckb_types::packed::Script::default(),
            vec![],
            bytes::Bytes::new(),
        )?;
        let txid = [38; 32].pack();
        for index in 0..4096 {
            spec.development_cells.push((OutPoint::new(txid.clone(), index), cell.clone()));
        }
        let mut store = Store::open(home.path(), &spec, Retention::Archive)?;
        store.enable_cell_node_index(NonZeroUsize::new(1).unwrap())?;
        let status = store.cell_node_index_status().unwrap();
        assert!(!status.active && status.accounted_bytes == 0 && status.nodes == 0);
        let point = OutPoint::new(txid, 4095);
        let (found, proof) = store.prove_cell(&point)?;
        let root: Key = store.tip().state.cell_root.unpack();
        assert!(record::verify_cell(&store.cells, &root, &point, found.as_ref(), &proof));
        assert_eq!(found.context("missing database-backed proof")?.cell.as_slice(), cell.as_slice());
        #[cfg(feature = "native-hot-cell-node-cache")]
        {
            let cold = store.cell_node_index_status().unwrap().hot_cache.unwrap();
            let (found, cached) = store.prove_cell(&point)?;
            assert_eq!(proof, cached);
            assert!(record::verify_cell(&store.cells, &root, &point, found.as_ref(), &cached));
            let status = store.cell_node_index_status().unwrap();
            let hot = status.hot_cache.unwrap();
            assert!(hot.rows > 0);
            assert!(hot.hits > cold.hits);
            assert!(hot.accounted_bytes <= status.limit_bytes);
        }
        Ok(())
    }

    #[test]
    fn resident_index_rejects_valid_records_outside_the_durable_commitment() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut store = Store::open(home.path(), &spec(), Retention::Archive)?;
        let point = OutPoint::new([25; 32].pack(), 0);
        let cell = LiveCell {
            cell: Cell::new(20_000_000_000, ckb_types::packed::Script::default(), vec![], bytes::Bytes::new())?,
            origin: Origin::new(&store.network, &store.tip.context),
        };
        store.db.put(store.live_cell_key(&point), record::encode_cell(&point, &cell))?;
        for workers in [1, 2] {
            let error = store
                .configure_live_cell_index(NonZeroUsize::new(2).unwrap(), LiveCellIndexLayout::Inline, workers)
                .unwrap_err();
            assert!(error.to_string().contains("authenticated Cell root"));
            assert!(store.live_cell_index_status().is_none());
        }
        Ok(())
    }

    #[cfg(feature = "jolteon-cancel-stale-execution")]
    #[test]
    fn abandoned_snapshot_work_has_no_prepared_result() -> Result<()> {
        let home = tempfile::tempdir()?;
        let store = Store::open(home.path(), &spec(), Retention::Archive)?;
        let tip = store.tip().clone();
        let cancellation = PreparationCancellation::default();
        let pinned = store.pinned_view().with_cancellation(cancellation.clone());
        let interrupted = pinned.with_snapshot(|view| {
            let prepared = view.prepare(&[], 1001, false)?;
            assert!(cancellation.cancel());
            Ok(prepared)
        });
        assert!(matches!(interrupted, Err(error) if error.is::<PreparationCancelled>()));
        assert!(!cancellation.cancel());

        let never_started = pinned.with_snapshot(|_| -> Result<()> {
            panic!("cancelled job should not open a snapshot");
        });
        assert!(matches!(never_started, Err(error) if error.is::<PreparationCancelled>()));
        assert_eq!(store.tip(), &tip);
        assert!(store.header(1)?.is_none());
        assert!(store.prepare(&[], 1001, false).is_ok());
        Ok(())
    }

    #[cfg(feature = "jolteon-cross-block-pipeline")]
    #[test]
    fn late_worker_can_open_only_the_expected_durable_prefix() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let point = OutPoint::new([39; 32].pack(), 0);
        spec.development_cells.push((
            point.clone(),
            Cell::new(
                20_000_000_000, ckb_types::packed::Script::default(), vec![], bytes::Bytes::new(),
            )?,
        ));
        let mut store = Store::open(home.path(), &spec, Retention::Recent(2))?;
        let pinned = store.pinned_view();
        assert!(pinned.with_snapshot(|view| view.live_cell(&point))?.flatten().is_some());
        let mut overlay = store.overlay(None);
        overlay.live.insert(point.clone(), None);
        let context = HeaderContext::after(
            &spec.network, store.tip(), 1001, Tree::transactions(spec.network.clone(), &[])?.root(),
        )?;
        let first = Arc::new(store.finish_block(context, overlay, &[], vec![], 0, 0, false)?);
        let fork = Arc::new(store.prepare(&[], 1002, false)?);
        let second = Arc::new(store.on_branch(std::slice::from_ref(&first), |view| {
            view.prepare(&[], 1003, false)
        })?);
        let branch = [first.clone(), second.clone()];
        let build = |branch: &[_], committing| {
            pinned.build_proposal(
                branch,
                std::iter::empty::<Transaction>(),
                1004,
                1024,
                &mut ProposalScan::default(),
                committing,
            )
        };
        let (_, before) = build(&branch, true)?.context("missing old snapshot result")?;
        store.commit_certified(&first, &[], &[])?;
        let exact_prefix = cfg!(feature = "jolteon-exact-prefix-pipeline");
        assert_eq!(build(&branch, false)?.is_some(), exact_prefix);
        assert!(build(&[fork.clone(), second.clone()], true)?.is_none());
        // The original pin previously cached this Cell at the old durable
        // tip. The rebased snapshot must see its committed deletion.
        let rebased_cell = pinned.with_branch_snapshot(&branch, true, |view, _| {
            view.live_cell(&point)
        })?;
        assert!(matches!(rebased_cell, Some(None)));

        let (_, late) = build(&branch, true)?.context("exact prefix should rebase")?;
        assert_eq!(before.header(), late.header());
        assert_eq!(before.receipts(), late.receipts());
        assert_same_batch_writes(&before.batch, &late.batch);
        let follower = pinned
            .prepare_candidate(&branch, &[], 1004, true)?
            .context("Follower should open the same exact prefix")?;
        assert_eq!(before.header(), follower.header());
        assert_same_batch_writes(&before.batch, &follower.batch);
        store.commit_certified(&second, &[], &[])?;
        let late = build(&branch, true)?;
        assert_eq!(late.is_some(), exact_prefix);
        if let Some((_, late)) = late {
            assert_eq!(before.header(), late.header());
            assert_eq!(before.receipts(), late.receipts());
            assert_same_batch_writes(&before.batch, &late.batch);
            let follower = pinned
                .prepare_candidate(&branch, &[], 1004, false)?
                .context("Follower should open both durable ancestors")?;
            assert_eq!(before.header(), follower.header());
            assert_same_batch_writes(&before.batch, &follower.batch);
        }
        assert!(build(&[fork, second.clone()], true)?.is_none());
        assert!(build(&[second], true)?.is_none());
        Ok(())
    }

    #[cfg(feature = "native-compact-prepared-cell-delta")]
    #[test]
    fn compact_prepared_branch_spends_mask_indexed_parent_creations() -> Result<()> {
        let home = tempfile::tempdir()?;
        let store = Store::open(home.path(), &spec(), Retention::Archive)?;
        let point = |index| OutPoint::new([94; 32].pack(), index);
        let cell = Cell::new(20_000_000_000, Default::default(), vec![], Default::default())?;
        let context = |view: &Store, time| {
            HeaderContext::after(
                view.network(),
                view.tip(),
                time,
                Tree::transactions(view.network().clone(), &[])?.root(),
            )
        };
        let first = context(&store, 1001)?;
        let origin = Origin::new(store.network(), &first);
        let mut overlay = store.overlay(None);
        for index in 0..256 {
            overlay.live.insert(point(index), Some(LiveCell {
                cell: cell.clone(),
                origin: origin.clone(),
            }));
        }
        let parent = Arc::new(store.finish_block(first, overlay, &[], vec![], 0, 0, false)?);
        let (spent, sibling) = store.on_branch(std::slice::from_ref(&parent), |view| {
            assert!(view.live_cell(&point(1))?.is_some());
            let mut overlay = view.overlay(None);
            overlay.live.insert(point(1), None);
            Ok((
                Arc::new(view.finish_block(context(view, 1002)?, overlay, &[], vec![], 0, 0, false)?),
                Arc::new(view.prepare(&[], 1003, false)?),
            ))
        })?;
        store.on_branch(&[Arc::clone(&parent), spent], |view| {
            assert!(view.live_cell(&point(1))?.is_none());
            assert!(view.live_cell(&point(2))?.is_some());
            Ok(())
        })?;
        store.on_branch(&[parent, sibling], |view| {
            assert!(view.live_cell(&point(1))?.is_some());
            Ok(())
        })?;
        assert!(store.live_cell(&point(1))?.is_none());
        Ok(())
    }

    #[cfg(feature = "jolteon-proposal-pipeline")]
    #[test]
    fn proposal_scan_distinguishes_spent_inputs_and_block_cycles() -> Result<()> {
        use ckb_crypto::secp::Privkey;
        use ckb_types::{
            bytes::Bytes,
            packed::{CellDep, Script},
        };
        use stem_ledger::{CellInput, GroupWitness};

        let path = tempfile::tempdir()?;
        let mut spec = spec();
        let program = test_contract::native_lock(Path::new(env!("CARGO_MANIFEST_DIR")))?;
        let key = Privkey::from_slice(&[18; 32]);
        let signer = ckb_hash::blake2b_256(key.pubkey()?.serialize());
        let lock = Script::new_builder()
            .code_hash(ckb_hash::blake2b_256(&program))
            .hash_type(ckb_types::core::ScriptHashType::Data)
            .args(Bytes::copy_from_slice(&signer[..20]))
            .build();
        let code_point = OutPoint::new([3; 32].pack(), 0);
        let bond = Cell::minimum_capacity(&lock, &[], program.len())?;
        let code = Cell::new(bond, lock.clone(), vec![], program)?;
        let reserve = Cell::new(20_000_000_000, lock.clone(), vec![], Bytes::new())?;
        let points = [4u8, 5, 6, 7, 8].map(|id| OutPoint::new([id; 32].pack(), 0));
        spec.development_cells = vec![
            (code_point.clone(), code.clone()),
            (points[0].clone(), reserve.clone()),
            (points[1].clone(), reserve.clone()),
            (points[3].clone(), reserve.clone()),
            (points[4].clone(), reserve.clone()),
        ];
        #[cfg(feature = "native-shared-cell-deps")]
        let group_point = {
            let point = OutPoint::new([2; 32].pack(), 0);
            let members = ckb_types::packed::OutPointVec::new_builder()
                .set(vec![code_point.clone()])
                .build();
            let group = Cell::new(reserve.capacity(), lock.clone(), vec![], members.as_bytes())?;
            spec.development_cells.push((point.clone(), group));
            point
        };
        let sign_for = |tx: Transaction, state: &Spec| -> Result<Transaction> {
            let digest = tx.signing_digest(
                &state.network,
                0,
                &lock,
                |wanted| {
                    Ok(state.development_cells.iter()
                        .find(|(point, _)| point == wanted)
                        .map_or_else(|| reserve.clone(), |(_, cell)| cell.clone()))
                },
                |_| anyhow::bail!("unexpected Header Dep"),
            )?;
            tx.with_witnesses(vec![GroupWitness {
                group_index: 0,
                proof_data: Bytes::new(),
                seal: Bytes::copy_from_slice(&key.sign_recoverable(&digest.unpack())?.serialize()),
            }])
        };
        let sign = |tx| sign_for(tx, &spec);
        let payment = |point: &OutPoint, dep: CellDep| {
            sign(Transaction::new(
                vec![dep],
                vec![],
                vec![CellInput::new(point.clone(), None)],
                vec![reserve.clone()],
                vec![],
            )?)
        };
        let signed = |point: &OutPoint| {
            payment(point, CellDep::new_builder().out_point(code_point.clone()).build())
        };
        let first = signed(&points[0])?;
        let second = signed(&points[1])?;
        let missing = signed(&points[2])?;
        let third = signed(&points[3])?;
        let fourth = signed(&points[4])?;
        let store = Store::open(path.path(), &spec, Retention::Archive)?;
        let parent = Arc::new(store.prepare(std::slice::from_ref(&first), 1001, false)?);
        let mut scan = ProposalScan { priority_candidates: 3, ..ProposalScan::default() };
        let workers = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
        #[cfg(feature = "native-rolling-leader-execution")]
        {
            let run = [first.clone(), second.clone(), third.clone(), fourth.clone()];
            let serial_pool = rayon::ThreadPoolBuilder::new().num_threads(1).build()?;
            let (expected, reference) =
                serial_pool.install(|| store.build_proposal(run.iter(), 1001, 16 * 1024))?;
            let mut scan = ProposalScan::default();
            let (selected, prepared) = workers.install(|| {
                store.build_proposal_measured(run.iter(), 1001, 16 * 1024, &mut scan, None)
            })?;
            assert_eq!(
                selected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                expected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
            );
            assert_eq!(prepared.header(), reference.header());
            assert_eq!(prepared.receipts(), reference.receipts());
            assert_same_batch_writes(&prepared.batch, &reference.batch);
            assert_eq!(scan.rolling_cells.jobs, 4);
            assert_eq!(scan.rolling_cells.peak_outstanding, 3);
            assert_eq!(scan.rolling_cells.refilled_jobs, 1);
            assert_eq!(
                scan.rolling_cells.projected_transactions,
                if cfg!(feature = "native-incremental-cell-projection") { 3 } else { 4 },
            );

            #[cfg(feature = "native-leader-tail-cycle-shares")]
            {
                let max = reference.receipts().iter().map(|receipt| receipt.cycles).max()
                    .expect("four payment receipts");
                let mut tail_spec = self::spec();
                tail_spec.development_cells = spec.development_cells.clone();
                tail_spec.limits.block_cycles = 4 * max;
                tail_spec.limits.transaction_cycles = tail_spec.limits.block_cycles;
                let tail_path = tempfile::tempdir()?;
                let tail_store = Store::open(tail_path.path(), &tail_spec, Retention::Archive)?;
                let (expected, reference) =
                    serial_pool.install(|| tail_store.build_proposal(run.iter(), 1001, 16 * 1024))?;
                let mut scan = ProposalScan::default();
                let (selected, leader) = workers.install(|| {
                    tail_store.build_proposal_measured(run.iter(), 1001, 16 * 1024, &mut scan, None)
                })?;
                assert_eq!(selected.len(), run.len());
                assert_eq!(
                    selected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                    expected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                );
                assert_eq!(leader.header(), reference.header());
                assert_eq!(leader.receipts(), reference.receipts());
                assert_same_batch_writes(&leader.batch, &reference.batch);
                assert!(scan.rolling_cells.tail_cycle_share_jobs >= 2);
                assert!(scan.rolling_cells.tail_cycle_share_reused >= 2);
            }

            #[cfg(feature = "native-compact-cell-trial-edits")]
            {
                // Three inputs and outputs spill both inline buffers.
                let wide = sign(Transaction::new(
                    vec![CellDep::new_builder().out_point(code_point.clone()).build()],
                    vec![],
                    [0, 1, 3].map(|index| CellInput::new(points[index].clone(), None)).to_vec(),
                    vec![reserve.clone(); 3],
                    vec![],
                )?)?;
                let wide_run = [wide, fourth.clone()];
                let (expected, reference) =
                    serial_pool.install(|| store.build_proposal(wide_run.iter(), 1001, 16 * 1024))?;
                let mut scan = ProposalScan::default();
                let (selected, leader) = workers.install(|| {
                    store.build_proposal_measured(wide_run.iter(), 1001, 16 * 1024, &mut scan, None)
                })?;
                assert_eq!(selected.len(), wide_run.len());
                assert_eq!(
                    selected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                    expected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                );
                assert_eq!(scan.rolling_cells.reused, 2);
                let follower = workers.install(|| store.prepare(&selected, 1001, true))?;
                for prepared in [leader, follower] {
                    assert_eq!(prepared.header(), reference.header());
                    assert_eq!(prepared.receipts(), reference.receipts());
                    assert_same_batch_writes(&prepared.batch, &reference.batch);
                }
            }

            // A stop consumes only the ordered entry that reported it.
            let block = BlockContext::after(
                &spec.network, store.tip(), 1001, Byte32::default(), spec.capacity,
            )?;
            let origin = Origin::new(&spec.network, block.candidate());
            let mut stats = CellStreamStats::default();
            let mut calls = 0;
            let outcome = workers.install(|| {
                store.drive_cell_run(
                    &run, &block, &origin, 2, &mut Budget::new(spec.limits),
                    &mut store.overlay(None), &mut stats, true,
                    #[cfg(feature = "native-shared-cell-deps")]
                    &mut None,
                    |tx, _, _, _, _| {
                        assert_eq!(tx.as_slice(), first.as_slice());
                        calls += 1;
                        Ok(ProposalStep::Stop)
                    },
                )
            })?;
            assert_eq!(outcome, (1, true));
            assert_eq!(calls, 1);
            assert_eq!((stats.jobs, stats.discarded_jobs), (3, 2));
            assert_eq!(
                stats.projected_transactions,
                if cfg!(feature = "native-incremental-cell-projection") { 2 } else { 4 },
            );

            #[cfg(feature = "native-reclaim-cell-projection")]
            {
                // Force the next submit to follow a fold at which the last
                // settled job had no successor and no published declaration.
                let mut budget = Budget::new(spec.limits);
                budget.cycles = 2 * spec.limits.transaction_cycles;
                let mut stats = CellStreamStats::default();
                let mut steps = 0;
                let outcome = workers.install(|| {
                    store.drive_cell_run(
                        &run, &block, &origin, 2, &mut budget,
                        &mut store.overlay(None), &mut stats, true,
                        #[cfg(feature = "native-shared-cell-deps")]
                        &mut None,
                        |_, _, budget, _, _| {
                            if steps == 0 {
                                budget.cycles -= 1;
                            }
                            steps += 1;
                            Ok(ProposalStep::Skipped)
                        },
                    )
                })?;
                assert_eq!(outcome, (4, false));
                assert_eq!(stats.jobs, 4);
                assert_eq!(stats.projected_transactions, 2);
                assert_eq!(stats.retired_projection_edits, 2);
            }
        }
        #[cfg(all(
            feature = "native-reclaim-cell-projection",
            feature = "native-rolling-leader-execution",
            feature = "native-rolling-cell-execution"
        ))]
        {
            let rejected = sign(Transaction::new(
                vec![CellDep::new_builder().out_point(code_point.clone()).build()],
                vec![],
                vec![
                    CellInput::new(points[0].clone(), None),
                    CellInput::new(points[2].clone(), None),
                ],
                vec![reserve.clone()],
                vec![],
            )?)?;
            let dependent = |tx: &Transaction| signed(&OutPoint::new(tx.id(&spec.network), 0));
            let child_a = dependent(&first)?;
            let child_b = dependent(&second)?;
            let child_c = dependent(&third)?;

            // A late worker consumes an output already transferred out of
            // the projection. It must still resolve the original Cell.
            let accepted = [
                first.clone(), second.clone(), third.clone(), fourth.clone(),
                child_b.clone(), child_c.clone(), dependent(&fourth)?, child_a.clone(),
            ];
            let serial = store.prepare(&accepted, 1001, false)?;
            let follower = workers.install(|| store.prepare(&accepted, 1001, true))?;
            assert_eq!(follower.header(), serial.header());
            assert_eq!(follower.receipts(), serial.receipts());
            assert_same_batch_writes(&follower.batch, &serial.batch);
            assert_eq!(follower.cell_stream().fallbacks, 0);
            assert!(follower.cell_stream().retired_projection_edits > 0);

            // The skipped candidate had declared a spend of the original
            // input. The later Leader can use it once that declaration retires.
            let run = [&rejected, &second, &third, &fourth, &child_b, &child_c, &first, &child_a];
            let expected = [&second, &third, &fourth, &child_b, &child_c, &first, &child_a];
            let mut scan = ProposalScan::default();
            let (selected, leader) = workers.install(|| {
                store.build_proposal_measured(run, 1001, 16 * 1024, &mut scan, None)
            })?;
            assert_eq!(
                selected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                expected.iter().map(|tx| tx.as_slice()).collect::<Vec<_>>(),
            );
            let serial = store.prepare(&selected, 1001, false)?;
            assert_eq!(leader.header(), serial.header());
            assert_eq!(leader.receipts(), serial.receipts());
            assert_same_batch_writes(&leader.batch, &serial.batch);
            assert_eq!(scan.rolling_cells.fallbacks, 1);
            assert!(scan.rolling_cells.retired_projection_edits > 0);
        }
        #[cfg(all(
            feature = "native-reuse-ordered-cell-writes",
            feature = "native-rolling-leader-execution"
        ))]
        {
            // The unavailable second input invalidates the first candidate,
            // which had projected a spend of the replacement's input.
            let rejected = sign(Transaction::new(
                vec![CellDep::new_builder().out_point(code_point.clone()).build()],
                vec![],
                vec![
                    CellInput::new(points[0].clone(), None),
                    CellInput::new(points[2].clone(), None),
                ],
                vec![reserve.clone()],
                vec![],
            )?)?;
            let child = signed(&OutPoint::new(first.id(&spec.network), 0))?;
            let run = [&rejected, &first, &child];
            let mut scan = ProposalScan::default();
            let (selected, proposal) = workers.install(|| {
                store.build_proposal_measured(run, 1001, 16 * 1024, &mut scan, None)
            })?;
            let expected = [first.clone(), child];
            assert_eq!(
                selected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                expected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
            );
            let serial = store.prepare(&expected, 1001, false)?;
            assert_eq!(proposal.header(), serial.header());
            assert_eq!(proposal.receipts(), serial.receipts());
            assert_same_batch_writes(&proposal.batch, &serial.batch);
            let stats = scan.rolling_cells;
            #[cfg(not(feature = "native-mask-rejected-cell-projections"))]
            assert_eq!(
                (stats.jobs, stats.fallbacks, stats.reused, stats.ordered_declarations),
                (3, 2, 1, 1)
            );
            #[cfg(feature = "native-mask-rejected-cell-projections")]
            {
                assert_eq!((stats.jobs, stats.masked_projection_positions), (3, 1));
                // The replacement may start on either side of rejection.
                assert!((1..=2).contains(&stats.fallbacks));
                assert_eq!(stats.reused + stats.fallbacks, 3);
                assert_eq!(stats.ordered_declarations + 1, stats.fallbacks);
            }
        }
        #[cfg(all(
            feature = "native-reuse-ordered-cell-writes",
            feature = "native-rolling-cell-execution"
        ))]
        {
            // The producer exceeds only the per-trial speculative read cap.
            // The ordinary ordered budget can accept its result.
            let large_path = tempfile::tempdir()?;
            let mut large_spec = self::spec();
            large_spec.development_cells = spec.development_cells.clone();
            let data_cell = Cell::new(
                1_000_000_000_000_000,
                lock.clone(),
                vec![],
                Bytes::from(vec![0; 1536 * 1024]),
            )?;
            let mut deps = vec![CellDep::new_builder().out_point(code_point.clone()).build()];
            for index in 0..3 {
                let point = OutPoint::new([46; 32].pack(), index);
                deps.push(CellDep::new_builder().out_point(point.clone()).build());
                large_spec.development_cells.push((point, data_cell.clone()));
            }
            let producer = sign_for(
                Transaction::new(
                    deps,
                    vec![],
                    vec![CellInput::new(points[0].clone(), None)],
                    vec![reserve.clone()],
                    vec![],
                )?,
                &large_spec,
            )?;
            let child = signed(&OutPoint::new(producer.id(&large_spec.network), 0))?;
            let candidate = [producer, child, third.clone(), fourth.clone()];
            let store = Store::open(large_path.path(), &large_spec, Retention::Archive)?;
            let serial = store.prepare(&candidate, 1001, false)?;
            let prepared = workers.install(|| store.prepare(&candidate, 1001, true))?;
            assert_eq!(prepared.header(), serial.header());
            assert_eq!(prepared.receipts(), serial.receipts());
            assert_same_batch_writes(&prepared.batch, &serial.batch);
            let stats = prepared.cell_stream();
            assert_eq!((stats.jobs, stats.fallbacks, stats.reused, stats.ordered_declarations), (4, 1, 3, 1));
        }
        #[cfg(feature = "native-rolling-cell-execution")]
        {
            let run = [first.clone(), second.clone(), third.clone(), fourth.clone()];
            let serial = store.prepare(&run, 1001, false)?;
            let pipelined = workers.install(|| store.prepare(&run, 1001, true))?;
            assert_eq!(pipelined.header(), serial.header());
            assert_eq!(pipelined.receipts(), serial.receipts());
            assert_same_batch_writes(&pipelined.batch, &serial.batch);
            let stats = pipelined.cell_stream();
            assert_eq!((stats.runs, stats.jobs, stats.reused, stats.fallbacks), (1, 4, 4, 0));
            assert_eq!(stats.peak_outstanding, 3);
            assert_eq!(stats.refilled_jobs, 1);
            assert_eq!(
                stats.projected_transactions,
                if cfg!(feature = "native-incremental-cell-projection") { 3 } else { 4 },
            );
            #[cfg(feature = "native-shared-cell-deps")]
            assert_eq!((stats.dependency_reads.cached, stats.dependency_reads.hits), (1, 4));
        }
        #[cfg(all(feature = "native-shared-cell-deps", feature = "native-dependency-execution"))]
        {
            use ckb_types::core::DepType;

            let group = CellDep::new_builder()
                .out_point(group_point.clone())
                .dep_type(DepType::DepGroup)
                .build();
            let payments = [0, 1, 3, 4].map(|i| payment(&points[i], group.clone()));
            let payments = payments.into_iter().collect::<Result<Vec<_>>>()?;
            let serial = store.prepare(&payments, 1001, false)?;
            let parallel = workers.install(|| store.prepare(&payments, 1001, true))?;
            assert_eq!(serial.header(), parallel.header());
            assert_eq!(serial.receipts(), parallel.receipts());
            assert_same_batch_writes(&serial.batch, &parallel.batch);
            let reads = parallel.cell_stream().dependency_reads;
            assert_eq!((reads.planned, reads.cached, reads.hits), (2, 2, 8));

            #[cfg(all(
                feature = "native-rolling-dependency-hints",
                feature = "native-rolling-cell-execution",
                feature = "native-rolling-leader-execution"
            ))]
            {
                let count = 68;
                let path = tempfile::tempdir()?;
                let mut mixed_spec = self::spec();
                mixed_spec.development_cells = spec.development_cells.clone();
                mixed_spec.limits.transaction_count = count;
                mixed_spec.limits.block_cycles = u64::from(count) * mixed_spec.limits.transaction_cycles;
                mixed_spec.limits.cell_reads = u64::from(count) * 4;
                mixed_spec.development_cells.extend((0..count).map(|index| {
                    (OutPoint::new([50; 32].pack(), index), reserve.clone())
                }));
                let code_dep = CellDep::new_builder().out_point(code_point.clone()).build();
                let mixed = (0..count).map(|index| {
                    let dep = if index < 64 { &code_dep } else { &group };
                    sign_for(Transaction::new(
                        vec![dep.clone()],
                        vec![],
                        vec![CellInput::new(OutPoint::new([50; 32].pack(), index), None)],
                        vec![reserve.clone()],
                        vec![],
                    )?, &mixed_spec)
                }).collect::<Result<Vec<_>>>()?;
                let store = Store::open(path.path(), &mixed_spec, Retention::Archive)?;
                let reference = store.prepare(&mixed, 1001, false)?;
                let follower = workers.install(|| store.prepare(&mixed, 1001, true))?;
                let mut scan = ProposalScan::default();
                let (selected, leader) = workers.install(|| {
                    store.build_proposal_measured(mixed.iter(), 1001, 128 * 1024, &mut scan, None)
                })?;
                assert_eq!(
                    selected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                    mixed.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                );
                for reads in [follower.cell_stream().dependency_reads, scan.dependency_reads] {
                    assert_eq!((reads.rolling_samples, reads.rolling_rebuilds, reads.rolling_carried), (1, 1, 1));
                    assert_eq!(reads.hits, 72);
                }
                for prepared in [leader, follower] {
                    assert_eq!(prepared.header(), reference.header());
                    assert_eq!(prepared.receipts(), reference.receipts());
                    assert_same_batch_writes(&prepared.batch, &reference.batch);
                }
            }

            let spend_group = signed(&group_point)?;
            let mut conflicting = vec![spend_group.clone()];
            conflicting.extend(payments.iter().cloned());
            assert!(
                workers.install(|| store.prepare(&conflicting, 1001, true))
                    .err().is_some_and(|err| err.is::<MissingLiveCell>())
            );
            let parent = Arc::new(store.prepare(std::slice::from_ref(&spend_group), 1001, false)?);
            store.on_branch(std::slice::from_ref(&parent), |view| {
                assert!(
                    workers.install(|| view.prepare(&payments, 1002, true))
                        .err().is_some_and(|err| err.is::<MissingLiveCell>())
                );
                Ok(())
            })?;
            #[cfg(feature = "native-leader-dependency-execution")]
            {
                let mut scan = ProposalScan::default();
                let (selected, proposed) = workers.install(|| store.build_proposal_measured(
                    conflicting.iter(), 1001, 16 * 1024, &mut scan, None,
                ))?;
                assert_eq!(selected.len(), 1);
                assert_eq!(selected[0].as_slice(), spend_group.as_slice());
                assert_eq!(proposed.header(), parent.header());
                assert_eq!(proposed.receipts(), parent.receipts());
                assert_same_batch_writes(&proposed.batch, &parent.batch);
                assert_eq!(scan.dependency_reads.cached, 2);
            }
        }
        let candidates = [first.clone(), missing, second.clone(), second.clone()];
        let (selected, proposal) = workers
            .install(|| store.pinned_view().build_proposal(
                std::slice::from_ref(&parent),
                candidates.iter(),
                1002,
                4096,
                &mut scan,
                false,
            ))?
            .context("missing proposal snapshot")?;
        let expected = store.on_branch(&[parent], |view| {
            view.prepare(std::slice::from_ref(&second), 1002, false)
        })?;
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].as_slice(), second.as_slice());
        assert_eq!(proposal.header(), expected.header());
        assert_eq!(proposal.receipts(), expected.receipts());
        assert_eq!(
            (scan.inspected, scan.selected, scan.spent_in_parent, scan.spent_in_proposal),
            (4, 1, 1, 1)
        );
        #[cfg(feature = "native-leader-dependency-execution")]
        assert_eq!(
            (scan.parallel_cell_jobs, scan.parallel_cell_reused, scan.parallel_cell_fallback),
            (3, 1, if cfg!(feature = "native-reuse-parent-spend-rejections") { 1 } else { 2 })
        );
        #[cfg(feature = "native-reuse-parent-spend-rejections")]
        assert_eq!(scan.parallel_parent_rejections_reused, 1);
        #[cfg(feature = "jolteon-aged-proposal-selection")]
        assert_eq!(
            (scan.priority_inspected, scan.priority_selected, scan.priority_spent_in_parent, scan.priority_spent_in_proposal),
            (3, 1, 1, 0)
        );

        #[cfg(feature = "native-reuse-parent-spend-rejections")]
        {
            // The same signed transaction is eligible on a sibling where its
            // explicit input is still live.
            let sibling = Arc::new(store.prepare(&[], 1001, false)?);
            let run = [first.clone(), second.clone()];
            let mut scan = ProposalScan::default();
            store.on_branch(&[sibling], |view| {
                let (selected, built) = workers.install(|| {
                    view.build_proposal_measured(run.iter(), 1002, 4096, &mut scan, None)
                })?;
                let reference = view.prepare(&run, 1002, false)?;
                assert_eq!(
                    selected.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                    run.iter().map(Transaction::as_slice).collect::<Vec<_>>(),
                );
                assert_eq!(built.header(), reference.header());
                assert_eq!(built.receipts(), reference.receipts());
                assert_same_batch_writes(&built.batch, &reference.batch);
                assert_eq!(scan.parallel_parent_rejections_reused, 0);
                Ok(())
            })?;
        }

        spec.limits.transaction_cycles = expected.receipts()[0].cycles + 1;
        spec.limits.block_cycles = spec.limits.transaction_cycles;
        let cold_path = tempfile::tempdir()?;
        let mut limited = Store::open(cold_path.path(), &spec, Retention::Archive)?;
        limited.check(&second)?;
        let mut scan = ProposalScan { priority_candidates: 2, ..ProposalScan::default() };
        let (selected, proposal) = limited.build_proposal_measured(
            [&first, &second, &third, &fourth],
            1001,
            4096,
            &mut scan,
            Some(2),
        )?;
        assert_eq!(
            (scan.inspected, scan.selected, scan.block_cycle_rejections, scan.cycle_scan_stops),
            (3, 1, 2, 1)
        );
        #[cfg(feature = "jolteon-aged-proposal-selection")]
        assert_eq!((scan.priority_inspected, scan.priority_selected, scan.priority_cycle_rejections), (2, 1, 1));
        assert_eq!(selected[0].as_slice(), first.as_slice());
        let reference = limited.prepare(std::slice::from_ref(&first), 1001, false)?;
        assert_eq!(proposal.header(), reference.header());
        assert_eq!(proposal.receipts(), reference.receipts());
        #[cfg(feature = "native-leader-tail-cycle-shares")]
        {
            // A worker receives at most half of the available cycles on the
            // first payment. The ordered retry can still use the full balance.
            let mut tail_scan = ProposalScan { priority_candidates: 2, ..ProposalScan::default() };
            let (selected, parallel) = workers.install(|| {
                limited.build_proposal_measured(
                    [&first, &second, &third, &fourth],
                    1001,
                    4096,
                    &mut tail_scan,
                    Some(2),
                )
            })?;
            assert_eq!(selected.len(), 1);
            assert_eq!(selected[0].as_slice(), first.as_slice());
            assert_eq!(parallel.header(), proposal.header());
            assert_eq!(parallel.receipts(), proposal.receipts());
            assert_same_batch_writes(&parallel.batch, &proposal.batch);
            assert_eq!(
                (tail_scan.inspected, tail_scan.block_cycle_rejections, tail_scan.cycle_scan_stops),
                (3, 2, 1),
            );
            assert!(tail_scan.rolling_cells.tail_cycle_share_jobs >= 2);
            assert_eq!(tail_scan.rolling_cells.tail_cycle_share_reused, 0);
        }
        limited.commit(proposal)?;
        assert!(limited.prepare(std::slice::from_ref(&fourth), 1002, false).is_ok());
        Ok(())
    }

    #[cfg(feature = "native-batched-cell-proof-records")]
    #[test]
    fn grouped_proof_records_check_absence_and_preserve_duplicate_positions() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let cell = Cell::new(
            20_000_000_000,
            ckb_types::packed::Script::default(),
            vec![],
            bytes::Bytes::new(),
        )?;
        let point = |index| OutPoint::new([51; 32].pack(), index);
        spec.development_cells.push((point(1), cell.clone()));
        let store = Store::open(home.path(), &spec, Retention::Archive)?;
        let points = [point(1), point(2), point(3), point(1), point(4), point(5)];
        let proved = store.prove_cells(&points)?;
        assert_eq!(
            proved.iter().map(|(cell, _)| cell.is_some()).collect::<Vec<_>>(),
            [true, false, false, true, false, false],
        );
        assert_eq!(proved[0].1, proved[3].1);
        for (point, (cell, proof)) in points.iter().zip(proved) {
            assert!(record::verify_cell(
                &store.cells, &as_key(&store.tip.state.cell_root), point, cell.as_ref(), &proof,
            ));
        }

        // Absence proofs still check the record store. None of these four
        // absent points is retained in the decoded cache.
        let absent = &points[1];
        let orphan = LiveCell { cell, origin: Origin::new(&store.network, &store.tip.context) };
        store.db.put(store.live_cell_key(absent), record::encode_cell(absent, &orphan))?;
        let error = store.prove_cells(&points).err().context("accepted an orphan record")?;
        assert!(error.to_string().contains("differs from its authenticated value"));
        assert!(store.cached_live_cell(absent).is_none());
        Ok(())
    }

    #[cfg(feature = "jolteon-candidate-pipeline")]
    #[test]
    fn shared_prefix_proofs_reduce_rocksdb_snapshot_reads() -> Result<()> {
        use bytes::Bytes;
        use ckb_types::packed::Script;

        const CELLS: u32 = 16_384;
        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let lock = Script::default();
        let capacity = Cell::minimum_capacity(&lock, &[], 0)? + 1_000_000_000;
        let cell = Cell::new(capacity, lock, vec![], Bytes::new())?;
        let point = |index| OutPoint::new([44; 32].pack(), index);
        spec.development_cells = (0..CELLS).map(|index| (point(index), cell.clone())).collect();
        let mut store = Store::open(home.path(), &spec, Retention::Archive)?;
        store.db.flush()?;
        let points: Vec<_> = (0..1024)
            .rev()
            .map(|index| {
                if index % 4 == 0 {
                    point(CELLS + index)
                } else {
                    point((index * 17) % 8192)
                }
            })
            .collect();
        let pinned = store.pinned_view();
        let first = points[0].clone();
        let root = Tree::transactions(store.network().clone(), &[])?.root();
        let context = HeaderContext::after(store.network(), store.tip(), 1001, root)?;
        let mut overlay = store.overlay(None);
        overlay.live.insert(first.clone(), None);
        let next = store.finish_block(context, overlay, &[], Vec::new(), 0, 0, false)?;
        pinned.with_snapshot(|view| {
            let root = as_key(&view.tip.state.cell_root);
            let keys: Vec<_> = points
                .iter()
                .map(|point| record::cell_key(&view.cells, point))
                .collect();
            let measure = |batch| -> Result<_> {
                let mut reads = 0;
                let mut tree = view.cells.edit(root, |id| {
                    reads += 1;
                    view.node(Namespace::Cells, id)
                });
                let proofs = if batch {
                    tree.prove_many(&keys)?
                } else {
                    keys.iter()
                        .map(|key| tree.prove(key))
                        .collect::<Result<Vec<_>>>()?
                };
                Ok((proofs, reads))
            };
            let (serial, reads) = measure(false)?;
            let (shared, batch_reads) = measure(true)?;
            assert_eq!(serial, shared);
            assert!(batch_reads < reads);
            store.commit(next)?;
            for ((key, (value, expected)), (point, (cell, actual))) in keys.iter()
                .zip(serial)
                .zip(points.iter().zip(view.prove_cells(&points)?))
            {
                assert_eq!(actual.encode()?, expected.encode()?);
                assert_eq!(
                    cell.as_ref().map(|cell| record::cell_value(&view.cells, point, cell)),
                    value
                );
                assert!(view.cells.verify(&root, key, value.as_ref(), &actual));
            }
            println!("RocksDB snapshot node reads for 1024 proofs: {reads} single, {batch_reads} batched");
            Ok(())
        })?.context("missing Cell proof snapshot")?;
        assert!(store.prove_cells(&[first])?[0].0.is_none());
        Ok(())
    }

    #[cfg(feature = "jolteon-commit-pipeline")]
    #[test]
    fn certified_write_moves_the_durable_header_and_journal_together() -> Result<()> {
        let home = tempfile::tempdir()?;
        let spec = spec();
        let mut store = Store::open(home.path(), &spec, Retention::Archive)?;
        let pinned = store.pinned_view();
        let prepared = Arc::new(store.prepare(&[], 1001, false)?);
        let records = vec![
            (b"state".to_vec(), b"next journal".to_vec().into()),
            (b"body/test".to_vec(), b"canonical proposal".to_vec().into()),
        ];
        let write = store.certified_write(Arc::clone(&prepared), records.clone(), vec![])?;
        assert!(store.header(1)?.is_none());
        assert!(store.consensus_record(b"state")?.is_none());
        #[cfg(feature = "jolteon-prebuilt-state-write")]
        let prebuild = store.prebuild_certified_state(Arc::clone(&prepared)).unwrap();
        let snapshot = pinned.with_snapshot(|view| {
            let timing = std::thread::spawn(move || {
                let mut cache = CertifiedWriteCache::default();
                #[cfg(feature = "jolteon-prebuilt-state-write")]
                prebuild(&mut cache);
                write(&mut cache)
            })
                .join()
                .map_err(|_| anyhow::anyhow!("certified write panicked"))??;
            #[cfg(feature = "jolteon-prebuilt-state-write")]
            assert!(timing.reused_state_build.is_some());
            #[cfg(not(feature = "jolteon-prebuilt-state-write"))]
            let _ = timing;
            assert!(view.header(1)?.is_none());
            assert!(view.consensus_record(b"state")?.is_none());
            assert!(view.consensus_record(b"body/test")?.is_none());
            Ok(view.tip().clone())
        })?;
        assert_eq!(snapshot.as_ref(), Some(store.tip()));
        store.install_durable_tip(&prepared, false);
        assert_eq!(store.tip(), prepared.header());
        assert!(store.certified_write(prepared, Vec::new(), Vec::new()).is_err());
        assert_eq!(
            store.consensus_record(b"body/test")?.as_deref(),
            Some(b"canonical proposal".as_slice()),
        );
        drop(store);
        drop(pinned);
        let reopened = Store::open(home.path(), &spec, Retention::Archive)?;
        assert_eq!(reopened.tip().context.height, 1);
        assert_eq!(reopened.consensus_record(b"state")?.unwrap(), records[0].1);
        Ok(())
    }

    #[cfg(feature = "native-pinned-index-try-read")]
    #[test]
    fn pinned_live_cell_readers_progress_while_the_index_is_locked() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let spent = OutPoint::new([53; 32].pack(), 0);
        let created = OutPoint::new([53; 32].pack(), 1);
        let cell = Cell::new(
            20_000_000_000, ckb_types::packed::Script::default(), vec![], bytes::Bytes::new(),
        )?;
        spec.development_cells = vec![(spent.clone(), cell.clone())];
        let mut store = Store::open(home.path(), &spec, Retention::Archive)?;
        store.enable_live_cell_index(NonZeroUsize::new(2).unwrap())?;
        let pinned = store.pinned_view();
        let context = HeaderContext::after(
            &spec.network, store.tip(), 1001, Tree::transactions(spec.network.clone(), &[])?.root(),
        )?;
        let mut overlay = store.overlay(None);
        overlay.live.insert(spent.clone(), None);
        overlay.live.insert(created.clone(), Some(LiveCell {
            cell, origin: Origin::new(&spec.network, &context),
        }));
        let prepared = Arc::new(store.finish_block(context, overlay, &[], vec![], 0, 0, false)?);
        pinned.with_snapshot(|old| {
            store.commit_certified(&prepared, &[], &[])?;
            let rebased = pinned.with_branch_snapshot(std::slice::from_ref(&prepared), true, |new, rest| {
                assert!(rest.is_empty());
                assert_eq!(new.tip(), prepared.header());
                let index = store.live_index.as_ref().expect("resident index");
                let old_spent = &spent;
                let new_cell = &created;
                let has_cell = |view: &Store, point: &OutPoint| {
                    view.live_cell(point).map(|record| record.is_some())
                };
                let rows = std::thread::scope(|scope| {
                    let (done, receive) = std::sync::mpsc::channel();
                    index.while_publishing_for_test(|| {
                        scope.spawn(move || {
                            let result = (|| {
                                assert!(!old.live_index.as_ref().unwrap().resident());
                                assert!(!new.live_index.as_ref().unwrap().resident());
                                Ok::<_, anyhow::Error>([
                                    has_cell(old, old_spent)?,
                                    has_cell(old, new_cell)?,
                                    has_cell(new, old_spent)?,
                                    has_cell(new, new_cell)?,
                                ])
                            })();
                            let _ = done.send(result);
                        });
                        receive.recv_timeout(Duration::from_secs(5))
                    })
                }).context("snapshot read waited on Live Cell publication")??;
                assert_eq!(rows, [true, false, false, true]);
                Ok(())
            })?;
            assert!(rebased.is_some());
            Ok(())
        })?.context("missing original snapshot")?;
        assert_eq!(store.live_cell_index_status().unwrap().snapshot_contention_fallbacks, 4);
        Ok(())
    }

    #[cfg(feature = "jolteon-resident-cell-publication")]
    #[test]
    fn certified_writer_publishes_live_cells_before_installing_their_header() -> Result<()> {
        let mut spec = spec();
        let point = |index| OutPoint::new([52; 32].pack(), index);
        let spent = point(0);
        let created = [point(2), point(3)];
        let cell = Cell::new(
            20_000_000_000, ckb_types::packed::Script::default(), vec![], bytes::Bytes::new(),
        )?;
        spec.development_cells = vec![(spent.clone(), cell.clone()), (point(1), cell.clone())];

        for layout in [LiveCellIndexLayout::Inline, LiveCellIndexLayout::InternedScripts] {
            let home = tempfile::tempdir()?;
            let mut store = Store::open(home.path(), &spec, Retention::Archive)?;
            store.enable_live_cell_index_with_layout(NonZeroUsize::new(2).unwrap(), layout)?;
            let pinned = store.pinned_view();
            let context = HeaderContext::after(
                &spec.network, store.tip(), 1001, Tree::transactions(spec.network.clone(), &[])?.root(),
            )?;
            let mut overlay = store.overlay(None);
            overlay.live.insert(spent.clone(), None);
            for point in &created {
                overlay.live.insert(point.clone(), Some(LiveCell {
                    cell: cell.clone(),
                    origin: Origin::new(&spec.network, &context),
                }));
            }
            let prepared = Arc::new(store.finish_block(context, overlay, &[], vec![], 0, 0, false)?);
            #[cfg(feature = "native-prepared-live-cell-ownership")]
            {
                assert_eq!(
                    prepared.live_records_detached(),
                    matches!(layout, LiveCellIndexLayout::Inline),
                );
                if prepared.live_records_detached() {
                    let owned = prepared.live.get(&created[0]).unwrap().as_ref().unwrap();
                    assert_eq!(owned.cell.as_slice(), cell.as_slice());
                    assert_ne!(owned.cell.as_slice().as_ptr(), cell.as_slice().as_ptr());
                }
            }
            let write = store.certified_write(Arc::clone(&prepared), Vec::new(), Vec::new())?;
            pinned.with_snapshot(|old| {
                let timing = std::thread::spawn(move || write(&mut CertifiedWriteCache::default()))
                    .join()
                    .map_err(|_| anyhow::anyhow!("certified writer panicked"))??;
                assert!(timing.live_cell_publication.is_some());
                assert_eq!(store.tip(), old.tip());
                assert_eq!(store.try_live_cell_index_status().unwrap().entries, 2);
                assert!(old.live_cell(&spent)?.is_some());
                assert!(old.live_cell(&created[0])?.is_none());
                store.install_durable_tip(&prepared, timing.cell_node_publication.is_some());
                assert_eq!(store.try_live_cell_index_status().unwrap().entries, 3);
                assert!(store.live_cell_index_status().unwrap().active);
                assert!(old.live_cell(&spent)?.is_some());
                assert!(store.live_cell(&spent)?.is_none());
                let published = store.live_cell(&created[0])?.context("published Cell missing")?;
                assert_eq!(published.cell.as_slice(), cell.as_slice());
                #[cfg(feature = "native-prepared-live-cell-ownership")]
                if prepared.live_records_detached() {
                    let owned = prepared.live.get(&created[0]).unwrap().as_ref().unwrap();
                    assert_eq!(published.cell.as_slice().as_ptr(), owned.cell.as_slice().as_ptr());
                }
                Ok(())
            })?.context("old snapshot unavailable")?;

            drop(pinned);
            drop(store);
            let mut reopened = Store::open(home.path(), &spec, Retention::Archive)?;
            reopened.enable_live_cell_index_with_layout(NonZeroUsize::new(2).unwrap(), layout)?;
            assert_eq!(reopened.tip(), prepared.header());
            assert_eq!(reopened.live_cell_index_status().unwrap().entries, 3);
        }
        Ok(())
    }

    #[cfg(feature = "jolteon-prebuilt-state-write")]
    #[test]
    fn unselected_state_prefix_cannot_be_persisted_by_another_candidate() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut store = Store::open(home.path(), &spec(), Retention::Archive)?;
        let speculative = Arc::new(store.prepare(&[], 1001, false)?);
        let certified = Arc::new(store.prepare(&[], 1002, false)?);
        let mut cache = CertifiedWriteCache::default();
        store.prebuild_certified_state(Arc::clone(&speculative)).unwrap()(&mut cache);
        assert!(cache.state.is_some());
        assert!(store.header(1)?.is_none());

        let write = store.certified_write(Arc::clone(&certified), Vec::new(), Vec::new())?;
        let timing = write(&mut cache)?;
        assert!(timing.reused_state_build.is_none());
        #[cfg(feature = "jolteon-reuse-write-buffer")]
        assert!(timing.reused_write_buffer);
        assert!(cache.state.is_none());
        assert!(
            store.db.get(index_key(
                HEADER_ID, speculative.header().id(store.network()).as_slice()
            ))?.is_none()
        );
        assert_eq!(store.header(1)?.as_ref(), Some(certified.header()));
        store.install_durable_tip(&certified, false);
        assert!(store.prebuild_certified_state(speculative).is_none());
        Ok(())
    }

    #[cfg(feature = "jolteon-reuse-write-buffer")]
    #[test]
    fn consecutive_certified_writes_reset_the_write_buffer() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut store = Store::open(home.path(), &spec(), Retention::Archive)?;
        let mut cache = CertifiedWriteCache::default();
        let write = |store: &mut Store, cache: &mut CertifiedWriteCache, time, records| {
            let prepared = Arc::new(store.prepare(&[], time, false)?);
            let persist = store.certified_write(Arc::clone(&prepared), records, Vec::new())?;
            let timing = persist(cache)?;
            store.install_durable_tip(&prepared, timing.cell_node_publication.is_some());
            Ok::<_, anyhow::Error>(timing.reused_write_buffer)
        };
        let marker = b"writer-test".to_vec();
        assert!(!write(
            &mut store, &mut cache, 1001, vec![(marker.clone(), b"first".to_vec().into())],
        )?);
        assert!(cache.spare.as_ref().is_some_and(WriteBatch::is_empty));
        store.persist_consensus(&[(marker.clone(), b"second".to_vec().into())])?;

        assert!(write(&mut store, &mut cache, 1002, Vec::new())?);
        assert!(cache.spare.as_ref().is_some_and(WriteBatch::is_empty));
        assert_eq!(store.consensus_record(&marker)?.as_deref(), Some(b"second".as_slice()));
        drop(store);
        let recovered = Store::open(home.path(), &spec(), Retention::Archive)?;
        assert_eq!(recovered.tip().context.height, 2);
        assert_eq!(recovered.consensus_record(&marker)?.as_deref(), Some(b"second".as_slice()));
        Ok(())
    }

    #[cfg(any(
        feature = "native-commitment-pipeline",
        feature = "native-cell-subtree-parallel",
        feature = "native-parallel-cell-record-hashes"
    ))]
    #[test]
    fn namespace_commitments_keep_the_ordered_write_batch() -> Result<()> {
        use bytes::Bytes;
        use ckb_types::packed::Script;
        use stem_ledger::CellInput;

        let home = tempfile::tempdir()?;
        let mut spec = spec();
        let lock = Script::default();
        let capacity = Cell::minimum_capacity(&lock, &[], 0)? + 1_000_000_000;
        #[cfg(feature = "native-parallel-cell-record-hashes")]
        let large_cell = Cell::new(
            Cell::minimum_capacity(&lock, &[], 4096)?,
            lock.clone(),
            vec![],
            Bytes::from(vec![0x5a; 4096]),
        )?;
        let cell = Cell::new(capacity, lock, vec![], Bytes::new())?;
        let spent = (0..400)
            .map(|index| OutPoint::new([41; 32].pack(), index))
            .collect::<Vec<_>>();
        spec.development_cells
            .extend(spent.iter().cloned().map(|point| (point, cell.clone())));
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
        let build = |view: &Store, parallel| {
            let mut overlay = view.overlay(None);
            let origin = Origin::new(view.network(), &context);
            for (index, point) in spent.iter().enumerate() {
                overlay.live.insert(point.clone(), None);
                let created = &cell;
                #[cfg(feature = "native-parallel-cell-record-hashes")]
                let created = if index % 64 == 0 { &large_cell } else { created };
                overlay.live.insert(
                    OutPoint::new([42; 32].pack(), index as u32),
                    Some(LiveCell {
                        cell: created.clone(),
                        origin: origin.clone(),
                    }),
                );
            }
            let receipts = vec![
                Receipt {
                    fee: 100,
                    cycles: 200,
                    protocol_result: Bytes::new(),
                };
                execution.len()
            ];
            view.finish_block(
                context.clone(),
                overlay,
                &execution,
                receipts,
                0,
                25_600,
                parallel,
            )
        };
        let serial = build(&store, false)?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
        let parallel = pool.install(|| build(&store, true))?;
        assert!(!serial.batch.writes.is_empty());
        assert_same_batch_writes(&serial.batch, &parallel.batch);
        assert_eq!(serial.header().encode(), parallel.header().encode());
        #[cfg(all(feature = "native-cell-subtree-parallel", feature = "jolteon-commit-pipeline"))]
        {
            #[cfg(feature = "native-verified-cell-journal")]
            let witness = {
                let mut changes = serial.live.iter().map(|(point, cell)| {
                    (
                        record::cell_key(&store.cells, point),
                        cell.as_ref().map(|cell| record::cell_value(&store.cells, point, cell)),
                    )
                }).collect::<Vec<_>>();
                changes.sort_unstable_by_key(|(id, _)| *id);
                let (root, edits) = store.cells.batch_update(
                    as_key(&store.tip.state.cell_root),
                    |id| store.node(Namespace::Cells, id),
                    &changes,
                    |_, _, _| {},
                )?;
                assert_eq!(root.as_slice(), serial.header().state.cell_root.as_slice());
                edits
            };
            let mut store = store;
            store.enable_cell_node_index(NonZeroUsize::new(8).unwrap())?;
            #[cfg(feature = "native-stream-node-publication")]
            let grouped = {
                // The raw journal uses the same test shard layout and the
                // ordinary grouping path, independent of the sorted stream.
                let mut raw = Batch::default();
                for (id, bytes) in &witness {
                    let key = state_key(STATE_NODE, Namespace::Cells, id);
                    match bytes {
                        Some(bytes) => raw.put_owned(key, node_storage::compact(bytes.clone())),
                        None => raw.delete(key),
                    }
                }
                let index = NodeIndex::recover(
                    &store,
                    store.cell_node_index_status().unwrap().limit_bytes,
                )?;
                index.advance(&raw);
                index
            };
            let pinned = store.pinned_view();
            let next: Key = parallel.header().state.cell_root.unpack();
            let parallel = Arc::new(parallel);
            let write = store.certified_write(Arc::clone(&parallel), Vec::new(), Vec::new())?;
            #[cfg(feature = "jolteon-prebuilt-state-write")]
            let prebuild = store.prebuild_certified_state(Arc::clone(&parallel)).unwrap();
            let from_snapshot = pinned
                .with_snapshot(|old| {
                    let timing = std::thread::spawn(move || {
                        let mut cache = CertifiedWriteCache::default();
                        #[cfg(feature = "jolteon-prebuilt-state-write")]
                        prebuild(&mut cache);
                        write(&mut cache)
                    })
                        .join()
                        .map_err(|_| anyhow::anyhow!("certified write panicked"))??;
                    #[cfg(feature = "jolteon-prebuilt-state-write")]
                    assert!(timing.reused_state_build.is_some());
                    assert_eq!(
                        timing.cell_node_publication.is_some(),
                        cfg!(feature = "jolteon-resident-node-publication"),
                    );
                    assert_eq!(store.tip(), old.tip());
                    if cfg!(feature = "jolteon-resident-node-publication") {
                        assert!(store.cell_nodes.as_ref().unwrap().lookup(&next).is_some());
                    }
                    pool.install(|| build(old, true))
                })?
                .context("missing snapshot for the old parent")?;
            assert_same_batch_writes(&from_snapshot.batch, &serial.batch);
            assert_eq!(from_snapshot.header(), serial.header());
            assert!(pinned.with_snapshot(|_| Ok(()))?.is_none());
            #[cfg(feature = "native-prepared-cell-record-keys")]
            for (point, cell) in serial.live.iter() {
                let expected = cell.as_ref().map(|cell| record::encode_cell(point, cell));
                let stored = store.db.get(store.live_cell_key(point))?;
                assert_eq!(stored.as_deref(), expected.as_deref());
            }
            #[cfg(feature = "native-stream-node-publication")]
            {
                store.install_durable_tip(&parallel, cfg!(feature = "jolteon-resident-node-publication"));
                let streamed = store.cell_nodes.as_ref().unwrap();
                let expected = grouped.status();
                let actual = streamed.status();
                assert!(actual.active && expected.active);
                assert_eq!(actual.nodes, expected.nodes);
                assert!(actual.accounted_bytes <= actual.limit_bytes);
                assert!(expected.accounted_bytes <= expected.limit_bytes);
                for (id, bytes) in &witness {
                    assert_eq!(streamed.lookup(id).is_some(), bytes.is_some());
                }
                let query = [
                    record::cell_key(&store.cells, &spent[0]),
                    record::cell_key(&store.cells, &OutPoint::new([42; 32].pack(), 0)),
                ];
                let read = |index: &NodeIndex| {
                    store.cells.edit(next, |id| Ok(index.lookup(id))).prove_many(&query)
                };
                assert_eq!(read(streamed)?, read(&grouped)?);
            }
            #[cfg(feature = "native-verified-cell-journal")]
            for (id, expected) in witness {
                let stored = store.db.get(state_key(STATE_NODE, Namespace::Cells, &id))?;
                assert_eq!(stored.as_deref(), expected.map(node_storage::compact).as_deref());
            }
        }
        Ok(())
    }
}
