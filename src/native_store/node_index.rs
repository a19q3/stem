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
    #[serde(default)]
    pub peak_accounted_bytes: usize,
    #[serde(default)]
    pub shard_rebuilds: u64,
    #[serde(default)]
    pub uncached_inserts: u64,
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
    peak_bytes: AtomicUsize,
    limit_bytes: usize,
    recovery_us: u64,
    rebuilds: AtomicU64,
    uncached_inserts: AtomicU64,
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
    fn new(limit_bytes: usize) -> Self {
        Self {
            shards: std::array::from_fn(|_| Shard::default()),
            active: AtomicBool::new(true),
            bytes: AtomicUsize::new(0),
            peak_bytes: AtomicUsize::new(0),
            limit_bytes,
            recovery_us: 0,
            rebuilds: AtomicU64::new(0),
            uncached_inserts: AtomicU64::new(0),
        }
    }

    pub(super) fn recover(store: &Store, limit_bytes: usize) -> Result<Self> {
        let started = Instant::now();
        let mut index = Self::new(limit_bytes);
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
        // An in-place reserve may double the table to clear tombstones. A
        // fresh table can use the capacity required by the live entries.
        // Budget the old and replacement allocations together.
        if bytes.saturating_add(estimate) > self.limit_bytes {
            return false;
        }
        let mut replacement = Rows::with_hasher(rows.hasher().clone());
        if replacement.try_reserve(count).is_err() {
            return false;
        }
        let peak = bytes.saturating_add(replacement.allocation_size());
        if peak > self.limit_bytes {
            return false;
        }
        self.peak_bytes.fetch_max(peak, Ordering::Relaxed);
        replacement.extend(rows.drain());
        *rows = replacement;
        self.bytes.store(peak - old, Ordering::Relaxed);
        if old != 0 {
            self.rebuilds.fetch_add(1, Ordering::Relaxed);
        }
        true
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
        let mut by_shard: [Vec<(&Key, Option<&[u8]>)>; SHARDS] =
            std::array::from_fn(|_| Vec::new());
        for (key, value) in &batch.writes {
            if let Some(id) = key
                .strip_prefix(&prefix)
                .and_then(|bytes| <&Key>::try_from(bytes).ok())
            {
                by_shard[slot(id)].push((id, value.as_deref()));
            }
        }
        for (shard, changes) in self.shards.iter().zip(by_shard) {
            if changes.is_empty() {
                continue;
            }
            let mut rows = shard.rows.write().expect("Cell node shard");
            for (id, encoded) in changes {
                let Some(encoded) = encoded else {
                    rows.remove(id);
                    continue;
                };
                let Ok(node) = node_storage::canonical(encoded) else {
                    drop(rows);
                    self.retire();
                    return;
                };
                let count = rows.len() + 1;
                if rows.contains_key(id) || self.reserve(&mut rows, count) {
                    rows.insert(*id, node);
                } else {
                    // The node is already durable. Reads use the caller's
                    // RocksDB snapshot when its content address is absent.
                    self.uncached_inserts.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    }

    pub(super) fn retire(&self) -> usize {
        self.active.store(false, Ordering::Release);
        for shard in &self.shards {
            *shard.rows.write().expect("Cell node shard") = Rows::default();
        }
        self.bytes.store(0, Ordering::Relaxed);
        self.limit_bytes
    }

    pub(super) fn status(&self) -> CellNodeIndexStatus {
        let nodes = self
            .shards
            .iter()
            .map(|shard| shard.rows.read().expect("Cell node shard").len())
            .sum();
        CellNodeIndexStatus {
            active: self.active.load(Ordering::Acquire),
            nodes,
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
            peak_accounted_bytes: self.peak_bytes.load(Ordering::Relaxed),
            shard_rebuilds: self.rebuilds.load(Ordering::Relaxed),
            uncached_inserts: self.uncached_inserts.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_store::state_key;

    fn key(index: u32) -> Key {
        let mut key = [0; 32];
        key[1..5].copy_from_slice(&index.to_le_bytes());
        key
    }

    fn fill(index: &NodeIndex, count: u32) {
        let mut batch = Batch::default();
        for i in 0..count {
            batch.put(state_key(STATE_NODE, Namespace::Cells, &key(i)), [0; 65]);
        }
        index.advance(&batch);
    }

    #[test]
    fn churn_rebuilds_a_shard_at_fixed_cardinality_and_budget() {
        const NODES: u32 = 100;
        let mut index = NodeIndex::new(1 << 20);
        fill(&index, NODES);
        let allocation = index.status().accounted_bytes;
        index.limit_bytes = 2 * allocation + 64;
        for offset in 0..4096 {
            let mut batch = Batch::default();
            batch.delete(state_key(STATE_NODE, Namespace::Cells, &key(offset)));
            batch.put(
                state_key(STATE_NODE, Namespace::Cells, &key(NODES + offset)),
                [0; 65],
            );
            index.advance(&batch);
        }
        let status = index.status();
        assert!(status.active && status.shard_rebuilds > 0);
        assert_eq!(status.nodes, NODES as usize);
        assert_eq!(status.uncached_inserts, 0);
        assert_eq!(status.accounted_bytes, allocation);
        assert!(status.peak_accounted_bytes <= status.limit_bytes);
    }

    #[test]
    fn budget_exhaustion_preserves_unrelated_entries() {
        let mut index = NodeIndex::new(1 << 20);
        fill(&index, 100);
        index.limit_bytes = index.status().accounted_bytes;
        let mut batch = Batch::default();
        let mut miss = key(101);
        miss[0] = 1;
        batch.put(state_key(STATE_NODE, Namespace::Cells, &miss), [0; 65]);
        index.advance(&batch);
        let status = index.status();
        assert!(status.active);
        assert_eq!(status.nodes, 100);
        assert_eq!(status.uncached_inserts, 1);
        assert!(index.lookup(&key(99)).is_some());
        assert!(index.lookup(&miss).is_none());
    }
}
