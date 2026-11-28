//! Bounded in-memory backing for immutable, content-addressed Cell Patricia nodes.

use std::{
    hash::RandomState,
    mem::size_of,
    sync::{
        RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{ReadOptions, prelude::*};
use ckb_types::prelude::*;
use hashbrown::HashMap;
use stem_ledger::patricia::{Key, Namespace};
use stem_stake::patricia::Commitment;

use super::{
    Batch, STATE_NODE, Store, TIP,
    node_storage::{self, CanonicalNode},
};

const SHARDS: usize = 64;
type Rows = HashMap<Key, CanonicalNode, RandomState>;

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct CellNodeIndexStatus {
    pub active: bool,
    pub nodes: usize,
    pub accounted_bytes: usize,
    pub limit_bytes: usize,
    pub recovery_us: u64,
    pub hits: u64,
    pub misses: u64,
}

#[derive(Default)]
struct Shard {
    rows: RwLock<Rows>,
    hits: AtomicU64,
    misses: AtomicU64,
}

/// Node IDs authenticate their canonical bytes. A retained node can be used by
/// any root, while a miss must fall back to that reader's database snapshot.
pub(super) struct NodeIndex {
    shards: [Shard; SHARDS],
    active: AtomicBool,
    bytes: AtomicUsize,
    nodes: AtomicUsize,
    limit_bytes: usize,
    recovery_us: u64,
}

fn slot(id: &Key) -> usize {
    usize::from(id[0]) % SHARDS
}

fn id_from_key(key: &[u8]) -> Result<Key> {
    Ok(key
        .get(2..)
        .context("truncated Cell node key")?
        .try_into()?)
}

impl NodeIndex {
    pub(super) fn recover(store: &Store, limit_bytes: usize) -> Result<Self> {
        let started = Instant::now();
        let mut index = Self {
            shards: std::array::from_fn(|_| Shard::default()),
            active: AtomicBool::new(true),
            bytes: AtomicUsize::new(0),
            nodes: AtomicUsize::new(0),
            limit_bytes,
            recovery_us: 0,
        };
        let snapshot = store.db.snapshot();
        let tip = snapshot.get(TIP)?.context("missing committed Stem tip")?;
        ensure!(
            tip.as_ref() == store.tip.encode(),
            "Cell node tip changed during recovery"
        );
        let prefix = [STATE_NODE, Namespace::Cells.tag()];
        let mut options = ReadOptions::default();
        options.fill_cache(false);
        let mut cursor = snapshot.get_raw_iter(&options);
        cursor.seek(prefix);
        let mut counts = [0usize; SHARDS];
        while let Some(key) = cursor.key().filter(|key| key.starts_with(&prefix)) {
            counts[slot(&id_from_key(key)?)] += 1;
            cursor.next();
        }
        for (shard, count) in index.shards.iter().zip(counts) {
            let mut rows = shard.rows.write().expect("Cell node shard");
            if !index.reserve(&mut rows, count) {
                drop(rows);
                index.retire();
                index.recovery_us = started.elapsed().as_micros() as u64;
                return Ok(index);
            }
        }

        let commitment = Commitment::new(store.network.unpack(), Namespace::Cells);
        let mut loaded = 0;
        cursor.seek(prefix);
        while let Some(key) = cursor.key().filter(|key| key.starts_with(&prefix)) {
            let id = id_from_key(key)?;
            let encoded = cursor.value().context("missing committed Cell node")?;
            let node = node_storage::canonical(encoded)?;
            ensure!(
                commitment.node_hash(node.as_ref()) == id,
                "corrupt committed Cell node"
            );
            index.shards[slot(&id)]
                .rows
                .get_mut()
                .expect("Cell node shard")
                .insert(id, node);
            loaded += 1;
            cursor.next();
        }
        if store.tip.state.cell_root.as_slice() != store.cells.empty_root() {
            let root: Key = store.tip.state.cell_root.unpack();
            ensure!(
                index.shards[slot(&root)]
                    .rows
                    .get_mut()
                    .expect("Cell node shard")
                    .contains_key(&root),
                "missing committed Cell root node"
            );
        }
        index.nodes.store(loaded, Ordering::Relaxed);
        index.recovery_us = started.elapsed().as_micros() as u64;
        Ok(index)
    }

    fn reserve(&self, rows: &mut Rows, count: usize) -> bool {
        if count <= rows.capacity() {
            return true;
        }
        let Some(buckets) = count
            .checked_mul(8)
            .and_then(|n| n.div_ceil(7).checked_next_power_of_two())
        else {
            return false;
        };
        let estimate = buckets
            .max(8)
            .saturating_mul(size_of::<(Key, CanonicalNode)>() + 1)
            .saturating_add(64);
        let bytes = self.bytes.load(Ordering::Relaxed);
        let old = rows.allocation_size();
        // Tombstones can force the table to double even when the requested
        // count would normally fit. Count both allocations at that peak.
        if bytes.saturating_add(estimate.max(old.saturating_mul(2))) > self.limit_bytes {
            return false;
        }
        if rows.try_reserve(count - rows.len()).is_err() {
            return false;
        }
        let used = bytes - old + rows.allocation_size();
        self.bytes.store(used, Ordering::Relaxed);
        used <= self.limit_bytes
    }

    pub(super) fn lookup(&self, id: &Key) -> Option<CanonicalNode> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        let shard = &self.shards[slot(id)];
        let result = shard.rows.read().expect("Cell node shard").get(id).cloned();
        let counter = if result.is_some() {
            &shard.hits
        } else {
            &shard.misses
        };
        counter.fetch_add(1, Ordering::Relaxed);
        result
    }

    pub(super) fn advance(&self, batch: &Batch) {
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        let prefix = [STATE_NODE, Namespace::Cells.tag()];
        let node_edits = || {
            batch.writes.iter().filter_map(|(key, value)| {
                let id = <&Key>::try_from(key.strip_prefix(&prefix)?).ok()?;
                Some((id, value.as_deref()))
            })
        };
        let mut counts = [0; SHARDS];
        for (id, _) in node_edits() {
            counts[slot(id)] += 1;
        }
        let mut by_shard = counts.map(Vec::with_capacity);
        for (id, value) in node_edits() {
            by_shard[slot(id)].push((id, value));
        }
        let mut nodes = self.nodes.load(Ordering::Relaxed);
        for (shard, changes) in self.shards.iter().zip(by_shard) {
            if changes.is_empty() {
                continue;
            }
            let mut rows = shard.rows.write().expect("Cell node shard");
            let old_len = rows.len();
            for (id, encoded) in changes {
                if let Some(encoded) = encoded {
                    let count = rows.len() + 1;
                    if let Ok(node) = node_storage::canonical(encoded)
                        && (count <= rows.capacity()
                            || rows.contains_key(id)
                            || self.reserve(&mut rows, count))
                    {
                        rows.insert(*id, node);
                        continue;
                    }
                    drop(rows);
                    self.retire();
                    return;
                }
                rows.remove(id);
            }
            nodes = nodes - old_len + rows.len();
        }
        self.nodes.store(nodes, Ordering::Relaxed);
    }

    pub(super) fn retire(&self) -> usize {
        self.active.store(false, Ordering::Release);
        for shard in &self.shards {
            *shard.rows.write().expect("Cell node shard") = Rows::default();
        }
        self.bytes.store(0, Ordering::Relaxed);
        self.nodes.store(0, Ordering::Relaxed);
        self.limit_bytes
    }

    pub(super) fn status(&self) -> CellNodeIndexStatus {
        CellNodeIndexStatus {
            active: self.active.load(Ordering::Acquire),
            nodes: self.nodes.load(Ordering::Relaxed),
            accounted_bytes: self.bytes.load(Ordering::Relaxed),
            limit_bytes: self.limit_bytes,
            recovery_us: self.recovery_us,
            hits: self
                .shards
                .iter()
                .map(|shard| shard.hits.load(Ordering::Relaxed))
                .sum(),
            misses: self
                .shards
                .iter()
                .map(|shard| shard.misses.load(Ordering::Relaxed))
                .sum(),
        }
    }
}
