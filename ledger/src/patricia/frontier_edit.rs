//! Consume a bounded batch of authenticated branch reads directly in the editor.

use super::{
    BatchWrites, Context, Fork, GeneratedJournal, Key, MIN_PARALLEL_CHANGES, Node, NodeValue,
    Nodes, Patricia, PreviousValue, Result, VerifiedChanges, check_change_order, checked_node,
    ensure, matching_fork,
};

const READ_WIDTH: usize = 64;
const MAX_PIECES: usize = 511;
const MIN_CHILD_CHANGES: usize = 8;

type Changes<'a> = &'a [(Key, Option<Key>)];

struct Piece<'a> {
    root: Key,
    changes: Changes<'a>,
    depth: usize,
    edit: Edit,
}

enum Edit {
    Pending,
    Terminal(Option<(Key, Node)>),
    Branch(Fork, [usize; 2]),
}

impl<'a> Piece<'a> {
    fn pending(root: Key, changes: Changes<'a>, depth: usize) -> Self {
        Self {
            root,
            changes,
            depth,
            edit: Edit::Pending,
        }
    }
}

impl Patricia {
    /// Prepare a bounded upper frontier over the old immutable root.
    /// It consumes already-checked nodes directly and returns a canonical journal.
    pub fn batch_update_verified_frontier<Read, Value>(
        &self,
        root: Key,
        read: Read,
        read_many: impl FnMut(&[Key]) -> Result<Vec<Option<Value>>>,
        changes: &[(Key, Option<Key>)],
        mut previous: impl FnMut(Key, Key, Option<Key>),
    ) -> Result<(Key, VerifiedChanges)>
    where
        Read: Fn(&Key) -> Result<Option<Value>> + Sync,
        Value: NodeValue,
    {
        let workers = rayon::current_num_threads();
        if workers < 2 || changes.len() < 2 * MIN_PARALLEL_CHANGES || root == self.empty_root() {
            return self.batch_update_verified(root, read, changes, previous, true);
        }

        check_change_order(changes)?;
        let pieces = self.plan_frontier(root, changes, read_many)?;
        let mut nodes = Nodes {
            tree: self,
            read: &read,
            writes: GeneratedJournal::with_capacity(0),
        };
        let mut prior = Vec::new();
        let next = nodes.apply_frontier(&pieces, 0, workers, &mut prior)?;
        for (key, value, replacement) in prior {
            previous(key, value, replacement);
        }
        Ok((next, VerifiedChanges::from_update(self, nodes.writes)))
    }

    fn plan_frontier<'a, Value: NodeValue>(
        &self,
        root: Key,
        changes: Changes<'a>,
        mut read_many: impl FnMut(&[Key]) -> Result<Vec<Option<Value>>>,
    ) -> Result<Vec<Piece<'a>>> {
        let empty = self.empty_root();
        let mut pieces = vec![Piece::pending(root, changes, 0)];
        let mut ids = Vec::with_capacity(READ_WIDTH);
        let mut start = 0;
        while start < pieces.len() {
            let end = (start + READ_WIDTH).min(pieces.len());
            ids.clear();
            ids.extend(
                pieces[start..end]
                    .iter()
                    .map(|piece| piece.root)
                    .filter(|id| *id != empty),
            );
            let reads = if ids.is_empty() {
                Vec::new()
            } else {
                read_many(&ids)?
            };
            ensure!(reads.len() == ids.len(), "wrong Patricia edit batch length");
            let mut reads = reads.into_iter();
            for position in start..end {
                let Piece {
                    root,
                    changes,
                    depth,
                    ..
                } = pieces[position];
                let old = if root == empty {
                    None
                } else {
                    let raw = reads.next().flatten().context("missing Patricia node")?;
                    Some((root, checked_node(&raw, self, &root)?.0))
                };
                let fork = matching_fork(&old, changes, depth, MIN_CHILD_CHANGES);
                if let Some((fork, subtrees)) = fork
                    && pieces.len() + 2 <= MAX_PIECES
                {
                    let left = pieces.len();
                    pieces.push(Piece::pending(fork.children[0], subtrees[0], fork.depth()));
                    pieces.push(Piece::pending(fork.children[1], subtrees[1], fork.depth()));
                    pieces[position].edit = Edit::Branch(fork, [left, left + 1]);
                } else {
                    pieces[position].edit = Edit::Terminal(old);
                }
            }
            start = end;
        }
        Ok(pieces)
    }
}

impl<Read, Value> Nodes<'_, &Read, GeneratedJournal>
where
    Read: Fn(&Key) -> Result<Option<Value>> + Sync,
    Value: NodeValue,
{
    fn apply_frontier(
        &mut self,
        pieces: &[Piece<'_>],
        position: usize,
        workers: usize,
        previous: &mut Vec<PreviousValue>,
    ) -> Result<Key> {
        let piece = &pieces[position];
        match &piece.edit {
            Edit::Pending => unreachable!("complete Patricia edit frontier"),
            Edit::Terminal(old) => self.update_parallel_loaded(
                old.clone(),
                piece.changes,
                piece.depth,
                workers,
                previous,
            ),
            Edit::Branch(fork, positions) => {
                let fork = *fork;
                if workers >= 2
                    && positions
                        .iter()
                        .all(|&position| pieces[position].changes.len() >= MIN_PARALLEL_CHANGES)
                {
                    return self.join_fork(
                        fork,
                        workers,
                        previous,
                        |child, side, budget, prior| {
                            child.apply_frontier(pieces, positions[side], budget, prior)
                        },
                    );
                }
                let left = self.apply_frontier(pieces, positions[0], 1, previous)?;
                let right = self.apply_frontier(pieces, positions[1], 1, previous)?;
                Ok(self.finish_branch(
                    fork.id,
                    &fork.prefix,
                    fork.bits,
                    fork.children,
                    [left, right],
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, sync::Mutex};

    use ckb_types::packed::Byte32;

    use super::*;
    use crate::patricia::Namespace;

    #[test]
    fn frontier_reuses_loaded_roots_and_preserves_parallel_journal_order() -> Result<()> {
        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let keys: Vec<_> = (0..6400u32)
            .map(|n| tree.record_key(&n.to_le_bytes()))
            .collect();
        let old = tree.value_hash(b"old");
        let new = tree.value_hash(b"new");
        let mut seed: Vec<_> = keys[..4096].iter().map(|&key| (key, Some(old))).collect();
        seed.sort_unstable_by_key(|&(key, _)| key);
        let (root, storage) = tree.batch_update(
            tree.empty_root(),
            |_| Ok(None::<Vec<u8>>),
            &seed,
            |_, _, _| {},
        )?;
        let mut edits: Vec<_> = keys
            .iter()
            .enumerate()
            .filter_map(|(index, &key)| match index {
                0..=1500 | 6000..=6399 => Some((key, None)),
                1501..=2100 => Some((key, Some(old))),
                2101..=2800 | 4096..=5999 => Some((key, Some(new))),
                _ => None,
            })
            .collect();
        edits.sort_unstable_by_key(|&(key, _)| key);

        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let mut expected_previous = Vec::new();
        let baseline = |id: &Key| Ok(storage.get(id).and_then(Option::as_deref));
        let expected = pool.install(|| {
            tree.batch_update_verified(
                root,
                baseline,
                &edits,
                |key, value, next| expected_previous.push((key, value, next)),
                true,
            )
        })?;
        let loaded = Mutex::new(HashSet::new());
        let read = |id: &Key| {
            assert!(
                loaded.lock().unwrap().insert(*id),
                "read a cached frontier node again"
            );
            baseline(id)
        };
        let mut actual_previous = Vec::new();
        let mut max_width = 0;
        let actual = pool.install(|| {
            tree.batch_update_verified_frontier(
                root,
                read,
                |ids| {
                    max_width = max_width.max(ids.len());
                    ids.iter().map(read).collect()
                },
                &edits,
                |key, value, next| actual_previous.push((key, value, next)),
            )
        })?;
        assert!((4..=READ_WIDTH).contains(&max_width));
        assert_eq!(actual, expected);
        assert_eq!(actual_previous, expected_previous);
        Ok(())
    }
}
