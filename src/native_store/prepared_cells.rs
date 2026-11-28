//! Compact, immutable Cell changes shared by all readers of one candidate.

use std::{
    hash::{BuildHasher, RandomState},
    sync::LazyLock,
};

use ckb_types::{packed::OutPoint, prelude::*};
use hashbrown::HashTable;
use rayon::prelude::*;
use stem_ledger::LiveCell;

use super::LiveChanges;

const MIN_INDEX_ENTRIES: usize = 128;
const MAX_INDEX_ENTRIES: usize = 16_384;
static POINT_HASHER: LazyLock<RandomState> = LazyLock::new(RandomState::new);

type Entry = (OutPoint, Option<LiveCell>);

pub(super) struct PreparedCells {
    ordered: Vec<Entry>,
    positions: Option<HashTable<u32>>,
}

impl From<LiveChanges> for PreparedCells {
    fn from(changes: LiveChanges) -> Self {
        Self {
            ordered: changes.into_iter().collect(),
            positions: None,
        }
    }
}

impl PreparedCells {
    pub(super) fn with_index(mut self) -> Self {
        let ordered = &self.ordered;
        let hash_position = |&index: &u32| hash(&ordered[index as usize].0);
        let mut positions = HashTable::new();
        self.positions = if (MIN_INDEX_ENTRIES..=MAX_INDEX_ENTRIES).contains(&ordered.len())
            && positions.try_reserve(ordered.len(), hash_position).is_ok()
        {
            for (position, (point, _)) in ordered.iter().enumerate() {
                positions.insert_unique(hash(point), position as u32, hash_position);
            }
            Some(positions)
        } else {
            None
        };
        self
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
        let position = if let Some(positions) = &self.positions {
            let hash = *hash_once.get_or_insert_with(|| hash(point));
            *positions.find(hash, |&index| {
                self.ordered[index as usize].0.cmp(point).is_eq()
            })? as usize
        } else {
            self.ordered
                .binary_search_by(|(key, _)| key.cmp(point))
                .ok()?
        };
        Some(&self.ordered[position].1)
    }

    #[cfg(test)]
    pub(super) fn get(&self, point: &OutPoint) -> Option<&Option<LiveCell>> {
        self.get_hashed(point, &mut None)
    }
}

fn hash(point: &OutPoint) -> u64 {
    // Use the two fixed-width fields compared by OutPoint::cmp.
    POINT_HASHER.hash_one(&point.as_slice()[..OutPoint::TOTAL_SIZE])
}

#[cfg(test)]
mod tests {
    use stem_ledger::{Cell, Origin};

    use super::*;

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
        for (len, indexed) in [(32, false), (256, true), (MAX_INDEX_ENTRIES + 1, false)] {
            let mut edits: LiveChanges = (0..len as u32).rev().map(|i| (point(i), None)).collect();
            edits.insert(point(1), Some(cell.clone()));
            let ordered = edits.keys().cloned().collect::<Vec<_>>();
            let prepared = PreparedCells::from(edits).with_index();
            assert_eq!(prepared.positions.is_some(), indexed);
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
