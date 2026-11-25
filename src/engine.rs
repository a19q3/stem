use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use anyhow::{Context, Result, ensure};
use ckb_chain_spec::consensus::Consensus;
use ckb_script::{TransactionScriptsVerifier, TxVerifyEnv};
use ckb_traits::{
    CellDataProvider, ExtensionProvider, HeaderFields, HeaderFieldsProvider, HeaderProvider,
};
use ckb_types::{
    bytes::Bytes,
    core::{
        DepType, HeaderView, TransactionView,
        cell::{
            CellMeta, CellProvider, CellStatus, HeaderChecker, ResolvedTransaction,
            resolve_transaction,
        },
        error::OutPointError,
    },
    packed::{self, Byte32, OutPoint},
    prelude::*,
};
use ckb_verification::{
    CapacityVerifier, NonContextualTransactionVerifier, TimeRelativeTransactionVerifier,
};
use lru::LruCache;
use rayon::{ThreadPool, ThreadPoolBuilder, prelude::*};

use crate::{
    bridge,
    genesis::{
        MAX_BLOCK_BYTES, MAX_BLOCK_CYCLES, MAX_BLOCK_TXS, MAX_TX_BYTES, MAX_TX_CYCLES, SYSTEM_LOCK,
        header, transactions_root,
    },
    governance,
    store::{Store, outputs},
};

const MAX_RESOLVED_CELL_BYTES: usize = 128 * 1024 * 1024;
const MAX_CELL_REFERENCES: usize = 100_000;
// Give the proposer room to assemble a full block and discard unsuitable candidates.
const MAX_PREPARE_CYCLES: u64 = 2 * MAX_BLOCK_CYCLES;

pub struct Engine {
    consensus: Arc<Consensus>,
    pool: ThreadPool,
    epoch_length: u16,
    scripts: Mutex<LruCache<(Byte32, u64), u64>>,
    script_checks: AtomicU64,
    script_cache_hits: AtomicU64,
}

pub struct Block {
    pub header: HeaderView,
    pub transactions: Vec<TransactionView>,
    pub cycles: Vec<u64>,
    pub cells: HashMap<OutPoint, Option<CellMeta>>,
}

impl Engine {
    pub fn new(consensus: Arc<Consensus>, epoch_length: u16, workers: usize) -> Result<Self> {
        let pool = ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(|index| format!("fern-vm-{index}"))
            .build()?;
        Ok(Self {
            consensus,
            pool,
            epoch_length,
            scripts: Mutex::new(LruCache::new(16_384)),
            script_checks: AtomicU64::new(0),
            script_cache_hits: AtomicU64::new(0),
        })
    }

    pub fn prepare(
        &self,
        store: &Store,
        height: u64,
        time: u64,
        raw: Vec<Bytes>,
        max_bytes: usize,
    ) -> Result<(Vec<Bytes>, Block)> {
        self.execute(
            store,
            height,
            time,
            raw,
            max_bytes.min(MAX_BLOCK_BYTES),
            Some(MAX_PREPARE_CYCLES),
        )
    }

    pub fn verify(&self, store: &Store, height: u64, time: u64, raw: Vec<Bytes>) -> Result<Block> {
        self.execute(store, height, time, raw, MAX_BLOCK_BYTES, None)
            .map(|(_, block)| block)
    }

    pub fn check_tx(&self, store: &Store, raw: Bytes) -> Result<u64> {
        let parent = &store.tip;
        let candidate = self.header(parent, parent.number() + 1, parent.timestamp())?;
        let overlay = Overlay::new(store);
        let rtx = self.resolve(&overlay, &raw)?;
        let loader = Loader {
            store,
            candidate: &candidate,
        };
        self.verify_scripts(
            Arc::new(rtx),
            loader,
            Arc::new(TxVerifyEnv::new_commit(&candidate)),
        )
    }

    pub fn script_metrics(&self) -> (u64, u64) {
        (
            self.script_checks.load(Ordering::Relaxed),
            self.script_cache_hits.load(Ordering::Relaxed),
        )
    }

    fn header(&self, parent: &HeaderView, height: u64, time: u64) -> Result<HeaderView> {
        ensure!(height == parent.number() + 1, "unexpected block height");
        ensure!(time >= parent.timestamp(), "block timestamp regressed");
        ensure!(
            height / u64::from(self.epoch_length) < 1 << 24,
            "epoch number overflow"
        );
        Ok(header(height, time, self.epoch_length)
            .parent_hash(parent.hash())
            .build())
    }

    fn execute(
        &self,
        store: &Store,
        height: u64,
        time: u64,
        raw: Vec<Bytes>,
        max_bytes: usize,
        preparation_budget: Option<u64>,
    ) -> Result<(Vec<Bytes>, Block)> {
        let filter = preparation_budget.is_some();
        let candidate = self.header(&store.tip, height, time)?;
        let mut overlay = Overlay::new(store);
        let mut used_bytes = 0;
        let mut cell_references = 0;
        let mut resolved = Vec::new();
        for bytes in raw {
            if resolved.len() == MAX_BLOCK_TXS {
                ensure!(filter, "block transaction count exceeded");
                break;
            }
            if used_bytes + bytes.len() > max_bytes {
                ensure!(filter, "block byte limit exceeded");
                continue;
            }
            let rtx = match self.resolve(&overlay, &bytes) {
                Ok(rtx) => Arc::new(rtx),
                Err(_) if filter => continue,
                Err(error) => return Err(error),
            };
            let references = rtx.resolved_inputs.len()
                + rtx.resolved_cell_deps.len()
                + rtx.resolved_dep_groups.len();
            if cell_references + references > MAX_CELL_REFERENCES {
                ensure!(filter, "block cell reference limit exceeded");
                continue;
            }
            cell_references += references;
            used_bytes += bytes.len();
            overlay.apply(&rtx.transaction, &candidate, resolved.len());
            resolved.push((bytes, rtx));
        }

        // Canonical order is the wire order. Cell reads, writes, dep-group expansion and
        // double-spend checks happen in that order. The VM consumes immutable resolved
        // cells and may run in any order. Current-block headers are never valid header_deps.
        let loader = Loader {
            store,
            candidate: &candidate,
        };
        let env = Arc::new(TxVerifyEnv::new_commit(&candidate));
        let check = |(_, tx): &(Bytes, Arc<ResolvedTransaction>)| {
            self.verify_scripts(Arc::clone(tx), loader, Arc::clone(&env))
        };
        let costs: Vec<_> = self.pool.install(|| {
            if let Some(mut remaining) = preparation_budget {
                let mut costs = Vec::new();
                while costs.len() < resolved.len() && remaining >= MAX_TX_CYCLES {
                    // Reserve the per-transaction maximum before launching each batch.
                    // Charge failures at that maximum since they may exhaust the VM budget.
                    let count =
                        ((remaining / MAX_TX_CYCLES) as usize).min(resolved.len() - costs.len());
                    let batch: Vec<_> = resolved[costs.len()..costs.len() + count]
                        .par_iter()
                        .map(check)
                        .collect();
                    remaining -= batch
                        .iter()
                        .map(|cost| cost.as_ref().copied().unwrap_or(MAX_TX_CYCLES))
                        .sum::<u64>();
                    costs.extend(batch);
                }
                return costs;
            }
            let verified_cycles = AtomicU64::new(0);
            resolved
                .par_iter()
                .map(|candidate| {
                    ensure!(
                        verified_cycles.load(Ordering::Relaxed) <= MAX_BLOCK_CYCLES,
                        "proposal exceeded its verification budget"
                    );
                    let cost = check(candidate);
                    let measured = cost.as_ref().copied().unwrap_or(MAX_BLOCK_CYCLES + 1);
                    verified_cycles.fetch_add(measured, Ordering::Relaxed);
                    cost
                })
                .collect()
        });
        let deferred = costs.len() != resolved.len();

        let mut selected = Vec::new();
        let mut transactions = Vec::new();
        let mut cycles = Vec::new();
        let mut total_cycles = 0;
        let mut dropped = HashSet::new();
        for ((raw, rtx), cost) in resolved.into_iter().zip(costs) {
            let cost = match cost {
                Ok(cost) if total_cycles + cost <= MAX_BLOCK_CYCLES => cost,
                Ok(_) if !filter => anyhow::bail!("block cycle limit exceeded"),
                Err(error) if !filter => return Err(error),
                _ => {
                    dropped.insert(rtx.transaction.hash());
                    continue;
                }
            };
            if !dropped.is_empty()
                && rtx
                    .resolved_inputs
                    .iter()
                    .chain(&rtx.resolved_cell_deps)
                    .chain(&rtx.resolved_dep_groups)
                    .any(|cell| dropped.contains(&cell.out_point.tx_hash()))
            {
                dropped.insert(rtx.transaction.hash());
                continue;
            }
            total_cycles += cost;
            selected.push(raw);
            transactions.push(rtx.transaction.clone());
            cycles.push(cost);
        }
        let header = candidate
            .data()
            .as_advanced_builder()
            .transactions_root(transactions_root(&transactions))
            .build();
        let cells = if !deferred && dropped.is_empty() {
            let hash = header.hash();
            for cell in overlay.writes.values_mut().flatten() {
                cell.transaction_info
                    .as_mut()
                    .expect("new cell origin")
                    .block_hash = hash.clone();
            }
            overlay.writes
        } else {
            let mut accepted = Overlay::new(store);
            for (index, tx) in transactions.iter().enumerate() {
                accepted.apply(tx, &header, index);
            }
            accepted.writes
        };
        Ok((
            selected,
            Block {
                header,
                transactions,
                cycles,
                cells,
            },
        ))
    }

    fn resolve(&self, overlay: &Overlay<'_>, raw: &Bytes) -> Result<ResolvedTransaction> {
        ensure!(raw.len() <= MAX_TX_BYTES, "transaction byte limit exceeded");
        let tx = packed::Transaction::from_slice(raw)?.into_view();
        ensure!(
            !overlay.tx_hashes.contains(&tx.hash())
                && !overlay.store.has_canonical_transaction(&tx.hash()),
            "transaction already exists"
        );
        if bridge::is_mint(&tx) {
            let bridge = overlay
                .store
                .bridge
                .as_ref()
                .context("peg-in is disabled")?;
            let guardians = overlay
                .store
                .guardians
                .as_ref()
                .context("missing deposit guardians")?;
            let anchor = bridge.verify_mint(&overlay.store.genesis_hash, &tx, guardians)?;
            ensure!(
                overlay
                    .mint_anchors
                    .get(&anchor.height)
                    .is_none_or(|hash| hash == &anchor.hash),
                "peg-ins in this block have conflicting L1 confirmations"
            );
            return Ok(ResolvedTransaction {
                transaction: tx,
                resolved_inputs: Vec::new(),
                resolved_cell_deps: Vec::new(),
                resolved_dep_groups: Vec::new(),
            });
        }
        NonContextualTransactionVerifier::new(&tx, &self.consensus).verify()?;
        ensure!(
            !tx.input_pts_iter().any(|point| point.is_null()),
            "null input"
        );
        for next in [&overlay.new_governance, &overlay.new_guardian_governance]
            .into_iter()
            .flatten()
        {
            ensure!(
                !tx.input_pts_iter().any(|point| &point == next),
                "governance can change only once per block"
            );
        }
        ensure!(
            tx.cell_deps_iter()
                .all(|dep| DepType::try_from(dep.dep_type()).is_ok()),
            "unknown cell dependency type"
        );
        for (output, data) in tx.outputs_with_data_iter() {
            if bridge::is_burn(&output.lock()) {
                overlay
                    .store
                    .bridge
                    .as_ref()
                    .context("peg-out is disabled")?
                    .payout(&output, &data, &overlay.store.genesis_hash)?;
            }
        }
        let resolved = resolve_transaction(tx, &mut HashSet::new(), overlay, overlay)?;
        // The Live Cell set is sufficient to reject ordinary replays only when
        // every ordinary transaction must consume an existing input. The mint
        // path above always checks the permanent transaction record.
        ensure!(
            !resolved.resolved_inputs.is_empty(),
            "ordinary transaction has no live input"
        );
        if let Some(bridge) = &overlay.store.bridge {
            bridge.verify_asset_balance(&resolved)?;
        }
        let committee = overlay.store.governance.as_ref();
        let guardians = overlay.store.guardian_governance.as_ref();
        if committee.is_some() || guardians.is_some() {
            governance::guard(&resolved, committee, guardians)?;
        }
        ensure!(
            resolved
                .resolved_inputs
                .iter()
                .all(|cell| cell.cell_output.lock() != *SYSTEM_LOCK),
            "immutable genesis cell cannot be spent"
        );
        let dao = self.consensus.dao_type_hash();
        ensure!(
            resolved
                .transaction
                .outputs()
                .into_iter()
                .chain(
                    resolved
                        .resolved_inputs
                        .iter()
                        .map(|cell| cell.cell_output.clone())
                )
                .all(|output| output
                    .type_()
                    .to_opt()
                    .is_none_or(|script| script.calc_script_hash() != dao)),
            "Nervos DAO is unavailable on this chain"
        );
        Ok(resolved)
    }

    fn verify_scripts(
        &self,
        tx: Arc<ResolvedTransaction>,
        loader: Loader<'_>,
        env: Arc<TxVerifyEnv>,
    ) -> Result<u64> {
        if bridge::is_mint(&tx.transaction) {
            // A system mint has already passed the bridge certificate checks.
            return Ok(0);
        }
        // CKB's VM owns the loader and requires it to be 'static. The resolved cells
        // already carry their bytes. Supply the exact committed headers declared by
        // the transaction and keep the store-backed verifier for median time windows.
        TimeRelativeTransactionVerifier::new(
            Arc::clone(&tx),
            Arc::clone(&self.consensus),
            loader,
            Arc::clone(&env),
        )
        .verify()
        .context("time locks")?;
        CapacityVerifier::new(Arc::clone(&tx), self.consensus.dao_type_hash())
            .verify()
            .context("capacity")?;
        // With no header deps the VM cannot observe the containing blocks of
        // resolved cells. The outpoints name immutable content and resolution,
        // capacity and since are still checked in the current state.
        let cache_key = tx
            .transaction
            .header_deps()
            .is_empty()
            .then(|| (tx.transaction.witness_hash(), env.epoch().number()));
        if let Some(key) = &cache_key
            && let Some(cycles) = self
                .scripts
                .lock()
                .expect("script cache lock")
                .get(key)
                .copied()
        {
            self.script_cache_hits.fetch_add(1, Ordering::Relaxed);
            return Ok(cycles);
        }
        let headers = tx
            .transaction
            .header_deps_iter()
            .map(|hash| {
                let header = loader
                    .get_header(&hash)
                    .expect("resolved header dependency");
                (hash, header)
            })
            .collect();
        self.script_checks.fetch_add(1, Ordering::Relaxed);
        let cycles = TransactionScriptsVerifier::new_with_vm_reuse(
            tx,
            VmHeaders(headers),
            Arc::clone(&self.consensus),
            env,
        )
        .verify(MAX_TX_CYCLES)?;
        if let Some(key) = cache_key {
            self.scripts
                .lock()
                .expect("script cache lock")
                .put(key, cycles);
        }
        Ok(cycles)
    }
}

struct Overlay<'a> {
    store: &'a Store,
    writes: HashMap<OutPoint, Option<CellMeta>>,
    reads: RefCell<Reads>,
    tx_hashes: HashSet<Byte32>,
    new_governance: Option<OutPoint>,
    new_guardian_governance: Option<OutPoint>,
    mint_anchors: HashMap<u64, ckb_types::H256>,
}

#[derive(Default)]
struct Reads {
    cells: HashMap<OutPoint, CellStatus>,
    bytes: usize,
}

impl<'a> Overlay<'a> {
    fn new(store: &'a Store) -> Self {
        Self {
            store,
            writes: HashMap::new(),
            reads: RefCell::default(),
            tx_hashes: HashSet::new(),
            new_governance: None,
            new_guardian_governance: None,
            mint_anchors: HashMap::new(),
        }
    }

    fn apply(&mut self, tx: &TransactionView, header: &HeaderView, index: usize) {
        self.tx_hashes.insert(tx.hash());
        if !bridge::is_mint(tx) {
            self.writes
                .extend(tx.input_pts_iter().map(|point| (point, None)));
            if let Some(next) = governance::successor_point(tx, self.store.governance.as_ref()) {
                self.new_governance = Some(next);
            }
            if let Some(next) =
                governance::successor_point(tx, self.store.guardian_governance.as_ref())
            {
                self.new_guardian_governance = Some(next);
            }
        } else if self.store.guardian_governance.is_some() {
            let anchor = bridge::Certificate::from_mint(tx)
                .expect("resolved peg-in certificate")
                .anchor();
            self.mint_anchors.insert(anchor.height, anchor.hash);
        }
        self.writes.extend(
            outputs(tx, header, index)
                .filter(|cell| !bridge::is_burn(&cell.cell_output.lock()))
                .map(|cell| (cell.out_point.clone(), Some(cell))),
        );
    }
}

impl CellProvider for Overlay<'_> {
    fn cell(&self, point: &OutPoint, eager_load: bool) -> CellStatus {
        if let Some(status) = self.writes.get(point) {
            return status.clone().map_or(CellStatus::Dead, CellStatus::Live);
        }
        let mut reads = self.reads.borrow_mut();
        if let Some(status) = reads.cells.get(point) {
            return status.clone();
        }
        let status = self.store.cell(point, eager_load);
        if let CellStatus::Live(cell) = &status {
            let bytes = cell.data_bytes as usize + cell.cell_output.as_slice().len();
            if reads.bytes + bytes > MAX_RESOLVED_CELL_BYTES {
                return CellStatus::Unknown;
            }
            reads.bytes += bytes;
        }
        reads.cells.insert(point.clone(), status.clone());
        status
    }
}

impl HeaderChecker for Overlay<'_> {
    fn check_valid(&self, hash: &Byte32) -> std::result::Result<(), OutPointError> {
        self.store
            .header(hash)
            .map(|_| ())
            .ok_or_else(|| OutPointError::InvalidHeader(hash.clone()))
    }
}

#[derive(Clone, Copy)]
struct Loader<'a> {
    store: &'a Store,
    candidate: &'a HeaderView,
}

impl HeaderProvider for Loader<'_> {
    fn get_header(&self, hash: &Byte32) -> Option<HeaderView> {
        if *hash == self.candidate.hash() {
            return Some(self.candidate.clone());
        }
        self.store.header(hash)
    }
}

impl HeaderFieldsProvider for Loader<'_> {
    fn get_header_fields(&self, hash: &Byte32) -> Option<HeaderFields> {
        self.get_header(hash).map(|header| HeaderFields {
            hash: hash.clone(),
            number: header.number(),
            epoch: header.epoch(),
            timestamp: header.timestamp(),
            parent_hash: header.parent_hash(),
        })
    }
}

#[derive(Clone)]
struct VmHeaders(HashMap<Byte32, HeaderView>);

impl HeaderProvider for VmHeaders {
    fn get_header(&self, hash: &Byte32) -> Option<HeaderView> {
        self.0.get(hash).cloned()
    }
}

impl CellDataProvider for VmHeaders {
    fn get_cell_data(&self, _point: &OutPoint) -> Option<Bytes> {
        None
    }

    fn get_cell_data_hash(&self, _point: &OutPoint) -> Option<Byte32> {
        None
    }
}

impl ExtensionProvider for VmHeaders {
    fn get_block_extension(&self, _hash: &Byte32) -> Option<packed::Bytes> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{genesis::script_consensus, tests::payments};

    #[test]
    fn preparation_budget_excludes_unverified_cells() {
        let fixture = payments(2);
        let path = tempfile::tempdir().unwrap();
        let store = Store::open(path.path(), &fixture.genesis).unwrap();
        let consensus = script_consensus().unwrap();
        let engine = Engine::new(consensus.clone(), fixture.genesis.epoch_length, 4).unwrap();
        let serial = Engine::new(consensus, fixture.genesis.epoch_length, 1).unwrap();
        let first = Bytes::copy_from_slice(fixture.transactions[0].as_bytes());
        let second = Bytes::copy_from_slice(fixture.transactions[1].as_bytes());
        let time = fixture.genesis.timestamp_ms;
        let (selected, proposal) = engine
            .execute(
                &store,
                1,
                time,
                vec![first.clone(), second],
                MAX_BLOCK_BYTES,
                Some(MAX_TX_CYCLES),
            )
            .unwrap();
        assert_eq!(selected, vec![first]);
        assert_eq!(engine.script_metrics(), (1, 0));
        let replayed = serial.verify(&store, 1, time, selected).unwrap();
        assert_eq!(proposal.header, replayed.header);
        assert_eq!(proposal.cycles, replayed.cycles);
        assert_eq!(proposal.cells, replayed.cells);
    }
}
