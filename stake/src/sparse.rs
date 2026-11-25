//! Bounded bitmap proofs for mutable L1 registry commitments.

use alloc::{collections::BTreeMap, vec::Vec};

use crate::{Error, Hash32, Result, digest};

pub const EMPTY: Hash32 = [0; 32];
const DEPTH: usize = 256;
const BITMAP: usize = DEPTH / 8;

#[derive(Clone, Copy)]
pub(crate) struct Domain {
    pub network: Hash32,
    pub node: &'static [u8],
}

impl Domain {
    fn join(self, level: usize, key: &Hash32, path: Hash32, sibling: Hash32) -> Hash32 {
        let (byte, mask) = bit(level);
        let (left, right) = if key[byte] & mask == 0 {
            (path, sibling)
        } else {
            (sibling, path)
        };
        if left == EMPTY && right == EMPTY {
            return EMPTY;
        }
        digest(
            self.node,
            &[&self.network, &(level as u16).to_le_bytes(), &left, &right],
        )
    }

    pub fn update(
        self,
        key: &Hash32,
        root: &Hash32,
        mut before: Hash32,
        mut after: Hash32,
        proof: &[u8],
    ) -> Result<Hash32> {
        let (bitmap, mut tail) = proof.split_at_checked(BITMAP).ok_or(Error::Encoding)?;
        for level in 0..DEPTH {
            let (byte, mask) = bit(level);
            let sibling = if bitmap[byte] & mask == 0 {
                EMPTY
            } else {
                let (raw, rest) = tail.split_at_checked(32).ok_or(Error::Encoding)?;
                tail = rest;
                let sibling = raw.try_into().expect("32-byte sibling");
                if sibling == EMPTY {
                    return Err(Error::Encoding);
                }
                sibling
            };
            let next_before = self.join(level, key, before, sibling);
            after = if before == after {
                next_before
            } else {
                self.join(level, key, after, sibling)
            };
            before = next_before;
        }
        if !tail.is_empty() || before != *root {
            return Err(Error::Sequence);
        }
        Ok(after)
    }
}

fn bit(level: usize) -> (usize, u8) {
    (level / 8, 1 << (level % 8))
}

/// A rebuildable index. The on-chain verifier needs only a root and the bitmap proof.
pub(crate) struct Tree {
    domain: Domain,
    nodes: BTreeMap<(usize, Hash32), Hash32>,
}

impl Tree {
    pub fn new(domain: Domain) -> Self {
        Self {
            domain,
            nodes: BTreeMap::new(),
        }
    }

    pub fn root(&self) -> Hash32 {
        self.node(DEPTH, [0; 32])
    }

    fn node(&self, level: usize, position: Hash32) -> Hash32 {
        self.nodes.get(&(level, position)).copied().unwrap_or(EMPTY)
    }

    pub fn value(&self, key: &Hash32) -> Hash32 {
        self.node(0, *key)
    }

    pub fn proof(&self, key: &Hash32) -> Vec<u8> {
        let mut proof = alloc::vec![0; BITMAP];
        let mut position = *key;
        for level in 0..DEPTH {
            let (byte, mask) = bit(level);
            let mut sibling = position;
            sibling[byte] ^= mask;
            let node = self.node(level, sibling);
            if node != EMPTY {
                proof[byte] |= mask;
                proof.extend_from_slice(&node);
            }
            position[byte] &= !mask;
        }
        proof
    }

    pub fn set(&mut self, key: Hash32, mut leaf: Hash32) {
        let mut position = key;
        for level in 0..=DEPTH {
            if leaf == EMPTY {
                self.nodes.remove(&(level, position));
            } else {
                self.nodes.insert((level, position), leaf);
            }
            if level < DEPTH {
                let (byte, mask) = bit(level);
                let mut sibling = position;
                sibling[byte] ^= mask;
                leaf = self
                    .domain
                    .join(level, &key, leaf, self.node(level, sibling));
                position[byte] &= !mask;
            }
        }
    }
}
