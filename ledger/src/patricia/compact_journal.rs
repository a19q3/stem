//! Decoded nodes kept separately from the sorted content IDs and tombstones.

use std::num::NonZeroUsize;

use super::{BatchWrites, Key, Node, NodeWrites};

type Entry = (Key, Option<NonZeroUsize>);

#[derive(Debug)]
pub(super) struct CompactJournal {
    entries: Vec<Entry>,
    nodes: Vec<Node>,
}

impl CompactJournal {
    pub(super) fn finish(mut self) -> Self {
        // Stable sort preserves write order for equal IDs. Keep the newest
        // entry, including a tombstone that supersedes an earlier put.
        self.entries.sort_by_key(|(id, _)| *id);
        self.entries.dedup_by(|later, earlier| {
            if later.0 == earlier.0 {
                *earlier = *later;
                true
            } else {
                false
            }
        });
        self
    }

    pub(super) fn get_key_value(&self, id: &Key) -> Option<(&Key, Option<&Node>)> {
        let index = self
            .entries
            .binary_search_by(|(found, _)| found.cmp(id))
            .ok()?;
        Some(self.row(&self.entries[index]))
    }

    pub(super) fn iter(&self) -> impl Iterator<Item = (&Key, Option<&Node>)> {
        self.entries.iter().map(|entry| self.row(entry))
    }

    fn row<'a>(&'a self, (id, node): &'a Entry) -> (&'a Key, Option<&'a Node>) {
        (id, node.map(|index| &self.nodes[index.get() - 1]))
    }
}

impl NodeWrites for CompactJournal {
    fn record_insert(&mut self, id: Key, node: Node, _: &[u8]) {
        self.nodes.push(node);
        self.entries.push((id, NonZeroUsize::new(self.nodes.len())));
    }

    fn record_delete(&mut self, id: Key) {
        self.entries.push((id, None));
    }
}

impl BatchWrites for CompactJournal {
    fn with_capacity(rows: usize) -> Self {
        Self {
            entries: Vec::with_capacity(rows),
            nodes: Vec::new(),
        }
    }

    #[cfg(feature = "parallel-patricia")]
    fn reserve(&mut self, rows: usize) {
        self.entries.reserve(rows);
    }

    #[cfg(feature = "parallel-patricia")]
    fn append(&mut self, other: Self) {
        let offset = self.nodes.len();
        self.entries
            .extend(other.entries.into_iter().map(|(id, node)| {
                (
                    id,
                    node.map(|index| index.checked_add(offset).expect("combined node index")),
                )
            }));
        self.nodes.extend(other.nodes);
    }
}

impl PartialEq for CompactJournal {
    fn eq(&self, other: &Self) -> bool {
        self.entries.len() == other.entries.len() && self.iter().eq(other.iter())
    }
}

impl Eq for CompactJournal {}

#[cfg(all(test, feature = "parallel-patricia"))]
mod tests {
    use super::*;

    #[test]
    fn joined_journal_orders_ids_and_keeps_the_last_edit() {
        let early = [1; 32];
        let overwritten = [2; 32];
        let unchanged = [3; 32];
        let missing = [4; 32];
        let leaf = |value| Node::Leaf {
            key: [9; 32],
            value: [value; 32],
        };
        let mut left = CompactJournal::with_capacity(0);
        left.record_insert(overwritten, leaf(1), &[]);
        left.record_insert(unchanged, leaf(2), &[]);
        let mut right = CompactJournal::with_capacity(0);
        right.record_insert(early, leaf(3), &[]);
        right.record_delete(overwritten);

        left.append(right);
        let rows = left.finish();
        assert_eq!(
            rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [early, overwritten, unchanged]
        );
        assert_eq!(rows.get_key_value(&early), Some((&early, Some(&leaf(3)))));
        assert_eq!(rows.get_key_value(&overwritten), Some((&overwritten, None)));
        assert_eq!(
            rows.get_key_value(&unchanged),
            Some((&unchanged, Some(&leaf(2))))
        );
        assert!(rows.get_key_value(&missing).is_none());
    }
}
