//! Bounded, demand-filled backing when the complete Patricia index will not fit.

use std::{
    mem::size_of,
    sync::{
        RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

use super::{Batch, Key, Namespace, NodeRead, Patricia, SHARDS, STATE_NODE, node_storage, slot};
use anyhow::Result;

const MAX_SLOTS_PER_SHARD: usize = 2048;
type CachedNode = (Key, NodeRead);
type Entries = Vec<Option<CachedNode>>;

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct HotCellNodeStatus {
    pub hits: u64,
    pub misses: u64,
    pub rows: usize,
    pub accounted_bytes: usize,
}

struct Shard {
    header: Option<Key>,
    entries: Entries,
    rows: usize,
    allocation_failed: bool,
}

pub(super) struct HotNodes {
    tree: Patricia,
    slots_per_shard: usize,
    shards: [RwLock<Shard>; SHARDS],
    hits: AtomicU64,
    misses: AtomicU64,
}

impl HotNodes {
    pub(super) fn new(tree: Patricia, limit: usize, header: Key) -> Self {
        let metadata = size_of::<Self>();
        let slots_per_shard = (limit.saturating_sub(metadata)
            / (SHARDS * size_of::<Option<CachedNode>>()))
        .min(MAX_SLOTS_PER_SHARD);
        Self {
            tree,
            slots_per_shard,
            shards: std::array::from_fn(|_| {
                RwLock::new(Shard {
                    header: Some(header),
                    entries: Vec::new(),
                    rows: 0,
                    allocation_failed: false,
                })
            }),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    fn position(id: &Key, slots: usize) -> usize {
        let hash = u64::from_le_bytes(id[1..9].try_into().expect("Cell node ID"));
        (hash % slots as u64) as usize
    }

    pub(super) fn lookup(&self, header: &Key, id: &Key) -> Option<NodeRead> {
        let shard = self.shards[slot(id)].read().expect("hot Cell node shard");
        let hit = if shard.header.as_ref() == Some(header) && !shard.entries.is_empty() {
            shard.entries[Self::position(id, shard.entries.len())]
                .as_ref()
                .filter(|(cached_id, _)| cached_id == id)
                .map(|(_, value)| value.clone())
        } else {
            None
        };
        drop(shard);
        if hit.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        } else {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        hit
    }

    pub(super) fn remember(&self, header: &Key, id: &Key, stored: &[u8]) -> Result<NodeRead> {
        let bytes = node_storage::canonical(stored)?;
        let node = NodeRead::authenticate(&self.tree, *id, bytes.as_ref())?;
        let mut shard = self.shards[slot(id)].write().expect("hot Cell node shard");
        if shard.header.as_ref() != Some(header)
            || self.slots_per_shard == 0
            || shard.allocation_failed
        {
            return Ok(node);
        }
        if shard.entries.is_empty() {
            if shard
                .entries
                .try_reserve_exact(self.slots_per_shard)
                .is_err()
            {
                shard.allocation_failed = true;
                return Ok(node);
            }
            shard.entries.resize_with(self.slots_per_shard, || None);
        }
        let position = Self::position(id, shard.entries.len());
        if shard.entries[position]
            .replace((*id, node.clone()))
            .is_none()
        {
            shard.rows += 1;
        }
        Ok(node)
    }

    pub(super) fn advance(&self, batch: &Batch, parent: Key, next: Key) {
        for (shard, deleted) in self.shards.iter().zip(deletions(batch)) {
            let mut shard = shard.write().expect("hot Cell node shard");
            if shard.header != Some(parent) {
                shard.entries.iter_mut().for_each(|entry| *entry = None);
                shard.rows = 0;
            } else if !shard.entries.is_empty() {
                for id in deleted {
                    let position = Self::position(id, shard.entries.len());
                    if shard.entries[position]
                        .as_ref()
                        .is_some_and(|entry| &entry.0 == id)
                    {
                        shard.entries[position] = None;
                        shard.rows -= 1;
                    }
                }
            }
            shard.header = Some(next);
        }
    }

    pub(super) fn retire(&self) {
        for shard in &self.shards {
            let mut shard = shard.write().expect("hot Cell node shard");
            shard.header = None;
            shard.entries = Vec::new();
            shard.rows = 0;
        }
    }

    pub(super) fn status(&self) -> HotCellNodeStatus {
        let (rows, accounted_bytes) =
            self.shards
                .iter()
                .fold((0, size_of::<Self>()), |(rows, bytes), shard| {
                    let shard = shard.read().expect("hot Cell node shard");
                    (
                        rows + shard.rows,
                        bytes + shard.entries.capacity() * size_of::<Option<CachedNode>>(),
                    )
                });
        HotCellNodeStatus {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            rows,
            accounted_bytes,
        }
    }
}

fn deletions(batch: &Batch) -> [Vec<&Key>; SHARDS] {
    let mut deleted: [Vec<&Key>; SHARDS] = std::array::from_fn(|_| Vec::new());
    #[cfg(feature = "native-verified-cell-journal")]
    if let Some((_, journal)) = &batch.cell_nodes {
        for (id, value) in journal.iter() {
            if value.is_none() {
                deleted[slot(id)].push(id);
            }
        }
        return deleted;
    }
    let prefix = [STATE_NODE, Namespace::Cells.tag()];
    for (key, value) in batch.writes.iter() {
        let key: &[u8] = key.as_ref();
        if value.is_none()
            && let Some(id) = key
                .strip_prefix(&prefix)
                .and_then(|id| <&Key>::try_from(id).ok())
        {
            deleted[slot(id)].push(id);
        }
    }
    deleted
}

#[cfg(test)]
mod tests {
    use ckb_types::prelude::*;
    use stem_stake::patricia::{Commitment, Node};

    use super::*;
    use crate::native_store::state_key;

    #[test]
    fn carried_nodes_keep_their_exact_view_and_late_reads_cannot_reinsert_deletions() -> Result<()>
    {
        let network = [6; 32];
        let tree = Patricia::new(network.pack(), Namespace::Cells);
        let commitment = Commitment::new(network, Namespace::Cells);
        let encoded = |value| {
            let bytes = Node::Leaf {
                key: [value; 32],
                value: [1; 32],
            }
            .encode();
            (commitment.node_hash(&bytes), bytes)
        };
        let (kept, kept_bytes) = encoded(2);
        let (removed, removed_bytes) = (3..=u8::MAX)
            .map(encoded)
            .find(|(id, _)| slot(id) != slot(&kept))
            .expect("a different cache shard");
        let (old, new) = ([4; 32], [5; 32]);
        let limit = 1 << 20;
        let cache = HotNodes::new(tree, limit, old);
        cache.remember(&old, &kept, &kept_bytes)?;
        cache.remember(&old, &removed, &removed_bytes)?;
        assert!(cache.lookup(&old, &kept).is_some());
        assert!(cache.lookup(&old, &removed).is_some());
        assert!(cache.remember(&old, &removed, &kept_bytes).is_err());
        assert!(cache.lookup(&new, &kept).is_none());

        let mut batch = Batch::default();
        batch.delete(state_key(STATE_NODE, Namespace::Cells, &removed));
        cache.advance(&batch, old, new);
        assert!(cache.lookup(&new, &kept).is_some());
        assert!(cache.lookup(&new, &removed).is_none());
        assert!(cache.lookup(&old, &kept).is_none());
        cache.remember(&old, &removed, &removed_bytes)?;
        assert!(cache.lookup(&new, &removed).is_none());
        assert!(cache.status().accounted_bytes <= limit);

        cache.retire();
        assert_eq!(cache.status().rows, 0);
        assert!(cache.lookup(&new, &kept).is_none());
        Ok(())
    }
}
