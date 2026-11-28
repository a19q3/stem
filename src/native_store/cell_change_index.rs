//! Exact OutPoint positions in the ordered Cell delta.

use std::{
    hash::{BuildHasher, RandomState},
    sync::LazyLock,
};

use hashbrown::HashTable;

pub(super) const MAX_CHANGES: usize = 16_384;
static HASHER: LazyLock<RandomState> = LazyLock::new(RandomState::new);

pub(super) struct CellPositions(HashTable<u32>);

impl CellPositions {
    pub(super) fn build<'a>(len: usize, point_at: impl Fn(usize) -> &'a [u8]) -> Option<Self> {
        if len > MAX_CHANGES {
            return None;
        }
        let hash_position = |&position: &u32| HASHER.hash_one(point_at(position as usize));
        let mut positions = HashTable::new();
        positions.try_reserve(len, hash_position).ok()?;
        for position in 0..len as u32 {
            positions.insert_unique(hash_position(&position), position, hash_position);
        }
        Some(Self(positions))
    }

    pub(super) fn position<'a>(
        &self,
        point: &[u8],
        hash_once: &mut Option<u64>,
        point_at: impl Fn(usize) -> &'a [u8],
    ) -> Option<usize> {
        if self.0.is_empty() {
            return None;
        }
        let hash = *hash_once.get_or_insert_with(|| HASHER.hash_one(point));
        self.0
            .find(hash, |&position| point_at(position as usize) == point)
            .map(|&position| position as usize)
    }
}

#[cfg(feature = "native-pinned-cell-cache-inheritance")]
mod shared {
    use std::sync::Arc;

    use ckb_types::{packed::OutPoint, prelude::*};

    use super::{CellPositions, MAX_CHANGES};
    use crate::native_store::PreparedCells;

    pub(in crate::native_store) struct ChangedOutPoints {
        packed: Box<[[u8; OutPoint::TOTAL_SIZE]]>,
        positions: CellPositions,
    }

    impl ChangedOutPoints {
        pub(in crate::native_store) fn build(live: &PreparedCells) -> Option<Arc<Self>> {
            let len = live.len();
            if len > MAX_CHANGES {
                return None;
            }
            let mut packed = Vec::new();
            packed.try_reserve_exact(len).ok()?;
            for point in live.keys() {
                packed.push(point.as_slice().try_into().ok()?);
            }
            let packed: Box<[[u8; OutPoint::TOTAL_SIZE]]> = packed.into_boxed_slice();
            let positions = CellPositions::build(len, |position| &packed[position])?;
            Some(Arc::new(Self { packed, positions }))
        }

        pub(in crate::native_store) fn position(
            &self,
            point: &[u8],
            hash_once: &mut Option<u64>,
        ) -> Option<usize> {
            self.positions
                .position(point, hash_once, |position| &self.packed[position])
        }
    }
}

#[cfg(feature = "native-pinned-cell-cache-inheritance")]
pub(super) use shared::ChangedOutPoints;
