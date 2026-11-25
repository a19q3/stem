//! Permanent retirement markers, committed separately from live Stake Cells.

use alloc::{collections::BTreeMap, vec::Vec};

use crate::{
    Error, Hash32, Result, ValidatorKey, digest,
    patricia::{Commitment, Namespace},
};

pub const EMPTY: Hash32 = [0; 32];
const DEPTH: usize = 256;
const BITMAP: usize = DEPTH / 8;

pub fn marker_key(network: &Hash32, registry: &Hash32, validator: &ValidatorKey) -> Hash32 {
    digest(b"Stem retired stake\0", &[network, registry, validator])
}

/// The Stem kernel writes a permanent marker during the snapshot signing
/// window. It can only authorize L1 retirement after the handoff is active.
pub fn verify_marker<'a>(
    network: &Hash32,
    registry: &Hash32,
    validator: &ValidatorKey,
    effective: u64,
    finalized_height: u64,
    root: &Hash32,
    nodes: impl ExactSizeIterator<Item = &'a [u8]>,
) -> Result<()> {
    if effective == 0 || effective > finalized_height {
        return Err(Error::Epoch);
    }
    let tree = Commitment::new(*network, Namespace::System);
    let key = tree.record_key(&marker_key(network, registry, validator));
    let value = tree.value_hash(&effective.to_le_bytes());
    if !tree.verify(root, &key, Some(&value), nodes) {
        return Err(Error::Anchor);
    }
    Ok(())
}

fn leaf(network: &Hash32, validator: &ValidatorKey) -> Hash32 {
    digest(b"Stem CKB retired validator\0", &[network, validator])
}

fn join(network: &Hash32, level: usize, left: Hash32, right: Hash32) -> Hash32 {
    if left == EMPTY && right == EMPTY {
        return EMPTY;
    }
    digest(
        b"Stem CKB retirement node\0",
        &[network, &(level as u16).to_le_bytes(), &left, &right],
    )
}

fn children(network: &Hash32, level: usize, right: bool, path: Hash32, sibling: Hash32) -> Hash32 {
    if right {
        join(network, level, sibling, path)
    } else {
        join(network, level, path, sibling)
    }
}

fn bit(level: usize) -> (usize, u8) {
    (level / 8, 1 << (level % 8))
}

/// Verify canonical non-inclusion and compute the root after retiring the key.
pub fn insert(
    network: &Hash32,
    validator: &ValidatorKey,
    root: &Hash32,
    proof: &[u8],
) -> Result<Hash32> {
    let (bitmap, mut tail) = proof.split_at_checked(BITMAP).ok_or(Error::Encoding)?;
    let mut before = EMPTY;
    let mut after = leaf(network, validator);
    for level in 0..DEPTH {
        let (byte, bit) = bit(level);
        let sibling = if bitmap[byte] & bit == 0 {
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
        let right = validator[byte] & bit != 0;
        before = children(network, level, right, before, sibling);
        after = children(network, level, right, after, sibling);
    }
    if !tail.is_empty() || before != *root {
        return Err(Error::Sequence);
    }
    Ok(after)
}

/// Rebuildable proof index for L1 transaction construction.
pub struct RetirementTree {
    network: Hash32,
    nodes: BTreeMap<(usize, Hash32), Hash32>,
}

impl RetirementTree {
    pub fn new(network: Hash32) -> Self {
        Self {
            network,
            nodes: BTreeMap::new(),
        }
    }

    pub fn root(&self) -> Hash32 {
        self.node(DEPTH, [0; 32])
    }

    fn node(&self, level: usize, position: Hash32) -> Hash32 {
        self.nodes.get(&(level, position)).copied().unwrap_or(EMPTY)
    }

    pub fn prove_absence(&self, validator: &ValidatorKey) -> Result<Vec<u8>> {
        if self.node(0, *validator) != EMPTY {
            return Err(Error::Sequence);
        }
        let mut proof = alloc::vec![0; BITMAP];
        let mut position = *validator;
        for level in 0..DEPTH {
            let (byte, bit) = bit(level);
            let mut sibling = position;
            sibling[byte] ^= bit;
            let node = self.node(level, sibling);
            if node != EMPTY {
                proof[byte] |= bit;
                proof.extend_from_slice(&node);
            }
            position[byte] &= !bit;
        }
        Ok(proof)
    }

    pub fn retire(&mut self, validator: ValidatorKey) -> Result<()> {
        if self.node(0, validator) != EMPTY {
            return Err(Error::Sequence);
        }
        let mut root = leaf(&self.network, &validator);
        let mut position = validator;
        self.nodes.insert((0, position), root);
        for level in 0..DEPTH {
            let (byte, bit) = bit(level);
            let mut sibling = position;
            sibling[byte] ^= bit;
            let other = self.node(level, sibling);
            root = children(&self.network, level, position[byte] & bit != 0, root, other);
            position[byte] &= !bit;
            self.nodes.insert((level + 1, position), root);
        }
        Ok(())
    }
}
