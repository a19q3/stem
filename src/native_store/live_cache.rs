//! Bounded decoded records from an exact durable Cell view.

use std::{collections::HashMap, sync::Mutex};

#[cfg(feature = "native-sharded-live-cache")]
use std::hash::{BuildHasher, RandomState};

use ckb_types::packed::OutPoint;
#[cfg(feature = "native-sharded-live-cache")]
use ckb_types::prelude::Entity;
use stem_ledger::{LiveCell, Origin};

#[cfg(feature = "native-pinned-cell-absence-cache")]
mod absence;

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
    #[cfg(feature = "native-pinned-cell-absence-cache")]
    absences: absence::Absences,
}

impl Shard {
    fn is_empty(&self) -> bool {
        #[cfg(feature = "native-pinned-cell-absence-cache")]
        if self.absences.entries != 0 {
            return false;
        }
        self.records.is_empty()
    }

    fn clear(&mut self) {
        self.records.clear();
        self.bytes = 0;
    }
}

#[derive(Default)]
pub(super) struct LiveCache {
    shards: [Mutex<Shard>; SHARDS],
    #[cfg(feature = "native-pinned-cell-absence-cache")]
    pinned: bool,
    #[cfg(feature = "native-sharded-live-cache")]
    shard_hash: RandomState,
}

#[cfg(feature = "native-shared-pinned-cell-cache")]
#[derive(Default)]
pub(super) struct CacheTotals {
    pub(super) entries: usize,
    pub(super) record_bytes: usize,
    pub(super) capacity_flushes: u64,
    pub(super) absence_entries: usize,
    pub(super) absence_hits: u64,
}

impl LiveCache {
    #[cfg(feature = "native-shared-pinned-cell-cache")]
    pub(super) fn pinned() -> Self {
        Self {
            #[cfg(feature = "native-pinned-cell-absence-cache")]
            pinned: true,
            ..Self::default()
        }
    }

    #[cfg(feature = "native-shared-pinned-cell-cache")]
    pub(super) const SHARDS: usize = SHARDS;

    fn shard_for(&self, _point: &OutPoint) -> usize {
        #[cfg(feature = "native-sharded-live-cache")]
        return self.shard_hash.hash_one(_point.as_slice()) as usize % SHARDS;
        #[cfg(not(feature = "native-sharded-live-cache"))]
        0
    }

    pub(super) fn lookup(&self, point: &OutPoint) -> Option<Option<LiveCell>> {
        let shard = self.shards[self.shard_for(point)].lock().expect("Live Cell cache shard");
        if let Some(cell) = shard.records.get(point) {
            return Some(Some(cell.clone()));
        }
        #[cfg(feature = "native-pinned-cell-absence-cache")]
        if self.pinned {
            let mut shard = shard;
            return shard.absences.contains(point).then_some(None);
        }
        None
    }

    #[cfg(feature = "native-pinned-cell-absence-cache")]
    pub(super) fn remember_absence(&self, point: &OutPoint) {
        if self.pinned {
            let mut shard = self.shards[self.shard_for(point)].lock().expect("Live Cell cache shard");
            shard.absences.insert(point);
        }
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
        if shards.iter().all(|shard| shard.is_empty()) {
            return;
        }
        for point in points {
            let shard = &mut shards[self.shard_for(point)];
            #[cfg(feature = "native-pinned-cell-absence-cache")]
            shard.absences.remove(point);
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
            #[cfg(feature = "native-pinned-cell-absence-cache")]
            {
                shard.absences = absence::Absences::default();
            }
        }
    }

    pub(super) fn record_bytes(cell: &LiveCell) -> usize {
        cell.cell.as_slice().len() + Origin::LEN + OutPoint::TOTAL_SIZE
    }

    #[cfg(feature = "native-shared-pinned-cell-cache")]
    pub(super) fn totals(&self) -> CacheTotals {
        let mut totals = CacheTotals::default();
        for shard in &self.shards {
            let shard = shard.lock().expect("Live Cell cache shard");
            totals.entries += shard.records.len();
            totals.record_bytes += shard.bytes;
            totals.capacity_flushes = totals.capacity_flushes.saturating_add(shard.capacity_flushes);
            #[cfg(feature = "native-pinned-cell-absence-cache")]
            {
                totals.absence_entries += shard.absences.entries;
                totals.absence_hits = totals.absence_hits.saturating_add(shard.absences.hits);
            }
        }
        totals
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
        assert!(cache.lookup(&collisions[0]).is_none());
        assert!(cache.lookup(last).is_some());
        assert!(cache.lookup(&sentinel).is_some());
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
        assert!(cache.lookup(last).is_none());
        assert!(cache.lookup(&sentinel).is_some());
        assert_eq!(cache.shards[overloaded_shard].lock().unwrap().bytes, 0);
        cache.clear();
        assert!(cache.lookup(&sentinel).is_none());
        Ok(())
    }
}
