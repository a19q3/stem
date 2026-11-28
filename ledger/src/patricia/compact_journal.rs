//! Decoded nodes kept separately from the sorted content IDs and tombstones.

#[cfg(not(feature = "segmented-verified-journal"))]
use std::num::NonZeroUsize;

use super::{BatchWrites, Key, Node, NodeWrites};

#[cfg(feature = "segmented-verified-journal")]
mod segments;
#[cfg(feature = "segmented-verified-journal")]
use segments::{Location, NodeSegments};

#[cfg(not(feature = "segmented-verified-journal"))]
type Location = NonZeroUsize;
#[cfg(not(feature = "segmented-verified-journal"))]
type NodeSegments = Vec<Node>;

type Entry = (Key, Option<Location>);

#[derive(Debug)]
pub(super) struct CompactJournal {
    entries: Vec<Entry>,
    nodes: NodeSegments,
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
        #[cfg(feature = "segmented-verified-journal")]
        let node = node.map(|location| self.nodes.get(location));
        #[cfg(not(feature = "segmented-verified-journal"))]
        let node = node.map(|location| &self.nodes[location.get() - 1]);
        (id, node)
    }
}

impl NodeWrites for CompactJournal {
    fn record_insert(&mut self, id: Key, node: Node, _: &[u8]) {
        #[cfg(feature = "segmented-verified-journal")]
        let location = self.nodes.push(node);
        #[cfg(not(feature = "segmented-verified-journal"))]
        let location = {
            self.nodes.push(node);
            NonZeroUsize::new(self.nodes.len()).expect("inserted generated node")
        };
        self.entries.push((id, Some(location)));
    }

    fn record_delete(&mut self, id: Key) {
        self.entries.push((id, None));
    }
}

impl BatchWrites for CompactJournal {
    fn with_capacity(rows: usize) -> Self {
        Self {
            entries: Vec::with_capacity(rows),
            nodes: NodeSegments::default(),
        }
    }

    #[cfg(feature = "parallel-patricia")]
    fn reserve(&mut self, rows: usize) {
        self.entries.reserve(rows);
    }

    #[cfg(feature = "parallel-patricia")]
    fn append(&mut self, other: Self) {
        #[cfg(feature = "segmented-verified-journal")]
        let offset = self.nodes.append(other.nodes);
        #[cfg(not(feature = "segmented-verified-journal"))]
        let offset = {
            let offset = self.nodes.len();
            self.nodes.extend(other.nodes);
            offset
        };
        self.entries
            .extend(other.entries.into_iter().map(|(id, node)| {
                let node = node.map(|location| {
                    #[cfg(feature = "segmented-verified-journal")]
                    return location.shift_segments(offset);
                    #[cfg(not(feature = "segmented-verified-journal"))]
                    location.checked_add(offset).expect("combined node index")
                });
                (id, node)
            }));
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
        let parent = [4; 32];
        let missing = [5; 32];
        let base = [0; 32];
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

        #[cfg(feature = "segmented-verified-journal")]
        let left_node = left.row(&left.entries[1]).1.unwrap() as *const Node;
        #[cfg(feature = "segmented-verified-journal")]
        let right_node = right.row(&right.entries[0]).1.unwrap() as *const Node;
        left.append(right);
        left.record_insert(parent, leaf(4), &[]);
        let mut top = CompactJournal::with_capacity(0);
        top.record_insert(base, leaf(0), &[]);
        top.append(left);
        let rows = top.finish();
        assert_eq!(
            rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            [base, early, overwritten, unchanged, parent]
        );
        assert_eq!(rows.get_key_value(&early), Some((&early, Some(&leaf(3)))));
        assert_eq!(rows.get_key_value(&overwritten), Some((&overwritten, None)));
        assert_eq!(
            rows.get_key_value(&unchanged),
            Some((&unchanged, Some(&leaf(2))))
        );
        assert_eq!(rows.get_key_value(&base), Some((&base, Some(&leaf(0)))));
        assert_eq!(rows.get_key_value(&parent), Some((&parent, Some(&leaf(4)))));
        assert!(rows.get_key_value(&missing).is_none());
        #[cfg(feature = "segmented-verified-journal")]
        {
            assert!(std::ptr::eq(
                rows.get_key_value(&unchanged).unwrap().1.unwrap(),
                left_node
            ));
            assert!(std::ptr::eq(
                rows.get_key_value(&early).unwrap().1.unwrap(),
                right_node
            ));
        }
    }
}
