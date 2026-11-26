//! Canonical Patricia commitments and paths shared by Stem and CKB contracts.

use alloc::vec::Vec;

use crate::{Error, Hash32, Result, digest, digest_parts};

pub type Key = Hash32;
pub const MAX_NODES: usize = 257;
pub const MAX_PATH_BYTES: usize = 4 + MAX_NODES * (4 + 98);

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
pub enum Node {
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
    pub fn encode(&self) -> Vec<u8> {
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

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(match bytes {
            [0, body @ ..] if body.len() == 64 => Self::Leaf {
                key: key(&body[..32]),
                value: key(&body[32..]),
            },
            [1, bits, body @ ..] if body.len() == 96 => {
                let prefix = key(&body[..32]);
                let children = [key(&body[32..64]), key(&body[64..])];
                if canonical_prefix(&prefix, *bits) != prefix || children[0] == children[1] {
                    return Err(Error::Encoding);
                }
                Self::Branch {
                    prefix,
                    bits: *bits,
                    children,
                }
            }
            _ => return Err(Error::Encoding),
        })
    }

    pub fn prefix(&self) -> (&Key, usize) {
        match self {
            Self::Leaf { key, .. } => (key, 256),
            Self::Branch { prefix, bits, .. } => (prefix, usize::from(*bits)),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Commitment {
    network: Key,
    namespace: Namespace,
    empty: Key,
}

impl Commitment {
    pub fn new(network: Key, namespace: Namespace) -> Self {
        let empty = digest(
            b"Stem empty Patricia tree\0",
            &[&network, &[namespace.tag()]],
        );
        Self {
            network,
            namespace,
            empty,
        }
    }

    pub fn empty_root(&self) -> Key {
        self.empty
    }

    pub fn node_hash(&self, encoded: &[u8]) -> Key {
        digest(
            b"Stem Patricia node\0",
            &[&self.network, &[self.namespace.tag()], encoded],
        )
    }

    pub fn value_hash(&self, bytes: &[u8]) -> Key {
        self.value_hash_parts(&[bytes])
    }

    /// Hash the concatenated canonical record without assembling a buffer.
    pub fn value_hash_parts(&self, parts: &[&[u8]]) -> Key {
        let namespace = [self.namespace.tag()];
        digest_parts(
            b"Stem state record\0",
            [self.network.as_slice(), namespace.as_slice()]
                .into_iter()
                .chain(parts.iter().copied()),
        )
    }

    pub fn record_key(&self, name: &[u8]) -> Key {
        digest(
            b"Stem state key\0",
            &[&self.network, &[self.namespace.tag()], name],
        )
    }

    pub fn verify<'a>(
        &self,
        root: &Key,
        query: &Key,
        expected: Option<&Key>,
        nodes: impl ExactSizeIterator<Item = &'a [u8]>,
    ) -> bool {
        let count = nodes.len();
        if count == 0 {
            return expected.is_none() && *root == self.empty_root();
        }
        if count > MAX_NODES || *root == self.empty_root() {
            return false;
        }
        let mut next = *root;
        let mut depth = 0;
        for (index, bytes) in nodes.enumerate() {
            if self.node_hash(bytes) != next {
                return false;
            }
            let Ok(node) = Node::decode(bytes) else {
                return false;
            };
            let terminal = index + 1 == count;
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

fn key(bytes: &[u8]) -> Key {
    bytes.try_into().expect("32 byte hash")
}

pub fn side(key: &Key, bit: u8) -> usize {
    usize::from((key[usize::from(bit / 8)] >> (7 - bit % 8)) & 1)
}

pub fn canonical_prefix(key: &Key, bits: u8) -> Key {
    let mut prefix = *key;
    let offset = usize::from(bits / 8);
    prefix[offset] &= !(0xff >> (bits % 8));
    prefix[offset + 1..].fill(0);
    prefix
}

/// Decode the canonical Strand PatriciaPath without copying individual nodes.
pub fn decode_path(mut encoded: &[u8]) -> Result<Vec<&[u8]>> {
    fn take_usize(bytes: &mut &[u8]) -> Result<usize> {
        let (raw, rest) = bytes.split_at_checked(4).ok_or(Error::Encoding)?;
        *bytes = rest;
        Ok(u32::from_le_bytes(raw.try_into().unwrap()) as usize)
    }
    let count = take_usize(&mut encoded)?;
    if count > MAX_NODES || count > encoded.len() / 69 {
        return Err(Error::Encoding);
    }
    let mut nodes = Vec::with_capacity(count);
    for _ in 0..count {
        let length = take_usize(&mut encoded)?;
        let (node, rest) = encoded.split_at_checked(length).ok_or(Error::Encoding)?;
        Node::decode(node)?;
        nodes.push(node);
        encoded = rest;
    }
    if !encoded.is_empty() {
        return Err(Error::Encoding);
    }
    Ok(nodes)
}
