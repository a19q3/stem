//! Bounded decoded records from an exact durable Cell view.

use std::{collections::HashMap, sync::Mutex};

#[cfg(feature = "native-sharded-live-cache")]
use std::hash::{BuildHasher, RandomState};

use ckb_types::packed::OutPoint;
#[cfg(feature = "native-sharded-live-cache")]
use ckb_types::prelude::Entity;
use stem_ledger::{LiveCell, Origin};

const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ITEMS: usize = 4096;
const SHARDS: usize = if cfg!(feature = "native-sharded-live-cache") {
    8
} else {
    1
};
const SHARD_BYTES: usize = MAX_BYTES / SHARDS;
const SHARD_ITEMS: usize = MAX_ITEMS / SHARDS;

#[derive(Default)]
struct Shard {
    records: HashMap<OutPoint, LiveCell>,
    bytes: usize,
    capacity_flushes: u64,
}

impl Shard {
    fn clear(&mut self) {
        self.records.clear();
        self.bytes = 0;
    }
}

#[derive(Default)]
pub(super) struct LiveCache {
    shards: [Mutex<Shard>; SHARDS],
    #[cfg(feature = "native-sharded-live-cache")]
    shard_hash: RandomState,
}

impl LiveCache {
    #[cfg(feature = "native-shared-pinned-cell-cache")]
    pub(super) const SHARDS: usize = SHARDS;

    fn shard_for(&self, _point: &OutPoint) -> usize {
        #[cfg(feature = "native-sharded-live-cache")]
        return self.shard_hash.hash_one(_point.as_slice()) as usize % SHARDS;
        #[cfg(not(feature = "native-sharded-live-cache"))]
        0
    }

    pub(super) fn get(&self, point: &OutPoint) -> Option<LiveCell> {
        self.shards[self.shard_for(point)]
            .lock()
            .expect("Live Cell cache shard")
            .records
            .get(point)
            .cloned()
    }

    pub(super) fn insert(&self, point: OutPoint, cell: LiveCell) {
        let bytes = Self::record_bytes(&cell);
        if bytes > SHARD_BYTES {
            return;
        }
        let mut shard = self.shards[self.shard_for(&point)]
            .lock()
            .expect("Live Cell cache shard");
        if shard.records.contains_key(&point) {
            return;
        }
        if shard.bytes + bytes > SHARD_BYTES || shard.records.len() == SHARD_ITEMS {
            shard.clear();
            shard.capacity_flushes = shard.capacity_flushes.saturating_add(1);
        }
        shard.bytes += bytes;
        shard.records.insert(point, cell);
    }

    pub(super) fn invalidate<'a>(&self, points: impl Iterator<Item = &'a OutPoint>) {
        // Hold all shards through the committed transition. Readers of older
        // pinned Headers keep their own generation.
        let mut shards = self
            .shards
            .each_ref()
            .map(|shard| shard.lock().expect("Live Cell cache shard"));
        if shards.iter().all(|shard| shard.records.is_empty()) {
            return;
        }
        for point in points {
            let shard = &mut shards[self.shard_for(point)];
            if let Some(cell) = shard.records.remove(point) {
                shard.bytes -= Self::record_bytes(&cell);
            }
        }
    }

    pub(super) fn clear(&self) {
        let mut shards = self
            .shards
            .each_ref()
            .map(|shard| shard.lock().expect("Live Cell cache shard"));
        for shard in &mut shards {
            shard.clear();
        }
    }

    pub(super) fn record_bytes(cell: &LiveCell) -> usize {
        cell.cell.as_slice().len() + Origin::LEN + OutPoint::TOTAL_SIZE
    }

    #[cfg(feature = "native-shared-pinned-cell-cache")]
    pub(super) fn totals(&self) -> (usize, usize, u64) {
        self.shards
            .iter()
            .fold((0, 0, 0), |(entries, bytes, flushes), shard| {
                let shard = shard.lock().expect("Live Cell cache shard");
                (
                    entries + shard.records.len(),
                    bytes + shard.bytes,
                    flushes.saturating_add(shard.capacity_flushes),
                )
            })
    }
}

#[cfg(all(test, feature = "native-sharded-live-cache"))]
mod tests {
    use super::*;
    use ckb_types::{packed::Script, prelude::*};
    use stem_ledger::Cell;

    #[test]
    fn capacity_flush_and_invalidation_are_local_to_the_selected_shard() -> anyhow::Result<()> {
        let cell = LiveCell {
            cell: Cell::new(
                20_000_000_000,
                Script::default(),
                vec![],
                Default::default(),
            )?,
            origin: Origin {
                height: 0,
                timestamp_ms: 0,
                context_id: Default::default(),
            },
        };
        let cache = LiveCache::default();
        let point = |index| OutPoint::new([7; 32].pack(), index);
        let sentinel = point(0);
        let preserved_shard = cache.shard_for(&sentinel);
        let overloaded_shard = (preserved_shard + 1) % SHARDS;
        cache.insert(sentinel.clone(), cell.clone());
        let collisions: Vec<_> = (1..)
            .map(point)
            .filter(|point| cache.shard_for(point) == overloaded_shard)
            .take(SHARD_ITEMS + 1)
            .collect();
        for point in &collisions {
            cache.insert(point.clone(), cell.clone());
        }
        let last = collisions.last().expect("overloaded shard entries");
        assert!(cache.get(&collisions[0]).is_none());
        assert!(cache.get(last).is_some());
        assert!(cache.get(&sentinel).is_some());
        assert_eq!(
            cache.shards[overloaded_shard]
                .lock()
                .unwrap()
                .capacity_flushes,
            1
        );
        assert_eq!(
            cache.shards[overloaded_shard].lock().unwrap().bytes,
            LiveCache::record_bytes(&cell)
        );

        cache.invalidate(std::iter::once(last));
        assert!(cache.get(last).is_none());
        assert!(cache.get(&sentinel).is_some());
        assert_eq!(cache.shards[overloaded_shard].lock().unwrap().bytes, 0);
        cache.clear();
        assert!(cache.get(&sentinel).is_none());
        Ok(())
    }
}
