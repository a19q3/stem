//! Compact, immutable Cell changes shared by all readers of one candidate.

#[cfg(feature = "native-pinned-cell-cache-inheritance")]
use std::sync::Arc;

use ckb_types::{packed::OutPoint, prelude::*};
use rayon::prelude::*;
use stem_ledger::LiveCell;

use super::LiveChanges;
#[cfg(not(feature = "native-pinned-cell-cache-inheritance"))]
use super::cell_change_index::CellPositions;
#[cfg(feature = "native-pinned-cell-cache-inheritance")]
use super::cell_change_index::ChangedOutPoints;

type Entry = (OutPoint, Option<LiveCell>);

pub(super) struct PreparedCells {
    ordered: Vec<Entry>,
    #[cfg(not(feature = "native-pinned-cell-cache-inheritance"))]
    positions: Option<CellPositions>,
    #[cfg(feature = "native-pinned-cell-cache-inheritance")]
    changed: Option<Arc<ChangedOutPoints>>,
}

impl From<LiveChanges> for PreparedCells {
    fn from(changes: LiveChanges) -> Self {
        Self {
            ordered: changes.into_iter().collect(),
            #[cfg(not(feature = "native-pinned-cell-cache-inheritance"))]
            positions: None,
            #[cfg(feature = "native-pinned-cell-cache-inheritance")]
            changed: None,
        }
    }
}

impl PreparedCells {
    pub(super) fn with_index(mut self) -> Self {
        #[cfg(feature = "native-pinned-cell-cache-inheritance")]
        {
            self.changed = ChangedOutPoints::build(&self);
        }
        #[cfg(not(feature = "native-pinned-cell-cache-inheritance"))]
        if self.len() >= 128 {
            self.positions = CellPositions::build(self.len(), |position| {
                point_bytes(&self.ordered[position].0)
            });
        }
        self
    }

    #[cfg(feature = "native-pinned-cell-cache-inheritance")]
    pub(super) fn changed_outpoints(&self) -> Option<&Arc<ChangedOutPoints>> {
        self.changed.as_ref()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.ordered.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.ordered.len()
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&OutPoint, &Option<LiveCell>)> {
        self.ordered.iter().map(|(point, cell)| (point, cell))
    }

    pub(super) fn par_iter(
        &self,
    ) -> impl IndexedParallelIterator<Item = (&OutPoint, &Option<LiveCell>)> {
        self.ordered.par_iter().map(|(point, cell)| (point, cell))
    }

    #[cfg(feature = "native-prepared-live-cell-ownership")]
    pub(super) fn values_mut(&mut self) -> impl Iterator<Item = &mut Option<LiveCell>> {
        self.ordered.iter_mut().map(|(_, cell)| cell)
    }

    pub(super) fn keys(&self) -> impl Iterator<Item = &OutPoint> {
        self.ordered.iter().map(|(point, _)| point)
    }

    pub(super) fn get_hashed(
        &self,
        point: &OutPoint,
        hash_once: &mut Option<u64>,
    ) -> Option<&Option<LiveCell>> {
        #[cfg(feature = "native-pinned-cell-cache-inheritance")]
        if let Some(changed) = &self.changed {
            let position = changed.position(point_bytes(point), hash_once)?;
            return Some(&self.ordered[position].1);
        }
        #[cfg(not(feature = "native-pinned-cell-cache-inheritance"))]
        if let Some(positions) = &self.positions {
            let position = positions.position(point_bytes(point), hash_once, |position| {
                point_bytes(&self.ordered[position].0)
            })?;
            return Some(&self.ordered[position].1);
        }
        let position = self
            .ordered
            .binary_search_by(|(key, _)| key.cmp(point))
            .ok()?;
        Some(&self.ordered[position].1)
    }

    #[cfg(test)]
    pub(super) fn get(&self, point: &OutPoint) -> Option<&Option<LiveCell>> {
        self.get_hashed(point, &mut None)
    }
}

fn point_bytes(point: &OutPoint) -> &[u8] {
    // Use the two fixed-width fields compared by OutPoint::cmp.
    &point.as_slice()[..OutPoint::TOTAL_SIZE]
}

#[cfg(test)]
mod tests {
    use stem_ledger::{Cell, Origin};

    use super::*;
    use crate::native_store::cell_change_index::MAX_CHANGES;

    #[test]
    fn preserves_live_map_order_and_distinguishes_deletions_from_misses() -> anyhow::Result<()> {
        let point = |index| OutPoint::new([9; 32].pack(), index);
        let cell = LiveCell {
            cell: Cell::new(
                20_000_000_000,
                Default::default(),
                vec![],
                Default::default(),
            )?,
            origin: Origin {
                height: 1,
                timestamp_ms: 2,
                context_id: Default::default(),
            },
        };
        for len in [0, 32, 256, MAX_CHANGES + 1] {
            let mut edits: LiveChanges = (0..len as u32).rev().map(|i| (point(i), None)).collect();
            if len > 1 {
                edits.insert(point(1), Some(cell.clone()));
            }
            let ordered = edits.keys().cloned().collect::<Vec<_>>();
            let prepared = PreparedCells::from(edits).with_index();
            #[cfg(not(feature = "native-pinned-cell-cache-inheritance"))]
            assert_eq!(
                prepared.positions.is_some(),
                (128..=MAX_CHANGES).contains(&len)
            );
            #[cfg(feature = "native-pinned-cell-cache-inheritance")]
            assert_eq!(prepared.changed_outpoints().is_some(), len <= MAX_CHANGES);
            assert_eq!(prepared.keys().cloned().collect::<Vec<_>>(), ordered);
            assert!(prepared.get(&point(len as u32)).is_none());
            for key in &ordered {
                let row = prepared.get(key).expect("record or tombstone");
                if key == &point(1) {
                    assert_eq!(row.as_ref().unwrap().cell.as_slice(), cell.cell.as_slice());
                } else {
                    assert!(row.is_none());
                }
            }
        }
        Ok(())
    }
}
