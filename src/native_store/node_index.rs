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
    #[cfg(feature = "jolteon-resident-node-publication")]
    nodes: AtomicUsize,
    limit_bytes: usize,
    recovery_us: u64,
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
    pub(super) fn recover(store: &Store, limit_bytes: usize) -> Result<Self> {
        let started = Instant::now();
        let mut index = Self {
            shards: std::array::from_fn(|_| Shard::empty(&store.cells)),
            active: AtomicBool::new(true),
            bytes: AtomicUsize::new(0),
            #[cfg(feature = "jolteon-resident-node-publication")]
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
    fn advance_sorted<'a>(
        &self,
        edits: impl Iterator<Item = (&'a Key, Option<NodeUpdate<'a>>)>,
    ) {
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
            if !fits || update.apply(&mut rows, id).is_err() {
                return false;
            }
        }
        #[cfg(feature = "jolteon-resident-node-publication")]
        {
            *nodes = *nodes - old_len + rows.len();
        }
        true
    }

    pub(super) fn retire(&self) -> usize {
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
        }
    }
}
