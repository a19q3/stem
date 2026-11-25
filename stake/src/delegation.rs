//! The Registry authenticates each validator's active delegation principal.

use alloc::vec::Vec;

use crate::{
    Error, Hash32, Result, ValidatorKey, digest,
    l1::DelegationId,
    sparse::{Domain, EMPTY, Tree},
};

fn domain(network: Hash32) -> Domain {
    Domain {
        network,
        node: b"Stem CKB delegation principal node\0",
    }
}

fn leaf(network: &Hash32, position: &Hash32, amount: u64) -> Hash32 {
    if amount == 0 {
        return EMPTY;
    }
    digest(
        b"Stem CKB delegation principal\0",
        &[network, position, &amount.to_le_bytes()],
    )
}

pub fn verify(
    network: &Hash32,
    id: DelegationId,
    root: &Hash32,
    amount: u64,
    proof: &[u8],
) -> Result<()> {
    if amount == 0 {
        return Err(Error::Encoding);
    }
    let position = id.commitment();
    let leaf = leaf(network, &position, amount);
    domain(*network).update(&position, root, leaf, leaf, proof)?;
    Ok(())
}

pub struct PrincipalChange<'a> {
    pub id: DelegationId,
    pub before: u64,
    pub after: u64,
    pub proof: &'a [u8],
}

impl PrincipalChange<'_> {
    pub fn apply(&self, network: &Hash32, root: &Hash32) -> Result<Hash32> {
        if self.before == self.after {
            if !self.proof.is_empty() {
                return Err(Error::Encoding);
            }
            return Ok(*root);
        }
        let position = self.id.commitment();
        domain(*network).update(
            &position,
            root,
            leaf(network, &position, self.before),
            leaf(network, &position, self.after),
            self.proof,
        )
    }
}

/// Rebuilds proofs from confirmed CKB positions for one validator.
pub struct DelegationTree {
    network: Hash32,
    registry: Hash32,
    validator: ValidatorKey,
    tree: Tree,
}

impl DelegationTree {
    pub fn new(network: Hash32, registry: Hash32, validator: ValidatorKey) -> Self {
        Self {
            network,
            registry,
            validator,
            tree: Tree::new(domain(network)),
        }
    }

    pub fn root(&self) -> Hash32 {
        self.tree.root()
    }

    pub fn insert(&mut self, id: DelegationId, amount: u64) -> Result<()> {
        let position = id.commitment();
        if id.registry != self.registry || id.validator != self.validator {
            return Err(Error::Identity);
        }
        if amount == 0 || self.tree.value(&position) != EMPTY {
            return Err(Error::Sequence);
        }
        self.tree
            .set(position, leaf(&self.network, &position, amount));
        Ok(())
    }

    pub fn prove(&self, position: &Hash32) -> Vec<u8> {
        self.tree.proof(position)
    }
}
