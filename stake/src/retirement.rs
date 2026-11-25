//! Permanent retirement markers, committed separately from live Stake Cells.

use alloc::vec::Vec;

use crate::{
    Error, Hash32, Result, ValidatorKey, digest,
    patricia::{Commitment, Namespace},
    sparse::{Domain, Tree},
};

pub use crate::sparse::EMPTY;

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

fn domain(network: Hash32) -> Domain {
    Domain {
        network,
        node: b"Stem CKB retirement node\0",
    }
}

/// Verify canonical non-inclusion and compute the root after retiring the key.
pub fn insert(
    network: &Hash32,
    validator: &ValidatorKey,
    root: &Hash32,
    proof: &[u8],
) -> Result<Hash32> {
    domain(*network).update(validator, root, EMPTY, leaf(network, validator), proof)
}

/// Rebuildable proof index for L1 transaction construction.
pub struct RetirementTree {
    network: Hash32,
    tree: Tree,
}

impl RetirementTree {
    pub fn new(network: Hash32) -> Self {
        Self {
            network,
            tree: Tree::new(domain(network)),
        }
    }

    pub fn root(&self) -> Hash32 {
        self.tree.root()
    }

    pub fn prove_absence(&self, validator: &ValidatorKey) -> Result<Vec<u8>> {
        if self.tree.value(validator) != EMPTY {
            return Err(Error::Sequence);
        }
        Ok(self.tree.proof(validator))
    }

    pub fn retire(&mut self, validator: ValidatorKey) -> Result<()> {
        if self.tree.value(&validator) != EMPTY {
            return Err(Error::Sequence);
        }
        self.tree.set(validator, leaf(&self.network, &validator));
        Ok(())
    }
}
