//! Bounded ordinary Cell dependency reads shared by one execution window or run.

use std::{
    borrow::Borrow,
    cell::Cell,
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use ckb_types::{core::DepType, packed::OutPoint};
use rayon::prelude::*;
use stem_ledger::{LiveCell, Transaction};

use super::{
    super::{DependencyReadStats, LiveCache},
    Store,
};

pub(super) const SAMPLE_TRANSACTIONS: usize = 64;
const SAMPLE_DECLARATIONS: usize = 1024;
const MAX_HINTS: usize = 32;
const MAX_RECORD_BYTES: usize = 128 * 1024;

#[derive(Default)]
struct Frequency {
    uses: usize,
    group: bool,
}

type HintCells = HashMap<OutPoint, Option<LiveCell>>;
type DirectHints = Vec<(OutPoint, Frequency)>;

pub(super) struct HintSet {
    cells: HintCells,
    #[cfg(all(
        feature = "native-rolling-dependency-hints",
        any(feature = "native-rolling-cell-execution", feature = "native-rolling-leader-execution")
    ))]
    direct: DirectHints,
}

#[cfg(feature = "native-rolling-dependency-hints")]
pub(super) type HintSetRef = std::sync::Arc<HintSet>;
#[cfg(not(feature = "native-rolling-dependency-hints"))]
type HintSetRef = HintSet;

pub(in crate::native_store) struct DependencyReads {
    hints: HintSetRef,
    hits: AtomicU64,
    stats: DependencyReadStats,
}

pub(super) struct HintReader<'a> {
    hints: &'a HintSet,
    total_hits: &'a AtomicU64,
    hits: Cell<u64>,
}

impl HintReader<'_> {
    pub(super) fn get(&self, point: &OutPoint) -> Option<&Option<LiveCell>> {
        let record = self.hints.cells.get(point)?;
        self.hits.set(self.hits.get() + 1);
        Some(record)
    }
}

impl Drop for HintReader<'_> {
    fn drop(&mut self) {
        let hits = self.hits.get();
        if hits != 0 {
            self.total_hits.fetch_add(hits, Ordering::Relaxed);
        }
    }
}

impl DependencyReads {
    pub(super) fn collect<T: Borrow<Transaction>>(
        store: &Store,
        transactions: impl IntoIterator<Item = T>,
    ) -> Self {
        let start = Instant::now();
        let (hints, mut stats) = Self::build(store, Self::plan(transactions), None);
        stats.build_us = Self::elapsed_us(start);
        Self {
            hints: HintSetRef::from(hints),
            hits: AtomicU64::new(0),
            stats,
        }
    }

    fn plan<T: Borrow<Transaction>>(transactions: impl IntoIterator<Item = T>) -> DirectHints {
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
        shared
    }

    fn build(
        store: &Store,
        direct: DirectHints,
        previous: Option<&HintSet>,
    ) -> (HintSet, DependencyReadStats) {
        let mut planned: HashSet<_> = direct.iter().map(|(id, _)| id.clone()).collect();
        let points: Vec<_> = direct.iter().map(|(id, _)| id).collect();
        let mut cells = Self::load(store, &points, previous);
        let mut members = Vec::new();
        for (id, _) in direct.iter().filter(|(_, entry)| entry.group) {
            if planned.len() == MAX_HINTS {
                break;
            }
            let Some(Some(record)) = cells.get(id) else {
                continue;
            };
            let Ok(outpoints) = record.cell.dep_group_members() else {
                continue;
            };
            // These are lookup hints. The execution resolver will expand the
            // group at its actual transaction position and verify every member.
            if outpoints.len() > MAX_HINTS {
                continue;
            }
            for point in outpoints {
                if planned.insert(point.clone()) {
                    members.push(point);
                    if planned.len() == MAX_HINTS {
                        break;
                    }
                }
            }
        }
        let indirect: Vec<_> = members.iter().collect();
        cells.extend(Self::load(store, &indirect, previous));

        let stats = DependencyReadStats {
            planned: planned.len() as u64,
            cached: cells.len() as u64,
            bytes: cells
                .values()
                .flatten()
                .map(|cell| LiveCache::record_bytes(cell) as u64)
                .sum(),
            ..DependencyReadStats::default()
        };
        let hints = HintSet {
            cells,
            #[cfg(all(
                feature = "native-rolling-dependency-hints",
                any(feature = "native-rolling-cell-execution", feature = "native-rolling-leader-execution")
            ))]
            direct,
        };
        (hints, stats)
    }

    fn load(store: &Store, points: &[&OutPoint], previous: Option<&HintSet>) -> HintCells {
        let load = |&point: &&OutPoint| {
            previous
                .and_then(|hints| hints.cells.get(point).cloned())
                .or_else(|| store.prefetch_cell(point, MAX_RECORD_BYTES))
                .map(|cell| (point.clone(), cell))
        };
        if points.len() >= 4 {
            points.par_iter().filter_map(load).collect()
        } else {
            points.iter().filter_map(load).collect()
        }
    }

    fn elapsed_us(start: Instant) -> u64 {
        u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    #[cfg(all(
        feature = "native-rolling-dependency-hints",
        any(feature = "native-rolling-cell-execution", feature = "native-rolling-leader-execution")
    ))]
    pub(super) fn snapshot(&self) -> HintSetRef {
        HintSetRef::clone(&self.hints)
    }

    #[cfg(all(
        feature = "native-rolling-dependency-hints",
        any(feature = "native-rolling-cell-execution", feature = "native-rolling-leader-execution")
    ))]
    pub(super) fn refresh(
        store: &Store,
        transactions: &[Transaction],
        previous: &mut HintSetRef,
        totals: &mut DependencyReadStats,
    ) {
        let start = Instant::now();
        totals.rolling_samples += 1;
        let direct = Self::plan(transactions);
        let same_plan = previous.direct.len() == direct.len()
            && previous.direct.iter().zip(&direct).all(|((old, before), (new, after))| {
                old == new && before.group == after.group
            });
        if !same_plan {
            let (hints, stats) = Self::build(store, direct, Some(previous));
            let carried = hints.cells.keys()
                .filter(|point| previous.cells.contains_key(*point))
                .count();
            totals.rolling_carried += carried as u64;
            totals.rolling_rebuilds += 1;
            totals.record(stats);
            *previous = HintSetRef::new(hints);
        }
        totals.build_us = totals.build_us.saturating_add(Self::elapsed_us(start));
    }

    #[cfg(all(
        feature = "native-rolling-dependency-hints",
        any(feature = "native-rolling-cell-execution", feature = "native-rolling-leader-execution")
    ))]
    pub(super) fn set_snapshot(&mut self, hints: HintSetRef, stats: DependencyReadStats) {
        self.hints = hints;
        self.stats.record(stats);
    }

    pub(super) fn reader(&self) -> HintReader<'_> {
        self.reader_from(&self.hints)
    }

    pub(super) fn reader_from<'a>(&'a self, hints: &'a HintSet) -> HintReader<'a> {
        HintReader {
            hints,
            total_hits: &self.hits,
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
