//! Ordered Merkle trees for witnessed transactions and execution receipts.

use anyhow::{Result, ensure};
use ckb_types::{packed::Byte32, prelude::Entity};

use crate::{
    Transaction,
    block::{ExecutionCommitment, Receipt},
    hash,
};

const TX_ROOT: &[u8] = b"Fern transaction Merkle root\0";
const RECEIPT_ROOT: &[u8] = b"Fern receipt Merkle root\0";

pub struct Tree {
    network: Byte32,
    domain: &'static [u8],
    leaves: Vec<Byte32>,
}

impl Tree {
    pub fn transactions(network: Byte32, transactions: &[Transaction]) -> Result<Self> {
        Self::new(
            network.clone(),
            TX_ROOT,
            transactions
                .iter()
                .map(|tx| tx.commitment(&network))
                .collect(),
        )
    }

    pub fn receipts(network: Byte32, receipts: &[Receipt]) -> Result<Self> {
        Self::new(
            network.clone(),
            RECEIPT_ROOT,
            receipts
                .iter()
                .map(|receipt| receipt_hash(&network, receipt))
                .collect(),
        )
    }

    fn new(network: Byte32, domain: &'static [u8], leaves: Vec<Byte32>) -> Result<Self> {
        ensure!(
            u32::try_from(leaves.len()).is_ok(),
            "too many Merkle leaves"
        );
        Ok(Self {
            network,
            domain,
            leaves,
        })
    }

    pub fn root(&self) -> Byte32 {
        bind_root(
            self.domain,
            &self.network,
            self.leaves.len() as u32,
            &merkle_root(&self.network, &self.leaves),
        )
    }

    pub fn prove(&self, position: u32) -> Result<InclusionProof> {
        ensure!(
            (position as usize) < self.leaves.len(),
            "Merkle position is out of bounds"
        );
        let mut leaves = self.leaves.as_slice();
        let mut relative = position as usize;
        let mut siblings = Vec::new();
        while leaves.len() > 1 {
            let split = split_point(leaves.len());
            if relative < split {
                siblings.push(merkle_root(&self.network, &leaves[split..]));
                leaves = &leaves[..split];
            } else {
                siblings.push(merkle_root(&self.network, &leaves[..split]));
                leaves = &leaves[split..];
                relative -= split;
            }
        }
        Ok(InclusionProof {
            position,
            count: self.leaves.len() as u32,
            siblings,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InclusionProof {
    pub position: u32,
    pub count: u32,
    /// Siblings from the root towards the leaf, excluding the leaf itself.
    pub siblings: Vec<Byte32>,
}

impl InclusionProof {
    pub fn verify_transaction(&self, network: &Byte32, root: &Byte32, tx: &Transaction) -> bool {
        self.verify(TX_ROOT, network, root, tx.commitment(network))
    }

    pub fn verify_execution(
        &self,
        network: &Byte32,
        roots: &ExecutionCommitment,
        tx: &Transaction,
        receipt_proof: &Self,
        receipt: &Receipt,
    ) -> bool {
        self.position == receipt_proof.position
            && self.count == receipt_proof.count
            && self.verify_transaction(network, &roots.transaction_root, tx)
            && receipt_proof.verify(
                RECEIPT_ROOT,
                network,
                &roots.receipt_root,
                receipt_hash(network, receipt),
            )
    }

    fn verify(&self, domain: &[u8], network: &Byte32, root: &Byte32, mut leaf: Byte32) -> bool {
        if self.position >= self.count || self.siblings.len() > 32 {
            return false;
        }
        let mut position = self.position as usize;
        let mut count = self.count as usize;
        let mut right_branches = Vec::new();
        while count > 1 {
            let split = split_point(count);
            let right = position >= split;
            right_branches.push(right);
            if right {
                position -= split;
                count -= split;
            } else {
                count = split;
            }
        }
        if right_branches.len() != self.siblings.len() {
            return false;
        }
        for (right, sibling) in right_branches.into_iter().zip(&self.siblings).rev() {
            leaf = if right {
                merge(network, sibling, &leaf)
            } else {
                merge(network, &leaf, sibling)
            };
        }
        bind_root(domain, network, self.count, &leaf) == *root
    }
}

fn receipt_hash(network: &Byte32, receipt: &Receipt) -> Byte32 {
    hash(b"Fern execution receipt\0", network, &[&receipt.encode()])
}

fn merkle_root(network: &Byte32, leaves: &[Byte32]) -> Byte32 {
    match leaves {
        [] => hash(b"Fern empty Merkle tree\0", network, &[]),
        [leaf] => leaf.clone(),
        _ => {
            let (left, right) = leaves.split_at(split_point(leaves.len()));
            merge(
                network,
                &merkle_root(network, left),
                &merkle_root(network, right),
            )
        }
    }
}

fn split_point(count: usize) -> usize {
    1usize << (usize::BITS - (count - 1).leading_zeros() - 1)
}

fn merge(network: &Byte32, left: &Byte32, right: &Byte32) -> Byte32 {
    hash(
        b"Fern Merkle node\0",
        network,
        &[left.as_slice(), right.as_slice()],
    )
}

fn bind_root(domain: &[u8], network: &Byte32, count: u32, root: &Byte32) -> Byte32 {
    hash(domain, network, &[&count.to_le_bytes(), root.as_slice()])
}
