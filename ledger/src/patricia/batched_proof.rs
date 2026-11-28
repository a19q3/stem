//! Read one frontier of a shared proof traversal from an immutable view.

use super::{
    Context, Key, Node, NodeValue, Patricia, Proof, Result, append_proof_node, checked_node,
    ensure, split_proof_queries,
};

const READ_WIDTH: usize = 64;

struct Path<'a> {
    id: Key,
    queries: &'a [(Key, usize)],
    depth: usize,
}

impl Patricia {
    /// Authenticate a shared traversal and return ordinary proofs in caller order.
    /// The reader must return one result per requested ID from the same view.
    pub fn prove_many_batched<Value: NodeValue>(
        &self,
        root: Key,
        keys: &[Key],
        mut read: impl FnMut(&[Key]) -> Result<Vec<Option<Value>>>,
    ) -> Result<Vec<(Option<Key>, Proof)>> {
        let mut proofs = vec![(None, Proof { nodes: Vec::new() }); keys.len()];
        let empty = self.empty_root();
        if keys.is_empty() || root == empty {
            return Ok(proofs);
        }
        let mut queries: Vec<_> = keys.iter().copied().zip(0..keys.len()).collect();
        queries.sort_unstable_by_key(|&(key, _)| key);
        let mut frontier = vec![Path {
            id: root,
            queries: &queries,
            depth: 0,
        }];
        let mut children = Vec::new();
        let mut ids = Vec::with_capacity(READ_WIDTH.min(keys.len()));
        while !frontier.is_empty() {
            for paths in frontier.chunks(READ_WIDTH) {
                ids.clear();
                ids.extend(paths.iter().map(|path| path.id));
                let values = read(&ids)?;
                ensure!(
                    values.len() == paths.len(),
                    "wrong Patricia batch read length"
                );
                for (path, value) in paths.iter().zip(values) {
                    let value = value.context("missing Patricia node")?;
                    let (node, bytes) = checked_node(&value, self, &path.id)?;
                    append_proof_node(&node, bytes, path.queries, &mut proofs);
                    match node {
                        Node::Leaf { key, value } => {
                            for &(query, index) in path.queries {
                                if query == key {
                                    proofs[index].0 = Some(value);
                                }
                            }
                        }
                        Node::Branch {
                            prefix,
                            bits,
                            children: ids,
                        } => {
                            ensure!(
                                usize::from(bits) >= path.depth,
                                "invalid Patricia path depth"
                            );
                            let (left, right) = split_proof_queries(path.queries, prefix, bits);
                            let depth = usize::from(bits) + 1;
                            for (id, queries) in ids.into_iter().zip([left, right]) {
                                if id != empty && !queries.is_empty() {
                                    children.push(Path { id, queries, depth });
                                }
                            }
                        }
                    }
                }
            }
            frontier.clear();
            std::mem::swap(&mut frontier, &mut children);
        }
        Ok(proofs)
    }
}

#[cfg(test)]
mod tests {
    use ckb_types::packed::Byte32;

    use super::*;
    use crate::patricia::Namespace;

    #[test]
    fn grouped_frontiers_preserve_order_and_exact_proofs() -> Result<()> {
        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let keys: Vec<_> = (0..300u16)
            .map(|value| tree.record_key(&value.to_le_bytes()))
            .collect();
        let value = tree.value_hash(b"record");
        let mut changes: Vec<_> = keys[..250].iter().map(|&key| (key, Some(value))).collect();
        changes.sort_unstable_by_key(|(key, _)| *key);
        let (root, nodes) = tree.batch_update(
            tree.empty_root(),
            |_| Ok(None::<Vec<u8>>),
            &changes,
            |_, _, _| {},
        )?;
        let queries: Vec<_> = keys
            .iter()
            .rev()
            .copied()
            .chain([keys[0], keys[299]])
            .collect();
        let mut standard = tree.edit(root, |id| Ok(nodes.get(id).cloned().flatten()));
        let expected = standard.prove_many(&queries)?;
        let mut widest = 0;
        let actual = tree.prove_many_batched(root, &queries, |ids| {
            widest = widest.max(ids.len());
            Ok(ids
                .iter()
                .map(|id| nodes.get(id).cloned().flatten())
                .collect())
        })?;
        assert_eq!(widest, READ_WIDTH);
        assert_eq!(actual, expected);
        for (query, (found, proof)) in queries.iter().zip(actual) {
            assert!(tree.verify(&root, query, found.as_ref(), &proof));
        }
        let bad = tree.prove_many_batched(root, &queries, |_| Ok(Vec::<Option<Vec<u8>>>::new()));
        assert!(bad.is_err());
        Ok(())
    }
}
