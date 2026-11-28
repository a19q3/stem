use std::num::NonZeroUsize;

use super::Node;

#[derive(Clone, Copy, Debug)]
pub(super) struct Location {
    segment: usize,
    row: NonZeroUsize,
}

impl Location {
    #[cfg(feature = "parallel-patricia")]
    pub(super) fn shift_segments(self, offset: usize) -> Self {
        Self {
            segment: self
                .segment
                .checked_add(offset)
                .expect("combined node segment"),
            ..self
        }
    }
}

/// Child updates transfer ownership of their populated buffers. Only nodes
/// inserted by the merger itself go into its new tail.
#[derive(Debug, Default)]
pub(super) struct NodeSegments {
    completed: Vec<Vec<Node>>,
    tail: Vec<Node>,
}

impl NodeSegments {
    pub(super) fn push(&mut self, node: Node) -> Location {
        self.tail.push(node);
        Location {
            segment: self.completed.len(),
            row: NonZeroUsize::new(self.tail.len()).expect("inserted generated node"),
        }
    }

    pub(super) fn get(&self, location: Location) -> &Node {
        let rows = if location.segment == self.completed.len() {
            &self.tail
        } else {
            &self.completed[location.segment]
        };
        &rows[location.row.get() - 1]
    }

    #[cfg(feature = "parallel-patricia")]
    pub(super) fn append(&mut self, mut other: Self) -> usize {
        if !self.tail.is_empty() {
            self.completed.push(std::mem::take(&mut self.tail));
        }
        let offset = self.completed.len();
        self.completed.append(&mut other.completed);
        if !other.tail.is_empty() {
            self.completed.push(other.tail);
        }
        offset
    }
}
