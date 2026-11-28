//! Share decoded Cell records only among workers reading the same durable Header.

use std::sync::{Arc, Mutex};

#[cfg(feature = "native-pinned-cell-cache-inheritance")]
use std::{
    collections::HashSet,
    sync::atomic::{AtomicU64, Ordering},
};

use ckb_types::{packed::Byte32, prelude::*};
use stem_ledger::block::Header;
#[cfg(feature = "native-pinned-cell-cache-inheritance")]
use {ckb_types::packed::OutPoint, stem_ledger::LiveCell};

use super::{LiveCache, PinnedCellCacheStatus, Prepared};

#[derive(Default)]
pub(super) struct PinnedCellCache {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    latest: Option<Generation>,
    shared_acquisitions: u64,
    private_acquisitions: u64,
    generations: u64,
    carried_forward: u64,
    inherited_generations: u64,
}

struct Generation {
    height: u64,
    header: [u8; 32],
    cache: SnapshotCache,
}

#[derive(Clone)]
pub(super) struct SnapshotCache {
    pub(super) cells: Arc<LiveCache>,
    #[cfg(feature = "native-pinned-cell-cache-inheritance")]
    pub(super) previous: Option<Arc<PreviousRows>>,
}

impl SnapshotCache {
    fn empty() -> Self {
        Self {
            cells: Arc::new(LiveCache::pinned()),
            #[cfg(feature = "native-pinned-cell-cache-inheritance")]
            previous: None,
        }
    }
}

#[cfg(feature = "native-pinned-cell-cache-inheritance")]
pub(super) struct PreviousRows {
    cells: Arc<LiveCache>,
    changed: HashSet<[u8; OutPoint::TOTAL_SIZE]>,
    hits: AtomicU64,
}

#[cfg(feature = "native-pinned-cell-cache-inheritance")]
impl PreviousRows {
    fn new(cells: &Arc<LiveCache>, transition: &Prepared) -> Option<Self> {
        const MAX_CHANGES: usize = 16_384;
        if transition.live.len() > MAX_CHANGES {
            return None;
        }
        let mut changed = HashSet::new();
        changed.try_reserve(transition.live.len()).ok()?;
        for point in transition.live.keys() {
            changed.insert(point.as_slice().try_into().ok()?);
        }
        Some(Self {
            cells: Arc::clone(cells),
            changed,
            hits: AtomicU64::new(0),
        })
    }

    pub(super) fn lookup(&self, point: &OutPoint) -> Option<Option<LiveCell>> {
        let key: &[u8; OutPoint::TOTAL_SIZE] = point.as_slice().try_into().ok()?;
        if self.changed.contains(key) {
            return None;
        }
        let row = self.cells.lookup(point)?;
        self.hits.fetch_add(1, Ordering::Relaxed);
        Some(row)
    }
}

impl State {
    fn start_generation(
        &mut self,
        header: &Header,
        id: [u8; 32],
        _transition: Option<&Prepared>,
    ) -> SnapshotCache {
        let cache = SnapshotCache::empty();
        #[cfg(feature = "native-pinned-cell-cache-inheritance")]
        let cache = {
            let mut cache = cache;
            if let Some(transition) = _transition
                && transition.header() == header
                && let Some(current) = &self.latest
                && current.header.as_slice() == transition.parent.as_slice()
                && current.height.checked_add(1) == Some(header.context.height)
            {
                cache.previous = PreviousRows::new(&current.cache.cells, transition).map(Arc::new);
                if cache.previous.is_some() {
                    self.inherited_generations = self.inherited_generations.saturating_add(1);
                }
            }
            cache
        };
        self.latest = Some(Generation {
            height: header.context.height,
            header: id,
            cache: cache.clone(),
        });
        self.generations = self.generations.saturating_add(1);
        cache
    }
}

impl PinnedCellCache {
    pub(super) fn acquire(
        &self,
        header: &Header,
        network: &Byte32,
        transition: Option<&Prepared>,
    ) -> SnapshotCache {
        let height = header.context.height;
        let id = header.id(network).unpack();
        let mut state = self.state.lock().expect("pinned Cell cache");
        if let Some(current) = &state.latest {
            if current.header == id {
                let cache = current.cache.clone();
                state.shared_acquisitions = state.shared_acquisitions.saturating_add(1);
                return cache;
            }
            if current.height >= height {
                // A slow worker can open its snapshot before a newer worker
                // selects a generation. Keep the newer bank in that case.
                state.private_acquisitions = state.private_acquisitions.saturating_add(1);
                return SnapshotCache::empty();
            }
        }
        state.start_generation(header, id, transition)
    }

    pub(super) fn advance(&self, prepared: &Prepared, network: &Byte32) {
        let mut state = self.state.lock().expect("pinned Cell cache");
        let Some(current) = &mut state.latest else {
            return;
        };
        if current.header.as_slice() != prepared.parent.as_slice() {
            return;
        }
        let Some(cells) = Arc::get_mut(&mut current.cache.cells) else {
            // Let an old reader finish with its own bank. The successor can
            // borrow its untouched rows without mutating that bank.
            #[cfg(feature = "native-pinned-cell-cache-inheritance")]
            state.start_generation(
                &prepared.next,
                prepared.next.id(network).unpack(),
                Some(prepared),
            );
            return;
        };
        if !prepared.live.is_empty() {
            cells.invalidate(prepared.live.keys());
        }
        #[cfg(feature = "native-pinned-cell-cache-inheritance")]
        {
            current.cache.previous = None;
        }
        current.height = prepared.next.context.height;
        current.header = prepared.next.id(network).unpack();
        state.carried_forward = state.carried_forward.saturating_add(1);
    }

    pub(super) fn status(&self) -> PinnedCellCacheStatus {
        let state = self.state.lock().expect("pinned Cell cache");
        let generation = state.latest.as_ref();
        let totals = generation
            .map(|current| current.cache.cells.totals())
            .unwrap_or_default();
        #[cfg(feature = "native-pinned-cell-cache-inheritance")]
        let inheritance_hits = generation
            .and_then(|current| current.cache.previous.as_ref())
            .map_or(0, |previous| previous.hits.load(Ordering::Relaxed));
        #[cfg(not(feature = "native-pinned-cell-cache-inheritance"))]
        let inheritance_hits = 0;
        PinnedCellCacheStatus {
            retained_height: generation.map(|current| current.height),
            entries: totals.entries,
            record_bytes: totals.record_bytes,
            shared_acquisitions: state.shared_acquisitions,
            private_acquisitions: state.private_acquisitions,
            generations: state.generations,
            carried_forward: state.carried_forward,
            shards: LiveCache::SHARDS,
            capacity_flushes: totals.capacity_flushes,
            absence_entries: totals.absence_entries,
            absence_hits: totals.absence_hits,
            inherited_generations: state.inherited_generations,
            inheritance_hits,
        }
    }
}
