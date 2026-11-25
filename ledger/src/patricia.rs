//! Incremental authenticated state. Node values contain only record hashes.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::Entity};

use crate::{
    hash,
    telegraph::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

pub type Key = [u8; 32];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespace {
    Cells,
    System,
}

impl Namespace {
    pub fn tag(self) -> u8 {
        match self {
            Self::Cells => 0,
            Self::System => 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Leaf {
        key: Key,
        value: Key,
    },
    Branch {
        prefix: Key,
        bits: u8,
        children: [Key; 2],
    },
}

impl Node {
    fn encode(&self) -> Vec<u8> {
        match self {
            Self::Leaf { key, value } => {
                let mut bytes = Vec::with_capacity(65);
                bytes.push(0);
                bytes.extend_from_slice(key);
                bytes.extend_from_slice(value);
                bytes
            }
            Self::Branch {
                prefix,
                bits,
                children,
            } => {
                let mut bytes = Vec::with_capacity(98);
                bytes.extend_from_slice(&[1, *bits]);
                bytes.extend_from_slice(prefix);
                bytes.extend_from_slice(&children[0]);
                bytes.extend_from_slice(&children[1]);
                bytes
            }
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(match bytes {
            [0, body @ ..] if body.len() == 64 => Self::Leaf {
                key: key(&body[..32]),
                value: key(&body[32..]),
            },
            [1, bits, body @ ..] if body.len() == 96 => {
                let prefix = key(&body[..32]);
                let children = [key(&body[32..64]), key(&body[64..])];
                ensure!(
                    canonical_prefix(&prefix, *bits) == prefix,
                    "invalid Patricia prefix"
                );
                ensure!(
                    children[0] != children[1],
                    "duplicate Patricia branch children"
                );
                Self::Branch {
                    prefix,
                    bits: *bits,
                    children,
                }
            }
            _ => bail!("invalid Patricia node encoding"),
        })
    }

    fn prefix(&self) -> (&Key, usize) {
        match self {
            Self::Leaf { key, .. } => (key, 256),
            Self::Branch { prefix, bits, .. } => (prefix, usize::from(*bits)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Patricia {
    network: Byte32,
    namespace: Namespace,
    empty: Key,
}

impl Patricia {
    pub fn new(network: Byte32, namespace: Namespace) -> Self {
        let empty = key(hash(
            b"Stem empty Patricia tree\0",
            &network,
            &[&[namespace.tag()]],
        )
        .as_slice());
        Self {
            network,
            namespace,
            empty,
        }
    }

    pub fn empty_root(&self) -> Key {
        self.empty
    }

    fn node_hash(&self, encoded: &[u8]) -> Key {
        key(hash(
            b"Stem Patricia node\0",
            &self.network,
            &[&[self.namespace.tag()], encoded],
        )
        .as_slice())
    }

    pub fn value_hash(&self, bytes: &[u8]) -> Key {
        key(hash(
            b"Stem state record\0",
            &self.network,
            &[&[self.namespace.tag()], bytes],
        )
        .as_slice())
    }

    pub fn record_key(&self, name: &[u8]) -> Key {
        key(hash(
            b"Stem state key\0",
            &self.network,
            &[&[self.namespace.tag()], name],
        )
        .as_slice())
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
        if proof.nodes.is_empty() {
            return expected.is_none() && *root == self.empty_root();
        }
        if proof.nodes.len() > 257 || *root == self.empty_root() {
            return false;
        }
        let mut next = *root;
        let mut depth = 0;
        for (index, bytes) in proof.nodes.iter().enumerate() {
            if self.node_hash(bytes) != next {
                return false;
            }
            let Ok(node) = Node::decode(bytes) else {
                return false;
            };
            let terminal = index + 1 == proof.nodes.len();
            match node {
                Node::Leaf { key, value } => {
                    return terminal
                        && if key == *query {
                            expected == Some(&value)
                        } else {
                            expected.is_none()
                        };
                }
                Node::Branch {
                    prefix,
                    bits,
                    children,
                } => {
                    if usize::from(bits) < depth {
                        return false;
                    }
                    if canonical_prefix(query, bits) != prefix {
                        return terminal && expected.is_none();
                    }
                    next = children[side(query, bits)];
                    depth = usize::from(bits) + 1;
                }
            }
        }
        false
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// The path from the root to the matching leaf or the point of divergence.
    pub nodes: Vec<Vec<u8>>,
}

impl Proof {
    pub fn encode(&self) -> Result<Bytes> {
        ensure!(self.nodes.len() <= 257, "oversized Patricia path");
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
        ensure!(
            bytes.len() <= 4 + 257 * (4 + 98),
            "oversized Patricia proof"
        );
        let encoded = wire::PatriciaPath::from_slice(bytes)?;
        ensure!(encoded.len() <= 257, "oversized Patricia path");
        let nodes = encoded
            .into_iter()
            .map(|node| {
                let raw = node.raw_data();
                Node::decode(&raw)?;
                Ok(raw.to_vec())
            })
            .collect::<Result<_>>()?;
        Ok(Self { nodes })
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
        let (root, previous) = self.update(self.root, key, value, 0)?;
        self.root = root;
        Ok(previous)
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

    fn update(
        &mut self,
        id: Key,
        key: Key,
        value: Option<Key>,
        depth: usize,
    ) -> Result<(Key, Option<Key>)> {
        let empty = self.tree.empty_root();
        if id == empty {
            return Ok((
                value.map_or(empty, |value| self.insert(Node::Leaf { key, value })),
                None,
            ));
        }
        let (node, _) = self.load(&id)?;
        let (prefix, bits) = node.prefix();
        ensure!(bits >= depth, "invalid Patricia path depth");
        if let Some(bit) = difference(prefix, &key).filter(|&bit| usize::from(bit) < bits) {
            let Some(value) = value else {
                return Ok((id, None));
            };
            let leaf = self.insert(Node::Leaf { key, value });
            let mut children = [id; 2];
            children[side(&key, bit)] = leaf;
            let branch = self.insert(Node::Branch {
                prefix: canonical_prefix(&key, bit),
                bits: bit,
                children,
            });
            return Ok((branch, None));
        }
        match node {
            Node::Leaf {
                value: previous, ..
            } => {
                if Some(previous) == value {
                    return Ok((id, Some(previous)));
                }
                self.writes.insert(id, None);
                let next = value.map_or(empty, |value| self.insert(Node::Leaf { key, value }));
                Ok((next, Some(previous)))
            }
            Node::Branch {
                prefix,
                bits,
                mut children,
            } => {
                let side = side(&key, bits);
                let (child, previous) =
                    self.update(children[side], key, value, usize::from(bits) + 1)?;
                if child == children[side] {
                    return Ok((id, previous));
                }
                self.writes.insert(id, None);
                if child == empty {
                    return Ok((children[1 - side], previous));
                }
                children[side] = child;
                Ok((
                    self.insert(Node::Branch {
                        prefix,
                        bits,
                        children,
                    }),
                    previous,
                ))
            }
        }
    }
}

fn key(bytes: &[u8]) -> Key {
    bytes.try_into().expect("32 byte hash")
}

fn side(key: &Key, bit: u8) -> usize {
    usize::from((key[usize::from(bit / 8)] >> (7 - bit % 8)) & 1)
}

fn canonical_prefix(key: &Key, bits: u8) -> Key {
    let mut prefix = *key;
    let offset = usize::from(bits / 8);
    prefix[offset] &= !(0xff >> (bits % 8));
    prefix[offset + 1..].fill(0);
    prefix
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
