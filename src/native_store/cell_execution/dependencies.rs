//! Bounded ordinary Cell dependency reads shared by one execution window or run.

use std::{
    borrow::Borrow,
    cell::Cell,
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use ckb_types::{
    core::DepType,
    packed::{OutPoint, OutPointVecReader},
    prelude::*,
};
use rayon::prelude::*;
use stem_ledger::{LiveCell, Transaction};

use super::{
    super::{DependencyReadStats, LiveCache},
    Store,
};

const SAMPLE_TRANSACTIONS: usize = 64;
const SAMPLE_DECLARATIONS: usize = 1024;
const MAX_HINTS: usize = 32;
const MAX_RECORD_BYTES: usize = 128 * 1024;

#[derive(Default)]
struct Frequency {
    uses: usize,
    group: bool,
}

pub(in crate::native_store) struct DependencyReads {
    cells: HashMap<OutPoint, Option<LiveCell>>,
    hits: AtomicU64,
    stats: DependencyReadStats,
}

pub(super) struct HintReader<'a> {
    reads: &'a DependencyReads,
    hits: Cell<u64>,
}

impl HintReader<'_> {
    pub(super) fn get(&self, point: &OutPoint) -> Option<&Option<LiveCell>> {
        let record = self.reads.cells.get(point)?;
        self.hits.set(self.hits.get() + 1);
        Some(record)
    }
}

impl Drop for HintReader<'_> {
    fn drop(&mut self) {
        let hits = self.hits.get();
        if hits != 0 {
            self.reads.hits.fetch_add(hits, Ordering::Relaxed);
        }
    }
}

impl DependencyReads {
    pub(super) fn collect<T: Borrow<Transaction>>(
        store: &Store,
        transactions: impl IntoIterator<Item = T>,
    ) -> Self {
        let start = Instant::now();
        let mut frequency: HashMap<OutPoint, Frequency> = HashMap::new();
        let mut remaining = SAMPLE_DECLARATIONS;
        for tx in transactions.into_iter().take(SAMPLE_TRANSACTIONS) {
            let deps = tx.borrow().cell_deps().take(remaining);
            for dep in deps {
                let entry = frequency.entry(dep.out_point()).or_default();
                entry.uses += 1;
                entry.group |= matches!(DepType::try_from(dep.dep_type()), Ok(DepType::DepGroup));
                remaining -= 1;
            }
            if remaining == 0 {
                break;
            }
        }
        let mut shared: Vec<_> = frequency
            .into_iter()
            .filter(|(_, entry)| entry.uses > 1)
            .collect();
        shared.sort_unstable_by(|(point, entry), (other, frequency)| {
            frequency
                .uses
                .cmp(&entry.uses)
                .then_with(|| point.cmp(other))
        });
        shared.truncate(MAX_HINTS);

        let mut planned: HashSet<_> = shared.iter().map(|(id, _)| id.clone()).collect();
        let direct: Vec<_> = shared.iter().map(|(id, _)| id).collect();
        let mut cells = Self::load(store, &direct);
        let mut members = Vec::new();
        for (id, _) in shared.iter().filter(|(_, entry)| entry.group) {
            if planned.len() == MAX_HINTS {
                break;
            }
            let Some(Some(record)) = cells.get(id) else {
                continue;
            };
            let data = record.cell.data();
            let Ok(outpoints) = OutPointVecReader::from_slice(&data) else {
                continue;
            };
            // These are lookup hints. The execution resolver will expand the
            // group at its actual transaction position and verify every member.
            if outpoints.len() > MAX_HINTS {
                continue;
            }
            for member in outpoints.iter() {
                let point = member.to_entity();
                if planned.insert(point.clone()) {
                    members.push(point);
                    if planned.len() == MAX_HINTS {
                        break;
                    }
                }
            }
        }
        let indirect: Vec<_> = members.iter().collect();
        cells.extend(Self::load(store, &indirect));

        let stats = DependencyReadStats {
            planned: planned.len() as u64,
            cached: cells.len() as u64,
            bytes: cells
                .values()
                .flatten()
                .map(|cell| LiveCache::record_bytes(cell) as u64)
                .sum(),
            hits: 0,
            build_us: u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX),
        };
        Self {
            cells,
            hits: AtomicU64::new(0),
            stats,
        }
    }

    fn load(store: &Store, points: &[&OutPoint]) -> HashMap<OutPoint, Option<LiveCell>> {
        let load = |&point: &&OutPoint| {
            store
                .prefetch_cell(point, MAX_RECORD_BYTES)
                .map(|cell| (point.clone(), cell))
        };
        if points.len() >= 4 {
            points.par_iter().filter_map(load).collect()
        } else {
            points.iter().filter_map(load).collect()
        }
    }

    pub(super) fn reader(&self) -> HintReader<'_> {
        HintReader {
            reads: self,
            hits: Cell::new(0),
        }
    }

    pub(in crate::native_store) fn stats(&self) -> DependencyReadStats {
        DependencyReadStats {
            hits: self.hits.load(Ordering::Relaxed),
            ..self.stats
        }
    }
}
