//! Bounded in-memory backing for immutable, content-addressed Cell Patricia nodes.

use std::{
    sync::{
        RwLock,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{ReadOptions, prelude::*};
use ckb_types::prelude::*;
#[cfg(feature = "native-verified-cell-journal")]
use stem_ledger::patricia::GeneratedNode;
#[cfg(feature = "native-verified-cell-nodes")]
use stem_ledger::patricia::VerifiedNodes;
use stem_ledger::patricia::{Key, Namespace, Patricia};
#[cfg(not(feature = "native-verified-cell-nodes"))]
use {hashbrown::HashMap, std::hash::RandomState, stem_stake::patricia::Commitment};

use super::{
    Batch, STATE_NODE, Store, TIP,
    node_storage::{self, CanonicalNode, NodeRead},
};

#[cfg(feature = "native-hot-cell-node-cache")]
mod hot;
#[cfg(feature = "native-hot-cell-node-cache")]
pub use hot::HotCellNodeStatus;

const SHARDS: usize = 64;
#[cfg(feature = "native-verified-cell-nodes")]
type Rows = VerifiedNodes;
#[cfg(not(feature = "native-verified-cell-nodes"))]
type Rows = HashMap<Key, CanonicalNode, RandomState>;

#[cfg(feature = "native-verified-cell-nodes")]
const ENTRY_BYTES: usize = VerifiedNodes::entry_size();
#[cfg(not(feature = "native-verified-cell-nodes"))]
const ENTRY_BYTES: usize = std::mem::size_of::<(Key, CanonicalNode)>();

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
    #[cfg(feature = "native-hot-cell-node-cache")]
    #[serde(default)]
    pub hot_cache: Option<HotCellNodeStatus>,
}

struct Shard {
    rows: RwLock<Rows>,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl Shard {
    fn empty(_cells: &Patricia) -> Self {
        #[cfg(feature = "native-verified-cell-nodes")]
        let rows = Rows::new(_cells.clone());
        #[cfg(not(feature = "native-verified-cell-nodes"))]
        let rows = Rows::default();
        Self {
            rows: RwLock::new(rows),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }
}

fn insert(rows: &mut Rows, id: Key, node: CanonicalNode) -> Result<()> {
    #[cfg(feature = "native-verified-cell-nodes")]
    rows.insert(id, node.as_ref())?;
    #[cfg(not(feature = "native-verified-cell-nodes"))]
    rows.insert(id, node);
    Ok(())
}

enum NodeUpdate<'a> {
    Stored(&'a [u8]),
    #[cfg(feature = "native-verified-cell-journal")]
    Generated(GeneratedNode<'a>),
}

impl NodeUpdate<'_> {
    fn apply(self, rows: &mut Rows, id: &Key) -> Result<()> {
        match self {
            Self::Stored(encoded) => insert(rows, *id, node_storage::canonical(encoded)?),
            #[cfg(feature = "native-verified-cell-journal")]
            Self::Generated(node) => rows.insert_generated(id, node),
        }
    }
}

/// Node IDs authenticate their canonical bytes. A retained node can be used by
/// any root, while a miss must fall back to that reader's database snapshot.
pub(super) struct NodeIndex {
    shards: [Shard; SHARDS],
    active: AtomicBool,
    bytes: AtomicUsize,
    peak_bytes: AtomicUsize,
    #[cfg(feature = "jolteon-resident-node-publication")]
    nodes: AtomicUsize,
    limit_bytes: usize,
    recovery_us: u64,
    rebuilds: AtomicU64,
    uncached_inserts: AtomicU64,
    #[cfg(feature = "native-hot-cell-node-cache")]
    hot: std::sync::OnceLock<hot::HotNodes>,
}

fn slot(id: &Key) -> usize {
    let first = usize::from(id[0]);
    if cfg!(feature = "native-stream-node-publication") {
        first * SHARDS / (usize::from(u8::MAX) + 1)
    } else {
        first % SHARDS
    }
}

fn id_from_key(key: &[u8]) -> Result<Key> {
    Ok(key
        .get(2..)
        .context("truncated Cell node key")?
        .try_into()?)
}

impl NodeIndex {
    fn new(cells: &Patricia, limit_bytes: usize) -> Self {
        Self {
            shards: std::array::from_fn(|_| Shard::empty(cells)),
            active: AtomicBool::new(true),
            bytes: AtomicUsize::new(0),
            peak_bytes: AtomicUsize::new(0),
            #[cfg(feature = "jolteon-resident-node-publication")]
            nodes: AtomicUsize::new(0),
            limit_bytes,
            recovery_us: 0,
            rebuilds: AtomicU64::new(0),
            uncached_inserts: AtomicU64::new(0),
            #[cfg(feature = "native-hot-cell-node-cache")]
            hot: std::sync::OnceLock::new(),
        }
    }

    pub(super) fn recover(store: &Store, limit_bytes: usize) -> Result<Self> {
        let started = Instant::now();
        let mut index = Self::new(&store.cells, limit_bytes);
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
        #[cfg(feature = "native-hot-cell-node-cache")]
        let mut candidates = 0usize;
        while let Some(key) = cursor.key().filter(|key| key.starts_with(&prefix)) {
            counts[slot(&id_from_key(key)?)] += 1;
            #[cfg(feature = "native-hot-cell-node-cache")]
            {
                candidates += 1;
                if candidates > limit_bytes / ENTRY_BYTES {
                    // A complete index cannot fit even without hash-table overhead.
                    index.use_database_reads(store, started);
                    return Ok(index);
                }
            }
            cursor.next();
        }
        for (shard, count) in index.shards.iter().zip(counts) {
            let mut rows = shard.rows.write().expect("Cell node shard");
            if !index.reserve(&mut rows, count) {
                drop(rows);
                index.use_database_reads(store, started);
                return Ok(index);
            }
        }

        #[cfg(not(feature = "native-verified-cell-nodes"))]
        let commitment = Commitment::new(store.network.unpack(), Namespace::Cells);
        #[cfg(feature = "jolteon-resident-node-publication")]
        let mut loaded = 0;
        cursor.seek(prefix);
        while let Some(key) = cursor.key().filter(|key| key.starts_with(&prefix)) {
            let id = id_from_key(key)?;
            let encoded = cursor.value().context("missing committed Cell node")?;
            let node = node_storage::canonical(encoded)?;
            #[cfg(not(feature = "native-verified-cell-nodes"))]
            ensure!(
                commitment.node_hash(node.as_ref()) == id,
                "corrupt committed Cell node"
            );
            let rows = index.shards[slot(&id)]
                .rows
                .get_mut()
                .expect("Cell node shard");
            insert(rows, id, node).context("corrupt committed Cell node")?;
            #[cfg(feature = "jolteon-resident-node-publication")]
            {
                loaded += 1;
            }
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
        #[cfg(feature = "jolteon-resident-node-publication")]
        index.nodes.store(loaded, Ordering::Relaxed);
        index.recovery_us = started.elapsed().as_micros() as u64;
        Ok(index)
    }

    fn use_database_reads(&mut self, _store: &Store, started: Instant) {
        self.retire();
        #[cfg(feature = "native-hot-cell-node-cache")]
        self.start_hot(&_store.cells, _store.cell_node_header);
        self.recovery_us = started.elapsed().as_micros() as u64;
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
            .saturating_mul(ENTRY_BYTES + 1)
            .saturating_add(64);
        let bytes = self.bytes.load(Ordering::Relaxed);
        let old = rows.allocation_size();
        // A fresh table can keep the same capacity when tombstones exhaust
        // its growth allowance. Budget both live allocations during the move.
        let available = self.limit_bytes.saturating_sub(bytes);
        if estimate > available {
            return false;
        }
        #[cfg(feature = "native-verified-cell-nodes")]
        let Ok(Some(allocation)) = rows.try_rebuild(count, available) else {
            return false;
        };
        #[cfg(not(feature = "native-verified-cell-nodes"))]
        let allocation = {
            let mut replacement = Rows::with_hasher(rows.hasher().clone());
            if replacement.try_reserve(count).is_err() || replacement.allocation_size() > available
            {
                return false;
            }
            let allocation = replacement.allocation_size();
            replacement.extend(rows.drain());
            *rows = replacement;
            allocation
        };
        let peak = bytes + allocation;
        self.peak_bytes.fetch_max(peak, Ordering::Relaxed);
        self.bytes.store(peak - old, Ordering::Relaxed);
        if old != 0 {
            self.rebuilds.fetch_add(1, Ordering::Relaxed);
        }
        true
    }

    pub(super) fn lookup(&self, id: &Key) -> Option<NodeRead> {
        if !self.active.load(Ordering::Acquire) {
            return None;
        }
        let shard = &self.shards[slot(id)];
        let rows = shard.rows.read().expect("Cell node shard");
        #[cfg(feature = "native-verified-cell-nodes")]
        let result = rows.get(id);
        #[cfg(not(feature = "native-verified-cell-nodes"))]
        let result = rows.get(id).cloned();
        drop(rows);
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
        #[cfg(feature = "native-verified-cell-journal")]
        if let Some((_, journal)) = &batch.cell_nodes {
            let edits = || {
                journal
                    .iter()
                    .map(|(id, node)| (id, node.map(NodeUpdate::Generated)))
            };
            #[cfg(feature = "native-stream-node-publication")]
            self.advance_sorted(edits());
            #[cfg(not(feature = "native-stream-node-publication"))]
            self.advance_edits(edits);
            return;
        }
        let prefix = [STATE_NODE, Namespace::Cells.tag()];
        self.advance_edits(|| {
            batch.writes.iter().filter_map(|(key, value)| {
                let key: &[u8] = key.as_ref();
                let id = <&Key>::try_from(key.strip_prefix(&prefix)?).ok()?;
                Some((id, value.as_deref().map(NodeUpdate::Stored)))
            })
        });
    }

    #[cfg(feature = "native-stream-node-publication")]
    fn advance_sorted<'a>(&self, edits: impl Iterator<Item = (&'a Key, Option<NodeUpdate<'a>>)>) {
        // An ordered content ID range belongs to just one shard. Each
        // generated journal is sorted before it reaches the durable writer.
        let mut edits = edits.peekable();
        #[cfg(feature = "jolteon-resident-node-publication")]
        let mut nodes = self.nodes.load(Ordering::Relaxed);
        while let Some((id, _)) = edits.peek() {
            let index = slot(id);
            let changes = std::iter::from_fn(|| edits.next_if(|(id, _)| slot(id) == index));
            if !self.apply_shard(
                &self.shards[index],
                changes,
                #[cfg(feature = "jolteon-resident-node-publication")]
                &mut nodes,
            ) {
                self.retire();
                return;
            }
        }
        #[cfg(feature = "jolteon-resident-node-publication")]
        self.nodes.store(nodes, Ordering::Relaxed);
    }

    fn advance_edits<'a, Edits>(&self, node_edits: impl Fn() -> Edits)
    where
        Edits: Iterator<Item = (&'a Key, Option<NodeUpdate<'a>>)>,
    {
        #[cfg(feature = "jolteon-resident-node-publication")]
        let mut by_shard = {
            let mut counts = [0; SHARDS];
            for (id, _) in node_edits() {
                counts[slot(id)] += 1;
            }
            counts.map(Vec::with_capacity)
        };
        #[cfg(not(feature = "jolteon-resident-node-publication"))]
        let mut by_shard: [Vec<_>; SHARDS] = std::array::from_fn(|_| Vec::new());
        for (id, value) in node_edits() {
            by_shard[slot(id)].push((id, value));
        }
        #[cfg(feature = "jolteon-resident-node-publication")]
        let mut nodes = self.nodes.load(Ordering::Relaxed);
        for (shard, changes) in self.shards.iter().zip(by_shard) {
            if !changes.is_empty()
                && !self.apply_shard(
                    shard,
                    changes,
                    #[cfg(feature = "jolteon-resident-node-publication")]
                    &mut nodes,
                )
            {
                self.retire();
                return;
            }
        }
        #[cfg(feature = "jolteon-resident-node-publication")]
        self.nodes.store(nodes, Ordering::Relaxed);
    }

    fn apply_shard<'a>(
        &self,
        shard: &Shard,
        changes: impl IntoIterator<Item = (&'a Key, Option<NodeUpdate<'a>>)>,
        #[cfg(feature = "jolteon-resident-node-publication")] nodes: &mut usize,
    ) -> bool {
        let mut rows = shard.rows.write().expect("Cell node shard");
        #[cfg(feature = "jolteon-resident-node-publication")]
        let old_len = rows.len();
        for (id, update) in changes {
            let Some(update) = update else {
                rows.remove(id);
                continue;
            };
            let count = rows.len() + 1;
            let fits = (cfg!(feature = "jolteon-resident-node-publication")
                && count <= rows.capacity())
                || rows.contains_key(id)
                || self.reserve(&mut rows, count);
            if !fits {
                // An uncached content address reads from the caller's durable snapshot.
                self.uncached_inserts.fetch_add(1, Ordering::Relaxed);
            } else if update.apply(&mut rows, id).is_err() {
                return false;
            }
        }
        #[cfg(feature = "jolteon-resident-node-publication")]
        {
            *nodes = *nodes - old_len + rows.len();
        }
        true
    }

    #[cfg(feature = "native-hot-cell-node-cache")]
    fn start_hot(&self, tree: &Patricia, header: Key) -> &hot::HotNodes {
        self.hot
            .get_or_init(|| hot::HotNodes::new(tree.clone(), self.limit_bytes, header))
    }

    #[cfg(feature = "native-hot-cell-node-cache")]
    pub(super) fn advance_hot(&self, tree: &Patricia, batch: &Batch, parent: Key, next: Key) {
        if !self.active.load(Ordering::Acquire) {
            self.start_hot(tree, parent).advance(batch, parent, next);
        }
    }

    #[cfg(feature = "native-hot-cell-node-cache")]
    pub(super) fn lookup_hot(&self, header: &Key, id: &Key) -> Option<NodeRead> {
        self.hot.get()?.lookup(header, id)
    }

    #[cfg(feature = "native-hot-cell-node-cache")]
    pub(super) fn remember_stored(
        &self,
        header: &Key,
        id: &Key,
        stored: &[u8],
    ) -> Result<NodeRead> {
        match self.hot.get() {
            Some(hot) => hot.remember(header, id, stored),
            None => node_storage::read(stored),
        }
    }

    pub(super) fn retire(&self) -> usize {
        #[cfg(feature = "native-hot-cell-node-cache")]
        if let Some(hot) = self.hot.get() {
            hot.retire();
        }
        self.active.store(false, Ordering::Release);
        for shard in &self.shards {
            let mut rows = shard.rows.write().expect("Cell node shard");
            #[cfg(feature = "native-verified-cell-nodes")]
            rows.release();
            #[cfg(not(feature = "native-verified-cell-nodes"))]
            {
                *rows = Rows::default();
            }
        }
        self.bytes.store(0, Ordering::Relaxed);
        #[cfg(feature = "jolteon-resident-node-publication")]
        self.nodes.store(0, Ordering::Relaxed);
        self.limit_bytes
    }

    pub(super) fn status(&self) -> CellNodeIndexStatus {
        #[cfg(feature = "jolteon-resident-node-publication")]
        let nodes = self.nodes.load(Ordering::Relaxed);
        #[cfg(not(feature = "jolteon-resident-node-publication"))]
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
            #[cfg(feature = "native-hot-cell-node-cache")]
            hot_cache: self.hot.get().map(hot::HotNodes::status),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_store::state_key;
    use stem_stake::patricia::Node;

    fn valid_rows(tree: &Patricia, shard: usize) -> impl Iterator<Item = (Key, Vec<u8>)> + '_ {
        (0u64..).filter_map(move |i| {
            let mut key = [0; 32];
            key[..8].copy_from_slice(&i.to_le_bytes());
            let encoded = Node::Leaf {
                key,
                value: [5; 32],
            }
            .encode();
            let id = tree.node_hash(&encoded);
            (slot(&id) == shard).then_some((id, encoded))
        })
    }

    fn fill(index: &NodeIndex, rows: &[(Key, Vec<u8>)]) {
        let mut batch = Batch::default();
        for (id, encoded) in rows {
            batch.put(state_key(STATE_NODE, Namespace::Cells, id), encoded);
        }
        index.advance(&batch);
    }

    #[test]
    fn churn_rebuilds_a_shard_at_fixed_cardinality_and_budget() {
        const NODES: usize = 100;
        let tree = Patricia::new(ckb_types::packed::Byte32::default(), Namespace::Cells);
        let rows: Vec<_> = valid_rows(&tree, 0).take(NODES + 1024).collect();
        let mut index = NodeIndex::new(&tree, 1 << 20);
        fill(&index, &rows[..NODES]);
        let allocation = index.status().accounted_bytes;
        index.limit_bytes = 2 * allocation + 64;
        for ((removed, _), (inserted, encoded)) in rows.iter().zip(rows.iter().skip(NODES)) {
            let mut batch = Batch::default();
            batch.delete(state_key(STATE_NODE, Namespace::Cells, removed));
            batch.put(state_key(STATE_NODE, Namespace::Cells, inserted), encoded);
            index.advance(&batch);
        }
        let status = index.status();
        assert!(status.active && status.shard_rebuilds > 0);
        assert_eq!(status.nodes, NODES);
        assert_eq!(status.uncached_inserts, 0);
        assert_eq!(status.accounted_bytes, allocation);
        assert!(status.peak_accounted_bytes <= status.limit_bytes);
        assert!(index.lookup(&rows.last().unwrap().0).is_some());
        assert!(index.lookup(&rows[0].0).is_none());
    }

    #[test]
    fn budget_exhaustion_preserves_unrelated_entries() {
        let tree = Patricia::new(ckb_types::packed::Byte32::default(), Namespace::Cells);
        let rows: Vec<_> = valid_rows(&tree, 0).take(100).collect();
        let mut index = NodeIndex::new(&tree, 1 << 20);
        fill(&index, &rows);
        index.limit_bytes = index.status().accounted_bytes;
        let miss = valid_rows(&tree, 1).next().unwrap();
        fill(&index, std::slice::from_ref(&miss));
        let status = index.status();
        assert!(status.active);
        assert_eq!(status.nodes, 100);
        assert_eq!(status.uncached_inserts, 1);
        assert!(index.lookup(&rows[99].0).is_some());
        assert!(index.lookup(&miss.0).is_none());
    }
}
