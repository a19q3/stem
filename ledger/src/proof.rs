//! Ordered Merkle trees for witnessed transactions and execution receipts.

use anyhow::{Result, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::*};

use crate::{
    Transaction,
    block::{ExecutionCommitment, Receipt},
    hash_bytes,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

const TX_ROOT: &[u8] = b"Stem transaction Merkle root\0";
const RECEIPT_ROOT: &[u8] = b"Stem receipt Merkle root\0";
type Digest = [u8; 32];

pub struct Tree {
    network: Byte32,
    domain: &'static [u8],
    leaves: Vec<Digest>,
}

impl Tree {
    pub fn transactions(network: Byte32, transactions: &[Transaction]) -> Result<Self> {
        check_leaf_count(transactions.len())?;
        let leaves = transactions
            .iter()
            .map(|tx| tx.commitment_bytes(&network))
            .collect();
        Ok(Self {
            network,
            domain: TX_ROOT,
            leaves,
        })
    }

    pub fn receipts(network: Byte32, receipts: &[Receipt]) -> Result<Self> {
        Self::receipts_with(network, receipts, |_, _| Ok(()))
    }

    /// Hash each canonical receipt and make its encoded bytes available to storage.
    pub fn receipts_with(
        network: Byte32,
        receipts: &[Receipt],
        mut record: impl FnMut(usize, &[u8]) -> Result<()>,
    ) -> Result<Self> {
        check_leaf_count(receipts.len())?;
        let mut leaves = Vec::with_capacity(receipts.len());
        for (position, receipt) in receipts.iter().enumerate() {
            let encoded = receipt.encode();
            leaves.push(receipt_hash(&network, &encoded));
            record(position, &encoded)?;
        }
        Ok(Self {
            network,
            domain: RECEIPT_ROOT,
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
        .pack()
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
                siblings.push(merkle_root(&self.network, &leaves[split..]).pack());
                leaves = &leaves[..split];
            } else {
                siblings.push(merkle_root(&self.network, &leaves[..split]).pack());
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
    pub fn encode_execution_pair(&self, receipt: &Self) -> Result<Bytes> {
        ensure!(
            self.position == receipt.position && self.count == receipt.count,
            "execution proof positions differ"
        );
        let (_, depth) = proof_path(self.position, self.count)
            .ok_or_else(|| anyhow::anyhow!("invalid proof position"))?;
        ensure!(
            self.siblings.len() == depth && receipt.siblings.len() == depth,
            "invalid execution proof depth"
        );
        let siblings = |items: &[Byte32]| {
            wire::Byte32Vec::new_builder()
                .set(
                    items
                        .iter()
                        .map(|hash| wire::Byte32::new_unchecked(hash.as_bytes()))
                        .collect(),
                )
                .build()
        };
        Ok(wire::ExecutionProof::new_builder()
            .position(
                wire::CompactUint32::new_builder()
                    .set(self.position.into())
                    .build(),
            )
            .count(
                wire::CompactUint32::new_builder()
                    .set(self.count.into())
                    .build(),
            )
            .transaction_siblings(siblings(&self.siblings))
            .receipt_siblings(siblings(&receipt.siblings))
            .build()
            .as_bytes())
    }

    pub fn decode_execution_pair(bytes: &[u8]) -> Result<(Self, Self)> {
        ensure!(
            bytes.len() <= 2 * 5 + 2 * (4 + 32 * 32),
            "oversized execution proof"
        );
        let proof = wire::ExecutionProof::from_slice(bytes)?;
        let position = proof.position().as_reader().value().try_into()?;
        let count = proof.count().as_reader().value().try_into()?;
        let (_, depth) =
            proof_path(position, count).ok_or_else(|| anyhow::anyhow!("invalid proof position"))?;
        let decode = |siblings: wire::Byte32Vec| -> Result<Self> {
            ensure!(siblings.len() == depth, "invalid execution proof depth");
            Ok(Self {
                position,
                count,
                siblings: siblings
                    .into_iter()
                    .map(|item| Byte32::new_unchecked(item.as_bytes()))
                    .collect(),
            })
        };
        Ok((
            decode(proof.transaction_siblings())?,
            decode(proof.receipt_siblings())?,
        ))
    }

    pub fn verify_transaction(&self, network: &Byte32, root: &Byte32, tx: &Transaction) -> bool {
        self.verify(TX_ROOT, network, root, tx.commitment_bytes(network))
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
                receipt_hash(network, &receipt.encode()),
            )
    }

    fn verify(&self, domain: &[u8], network: &Byte32, root: &Byte32, mut leaf: Digest) -> bool {
        let Some((directions, depth)) = proof_path(self.position, self.count) else {
            return false;
        };
        if depth != self.siblings.len() {
            return false;
        }
        for (step, sibling) in self.siblings.iter().enumerate().rev() {
            leaf = if directions & (1 << step) != 0 {
                merge(network, sibling.as_slice(), &leaf)
            } else {
                merge(network, &leaf, sibling.as_slice())
            };
        }
        bind_root(domain, network, self.count, &leaf) == root.as_slice()
    }
}

fn proof_path(position: u32, count: u32) -> Option<(u32, usize)> {
    if position >= count {
        return None;
    }
    let (mut position, mut count) = (position as usize, count as usize);
    let mut directions = 0;
    let mut depth = 0;
    while count > 1 {
        let split = split_point(count);
        if position >= split {
            directions |= 1 << depth;
            position -= split;
            count -= split;
        } else {
            count = split;
        }
        depth += 1;
    }
    Some((directions, depth))
}

fn check_leaf_count(count: usize) -> Result<()> {
    ensure!(u32::try_from(count).is_ok(), "too many Merkle leaves");
    Ok(())
}

fn receipt_hash(network: &Byte32, encoded: &[u8]) -> Digest {
    hash_bytes(b"Stem execution receipt\0", network, &[encoded])
}

fn merkle_root(network: &Byte32, leaves: &[Digest]) -> Digest {
    match leaves {
        [] => hash_bytes(b"Stem empty Merkle tree\0", network, &[]),
        [leaf] => *leaf,
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

fn merge(network: &Byte32, left: &[u8], right: &[u8]) -> Digest {
    hash_bytes(b"Stem Merkle node\0", network, &[left, right])
}

fn bind_root(domain: &[u8], network: &Byte32, count: u32, root: &Digest) -> Digest {
    hash_bytes(domain, network, &[&count.to_le_bytes(), root])
}
