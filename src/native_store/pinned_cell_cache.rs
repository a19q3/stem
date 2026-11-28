//! Share decoded Cell records only among workers reading the same durable Header.

use std::sync::{Arc, Mutex};

use ckb_types::{packed::Byte32, prelude::*};
use stem_ledger::block::Header;

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
}

struct Generation {
    height: u64,
    header: [u8; 32],
    cells: Arc<LiveCache>,
}

impl PinnedCellCache {
    pub(super) fn acquire(&self, header: &Header, network: &Byte32) -> Arc<LiveCache> {
        let height = header.context.height;
        let id = header.id(network).unpack();
        let mut state = self.state.lock().expect("pinned Cell cache");
        if let Some(current) = &state.latest {
            if current.header == id {
                let cells = Arc::clone(&current.cells);
                state.shared_acquisitions = state.shared_acquisitions.saturating_add(1);
                return cells;
            }
            if current.height >= height {
                // A slow worker can open its snapshot before a newer worker
                // selects a generation. Keep the newer bank in that case.
                state.private_acquisitions = state.private_acquisitions.saturating_add(1);
                return Arc::new(LiveCache::pinned());
            }
        }
        let cells = Arc::new(LiveCache::pinned());
        state.latest = Some(Generation {
            height,
            header: id,
            cells: Arc::clone(&cells),
        });
        state.generations = state.generations.saturating_add(1);
        cells
    }

    pub(super) fn advance(&self, prepared: &Prepared, network: &Byte32) {
        let mut state = self.state.lock().expect("pinned Cell cache");
        let Some(current) = &mut state.latest else {
            return;
        };
        if current.header.as_slice() != prepared.parent.as_slice() {
            return;
        }
        let Some(cells) = Arc::get_mut(&mut current.cells) else {
            // An old snapshot may still read or populate this cache.
            // Its view must remain valid after this tip is installed.
            return;
        };
        if !prepared.live.is_empty() {
            cells.invalidate(prepared.live.keys());
        }
        current.height = prepared.next.context.height;
        current.header = prepared.next.id(network).unpack();
        state.carried_forward = state.carried_forward.saturating_add(1);
    }

    pub(super) fn status(&self) -> PinnedCellCacheStatus {
        let state = self.state.lock().expect("pinned Cell cache");
        let generation = state.latest.as_ref();
        let totals = generation.map(|current| current.cells.totals()).unwrap_or_default();
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
        }
    }
}
