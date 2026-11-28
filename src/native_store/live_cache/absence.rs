//! A small direct-mapped cache for exact OutPoint misses in a pinned snapshot.

use std::{
    hash::{BuildHasher, RandomState},
    sync::LazyLock,
};

use ckb_types::{packed::OutPoint, prelude::*};

use super::SHARD_ITEMS;

static HASHER: LazyLock<RandomState> = LazyLock::new(RandomState::new);
type Point = [u8; OutPoint::TOTAL_SIZE];

#[derive(Default)]
pub(super) struct Absences {
    slots: Box<[Option<Point>]>,
    pub(super) entries: usize,
    pub(super) hits: u64,
}

impl Absences {
    pub(super) fn contains(&mut self, point: &OutPoint) -> bool {
        if self.entries == 0 {
            return false;
        }
        let Ok(point) = <&Point>::try_from(point.as_slice()) else {
            return false;
        };
        let found = self.slots[slot(point)].as_ref() == Some(point);
        self.hits = self.hits.saturating_add(u64::from(found));
        found
    }

    pub(super) fn insert(&mut self, point: &OutPoint) {
        let Ok(point) = <Point>::try_from(point.as_slice()) else {
            return;
        };
        if self.slots.is_empty() {
            self.slots = vec![None; SHARD_ITEMS].into_boxed_slice();
        }
        if self.slots[slot(&point)].replace(point).is_none() {
            self.entries += 1;
        }
    }

    pub(super) fn remove(&mut self, point: &OutPoint) {
        if self.entries == 0 {
            return;
        }
        let Ok(point) = <&Point>::try_from(point.as_slice()) else {
            return;
        };
        let item = &mut self.slots[slot(point)];
        if item.as_ref() == Some(point) {
            *item = None;
            self.entries -= 1;
        }
    }
}

fn slot(point: &Point) -> usize {
    HASHER.hash_one(point) as usize % SHARD_ITEMS
}
