//! Incremental authenticated state. Node values contain only record hashes.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::Entity};

use crate::{
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

use stem_stake::patricia::{
    Commitment, MAX_NODES, MAX_PATH_BYTES, Node, canonical_prefix, decode_path, side,
};
pub use stem_stake::patricia::{Key, Namespace};

#[derive(Clone, Debug)]
pub struct Patricia {
    commitment: Commitment,
}

impl Patricia {
    pub fn new(network: Byte32, namespace: Namespace) -> Self {
        Self {
            commitment: Commitment::new(key(network.as_slice()), namespace),
        }
    }

    pub fn empty_root(&self) -> Key {
        self.commitment.empty_root()
    }

    fn node_hash(&self, encoded: &[u8]) -> Key {
        self.commitment.node_hash(encoded)
    }

    pub fn value_hash(&self, bytes: &[u8]) -> Key {
        self.commitment.value_hash(bytes)
    }

    pub fn record_key(&self, name: &[u8]) -> Key {
        self.commitment.record_key(name)
    }

    pub fn edit(
        &self,
        root: Key,
        read: impl FnMut(&Key) -> Result<Option<Vec<u8>>>,
    ) -> Edit<'_, impl FnMut(&Key) -> Result<Option<Vec<u8>>>> {
        Edit {
            tree: self,
            root,
            read,
            writes: BTreeMap::new(),
        }
    }

    pub fn verify(&self, root: &Key, query: &Key, expected: Option<&Key>, proof: &Proof) -> bool {
        self.commitment
            .verify(root, query, expected, proof.nodes.iter().map(Vec::as_slice))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// The path from the root to the matching leaf or the point of divergence.
    pub nodes: Vec<Vec<u8>>,
}

impl Proof {
    pub fn encode(&self) -> Result<Bytes> {
        ensure!(self.nodes.len() <= MAX_NODES, "oversized Patricia path");
        let nodes = self
            .nodes
            .iter()
            .map(|node| {
                Node::decode(node)?;
                Ok(wire::Bytes::from_raw_data(node)?)
            })
            .collect::<Result<_>>()?;
        Ok(wire::PatriciaPath::new_builder()
            .set(nodes)
            .build()
            .as_bytes())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_PATH_BYTES, "oversized Patricia proof");
        Ok(Self {
            nodes: decode_path(bytes)?
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect(),
        })
    }
}

pub struct Edit<'tree, Read> {
    tree: &'tree Patricia,
    root: Key,
    read: Read,
    writes: BTreeMap<Key, Option<Vec<u8>>>,
}

impl<Read: FnMut(&Key) -> Result<Option<Vec<u8>>>> Edit<'_, Read> {
    pub fn root(&self) -> Key {
        self.root
    }

    pub fn set(&mut self, key: Key, value: Option<Key>) -> Result<Option<Key>> {
        let mut previous = None;
        let old = self.load_optional(self.root)?;
        let root = self.update(old, &[(key, value)], 0, &mut |_, value, _| {
            previous = Some(value)
        })?;
        self.root = root;
        Ok(previous)
    }

    /// Apply unique changes in key order and report the touched leaves that existed.
    pub fn set_batch(
        &mut self,
        changes: &[(Key, Option<Key>)],
        mut previous: impl FnMut(Key, Key, Option<Key>),
    ) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        ensure!(
            changes.windows(2).all(|pair| pair[0].0 < pair[1].0),
            "unordered or duplicate Patricia changes"
        );
        let old = self.load_optional(self.root)?;
        self.root = self.update(old, changes, 0, &mut previous)?;
        Ok(())
    }

    pub fn prove(&mut self, key: &Key) -> Result<(Option<Key>, Proof)> {
        let mut nodes = Vec::new();
        let mut id = self.root;
        let mut depth = 0;
        let mut value = None;
        while id != self.tree.empty_root() {
            let (node, bytes) = self.load(&id)?;
            nodes.push(bytes);
            match node {
                Node::Leaf {
                    key: found,
                    value: hash,
                } => {
                    value = (found == *key).then_some(hash);
                    break;
                }
                Node::Branch {
                    prefix,
                    bits,
                    children,
                } => {
                    ensure!(usize::from(bits) >= depth, "invalid Patricia path depth");
                    if canonical_prefix(key, bits) != prefix {
                        break;
                    }
                    id = children[side(key, bits)];
                    depth = usize::from(bits) + 1;
                }
            }
        }
        Ok((value, Proof { nodes }))
    }

    pub fn into_writes(self) -> BTreeMap<Key, Option<Vec<u8>>> {
        self.writes
    }

    fn load(&mut self, id: &Key) -> Result<(Node, Vec<u8>)> {
        let bytes = match self.writes.get(id) {
            Some(entry) => entry.clone(),
            None => (self.read)(id)?,
        }
        .context("missing Patricia node")?;
        ensure!(
            self.tree.node_hash(&bytes) == *id,
            "Patricia node hash mismatch"
        );
        Ok((Node::decode(&bytes)?, bytes))
    }

    fn insert(&mut self, node: Node) -> Key {
        let bytes = node.encode();
        let id = self.tree.node_hash(&bytes);
        self.writes.insert(id, Some(bytes));
        id
    }

    fn load_optional(&mut self, id: Key) -> Result<Option<(Key, Node)>> {
        if id == self.tree.empty_root() {
            Ok(None)
        } else {
            Ok(Some((id, self.load(&id)?.0)))
        }
    }

    fn branch(&mut self, key: &Key, bits: u8, children: [Key; 2]) -> Key {
        let empty = self.tree.empty_root();
        if children[0] == empty {
            children[1]
        } else if children[1] == empty {
            children[0]
        } else {
            self.insert(Node::Branch {
                prefix: canonical_prefix(key, bits),
                bits,
                children,
            })
        }
    }

    fn update(
        &mut self,
        old: Option<(Key, Node)>,
        changes: &[(Key, Option<Key>)],
        depth: usize,
        previous: &mut impl FnMut(Key, Key, Option<Key>),
    ) -> Result<Key> {
        let empty = self.tree.empty_root();
        let Some((first, _)) = changes.first() else {
            return Ok(old.map_or(empty, |(id, _)| id));
        };
        let last = &changes.last().expect("nonempty changes").0;
        let Some((id, node)) = old else {
            if let Some(bit) = difference(first, last) {
                ensure!(usize::from(bit) >= depth, "invalid Patricia update depth");
                let (left, right) =
                    changes.split_at(changes.partition_point(|(key, _)| side(key, bit) == 0));
                let children = [
                    self.update(None, left, usize::from(bit) + 1, previous)?,
                    self.update(None, right, usize::from(bit) + 1, previous)?,
                ];
                return Ok(self.branch(first, bit, children));
            }
            return Ok(changes[0].1.map_or(empty, |value| {
                self.insert(Node::Leaf { key: *first, value })
            }));
        };
        let (prefix, bits) = node.prefix();
        ensure!(bits >= depth, "invalid Patricia path depth");
        let split = difference(prefix, first)
            .into_iter()
            .chain(difference(prefix, last))
            .min();
        if let Some(bit) = split.filter(|&bit| usize::from(bit) < bits) {
            ensure!(
                usize::from(bit) >= depth,
                "Patricia child escapes its parent"
            );
            let mut children = [empty; 2];
            let old_side = side(prefix, bit);
            let offset = changes.partition_point(|(key, _)| side(key, bit) == 0);
            let (left, right) = changes.split_at(offset);
            let halves = [left, right];
            let depth = usize::from(bit) + 1;
            children[1 - old_side] = self.update(None, halves[1 - old_side], depth, previous)?;
            children[old_side] =
                self.update(Some((id, node)), halves[old_side], depth, previous)?;
            return Ok(self.branch(first, bit, children));
        }
        Ok(match node {
            Node::Leaf { key, value } => {
                previous(key, value, changes[0].1);
                if Some(value) == changes[0].1 {
                    return Ok(id);
                }
                self.writes.insert(id, None);
                changes[0]
                    .1
                    .map_or(empty, |value| self.insert(Node::Leaf { key, value }))
            }
            Node::Branch {
                prefix,
                bits,
                children,
            } => {
                let offset = changes.partition_point(|(key, _)| side(key, bits) == 0);
                let (left, right) = changes.split_at(offset);
                let mut updated = children;
                for (index, edits) in [left, right].into_iter().enumerate() {
                    if !edits.is_empty() {
                        let child = self.load_optional(children[index])?;
                        updated[index] =
                            self.update(child, edits, usize::from(bits) + 1, previous)?;
                    }
                }
                if updated == children {
                    return Ok(id);
                }
                self.writes.insert(id, None);
                self.branch(&prefix, bits, updated)
            }
        })
    }
}

fn key(bytes: &[u8]) -> Key {
    bytes.try_into().expect("32 byte hash")
}

fn difference(left: &Key, right: &Key) -> Option<u8> {
    left.iter()
        .zip(right)
        .enumerate()
        .find_map(|(index, (&left, &right))| {
            (left != right).then(|| (index * 8 + (left ^ right).leading_zeros() as usize) as u8)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_rewrites_shared_paths_once_and_preserves_proofs() -> Result<()> {
        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let keys: Vec<_> = (0..1024u16)
            .map(|i| tree.record_key(&i.to_le_bytes()))
            .collect();
        let old = tree.value_hash(b"old");
        let new = tree.value_hash(b"new");
        let mut genesis = tree.edit(tree.empty_root(), |_| Ok(None));
        for &key in &keys[..700] {
            genesis.set(key, Some(old))?;
        }
        let root = genesis.root();
        let mut storage: BTreeMap<_, _> = genesis
            .into_writes()
            .into_iter()
            .filter_map(|(id, value)| value.map(|bytes| (id, bytes)))
            .collect();
        let mut changes: Vec<_> = keys
            .iter()
            .enumerate()
            .filter_map(|(index, &key)| match index % 5 {
                0 => Some((key, None)),
                1 | 2 => Some((key, Some(new))),
                3 => Some((key, Some(old))),
                _ => None,
            })
            .collect();
        changes.sort_unstable_by_key(|(key, _)| *key);
        let (batch_result, batch_writes) = {
            let mut reads = 0;
            let mut batch = tree.edit(root, |id| {
                reads += 1;
                Ok(storage.get(id).cloned())
            });
            let mut seen = BTreeMap::new();
            batch.set_batch(&changes, |key, before, after| {
                assert!(seen.insert(key, before).is_none());
                assert_eq!(
                    after,
                    changes[changes.binary_search_by_key(&key, |(key, _)| *key).unwrap()].1
                );
            })?;
            let mut sequential_reads = 0;
            let mut sequential = tree.edit(root, |id| {
                sequential_reads += 1;
                Ok(storage.get(id).cloned())
            });
            for &(key, value) in &changes {
                assert_eq!(sequential.set(key, value)?, seen.get(&key).copied());
            }
            let result = batch.root();
            assert_eq!(result, sequential.root());
            let writes = batch.into_writes();
            assert!(writes.len() < sequential.into_writes().len());
            assert!(reads < sequential_reads);
            (result, writes)
        };
        for (id, value) in batch_writes {
            if let Some(bytes) = value {
                storage.insert(id, bytes);
            } else {
                storage.remove(&id);
            }
        }
        let mut verify = tree.edit(batch_result, |id| Ok(storage.get(id).cloned()));
        for (index, key) in keys.iter().enumerate() {
            let expected = match index % 5 {
                0 => None,
                1 | 2 => Some(new),
                3 => Some(old),
                _ => (index < 700).then_some(old),
            };
            let (value, proof) = verify.prove(key)?;
            assert_eq!(value, expected);
            assert!(tree.verify(&batch_result, key, expected.as_ref(), &proof));
        }
        let leaves = (0..keys.len())
            .filter(|index| index % 5 != 0 && (index % 5 != 4 || *index < 700))
            .count();
        assert_eq!(storage.len(), 2 * leaves - 1);
        assert!(
            verify
                .set_batch(&[changes[0], changes[0]], |_, _, _| {})
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn incremental_roots_proofs_and_reclamation() -> Result<()> {
        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let other = Patricia::new(Byte32::default(), Namespace::System);
        let mut storage = BTreeMap::new();
        let keys: Vec<_> = (0..512u16)
            .map(|i| tree.record_key(&i.to_le_bytes()))
            .collect();
        let value = tree.value_hash(b"record");
        let mut first = tree.edit(tree.empty_root(), |_| Ok(None));
        let mut second = tree.edit(tree.empty_root(), |_| Ok(None));
        for (&a, &b) in keys.iter().zip(keys.iter().rev()) {
            assert!(first.set(a, Some(value))?.is_none());
            assert!(second.set(b, Some(value))?.is_none());
        }
        let root = first.root();
        assert_eq!(root, second.root());
        for (id, data) in first.into_writes() {
            storage.extend(data.map(|bytes| (id, bytes)));
        }
        assert_eq!(storage.len(), 2 * keys.len() - 1);
        let absent = tree.record_key(b"absent");
        let mut reads = 0;
        let mut third = tree.edit(root, |id| {
            reads += 1;
            Ok(storage.get(id).cloned())
        });
        let (actual, included) = third.prove(&keys[71])?;
        assert_eq!(actual, Some(value));
        assert!(tree.verify(&root, &keys[71], Some(&value), &included));
        assert!(!tree.verify(&root, &keys[71], None, &included));
        assert!(!other.verify(&root, &keys[71], Some(&value), &included));
        let (actual, excluded) = third.prove(&absent)?;
        assert!(actual.is_none() && tree.verify(&root, &absent, None, &excluded));
        assert_eq!(third.set(keys[71], None)?, Some(value));
        let after = third.root();
        let (_, new_proof) = third.prove(&keys[71])?;
        assert!(tree.verify(&after, &keys[71], None, &new_proof));
        let writes = third.into_writes();
        assert!(reads < 64);
        assert!(writes.len() < 64);
        for (id, data) in writes {
            match data {
                Some(bytes) => {
                    storage.insert(id, bytes);
                }
                None => {
                    storage.remove(&id);
                }
            }
        }
        assert_eq!(storage.len(), 2 * (keys.len() - 1) - 1);
        let mut final_edit = tree.edit(after, |id| Ok(storage.get(id).cloned()));
        for &id in &keys {
            final_edit.set(id, None)?;
        }
        assert_eq!(final_edit.root(), tree.empty_root());
        assert!(final_edit.into_writes().values().all(Option::is_none));
        Ok(())
    }
}
