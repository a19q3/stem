//! Shared declarations for positions that rolling workers can still observe.

#[cfg(feature = "native-reclaim-cell-projection")]
use std::collections::{VecDeque, hash_map::Entry};

#[cfg(feature = "native-reclaim-cell-projection")]
use super::LiveChanges;
use super::{OutPoint, Write, Writes, record};

pub(super) struct IncrementalProjection {
    pub(super) writes: Writes,
    edits: usize,
    #[cfg(feature = "native-reclaim-cell-projection")]
    pending: VecDeque<(usize, OutPoint)>,
    #[cfg(feature = "native-reclaim-cell-projection")]
    pub(super) settled: LiveChanges,
}

impl IncrementalProjection {
    pub(super) fn new(expected: usize) -> Self {
        Self {
            writes: Writes::with_capacity(expected),
            edits: 0,
            #[cfg(feature = "native-reclaim-cell-projection")]
            pending: VecDeque::new(),
            #[cfg(feature = "native-reclaim-cell-projection")]
            settled: LiveChanges::new(),
        }
    }

    pub(super) fn add(&mut self, point: OutPoint, edit: Write) {
        #[cfg(feature = "native-reclaim-cell-projection")]
        self.pending.push_back((edit.position, point.clone()));
        self.edits += 1;
        record(&mut self.writes, point, edit);
    }

    pub(super) fn len(&self) -> usize {
        self.edits
    }

    /// New jobs see the authoritative prefix and only the remaining declarations.
    /// Outstanding jobs may already have observed an older declaration. The
    /// coordinator still checks their source positions independently of this map.
    #[cfg(feature = "native-reclaim-cell-projection")]
    pub(super) fn advance(&mut self, position: usize, delta: LiveChanges) -> usize {
        self.settled.extend(delta);
        let mut removed = 0;
        while self
            .pending
            .front()
            .is_some_and(|(earlier, _)| *earlier < position)
        {
            let (_, point) = self.pending.pop_front().expect("checked projection edit");
            removed += 1;
            let Entry::Occupied(mut entry) = self.writes.entry(point) else {
                continue;
            };
            let history = entry.get_mut();
            if history.first.position >= position {
                continue;
            }
            let first_visible = history
                .later
                .partition_point(|edit| edit.position < position);
            if first_visible == history.later.len() {
                entry.remove();
            } else {
                history.first = history
                    .later
                    .drain(..=first_visible)
                    .last()
                    .expect("visible edit");
            }
        }
        self.edits -= removed;
        removed
    }
}
