//! Network-bound transaction and Live Cell commitments.

use anyhow::{Context, Result, ensure};
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::Entity,
    utilities::{CBMT, MerkleProof, merkle_root},
};

use crate::{Cell, Transaction, hash};

const TX_ROOT: &[u8] = b"Fern transaction Merkle root\0";
const CELL_ROOT: &[u8] = b"Fern live Cell Merkle root\0";

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

    /// Entries are ordered lexicographically by their canonical OutPoint bytes.
    pub fn live_cells<'a>(
        network: Byte32,
        cells: impl IntoIterator<Item = (&'a OutPoint, &'a Cell)>,
    ) -> Result<Self> {
        let mut entries: Vec<_> = cells.into_iter().collect();
        entries.sort_unstable_by(|(a, _), (b, _)| a.as_slice().cmp(b.as_slice()));
        ensure!(
            entries.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "duplicate Live Cell OutPoint"
        );
        let leaves = entries
            .iter()
            .map(|(point, cell)| cell.commitment(&network, point))
            .collect();
        Self::new(network, CELL_ROOT, leaves)
    }

    fn new(network: Byte32, domain: &'static [u8], leaves: Vec<Byte32>) -> Result<Self> {
        ensure!(
            leaves.len() <= u32::MAX as usize / 2,
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
            &merkle_root(&self.leaves),
        )
    }

    pub fn prove(&self, position: u32) -> Result<InclusionProof> {
        let proof = CBMT::build_merkle_proof(&self.leaves, &[position])
            .context("Merkle position is out of bounds")?;
        Ok(InclusionProof {
            position,
            count: self.leaves.len() as u32,
            siblings: proof.lemmas().to_vec(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InclusionProof {
    pub position: u32,
    pub count: u32,
    pub siblings: Vec<Byte32>,
}

impl InclusionProof {
    pub fn verify_transaction(&self, network: &Byte32, root: &Byte32, tx: &Transaction) -> bool {
        self.verify(TX_ROOT, network, root, tx.commitment(network))
    }

    pub fn verify_live_cell(
        &self,
        network: &Byte32,
        root: &Byte32,
        point: &OutPoint,
        cell: &Cell,
    ) -> bool {
        self.verify(CELL_ROOT, network, root, cell.commitment(network, point))
    }

    fn verify(&self, domain: &[u8], network: &Byte32, root: &Byte32, leaf: Byte32) -> bool {
        if self.position >= self.count || self.count > u32::MAX / 2 || self.siblings.len() > 32 {
            return false;
        }
        let tree_index = self.count - 1 + self.position;
        MerkleProof::new(vec![tree_index], self.siblings.clone())
            .root(&[leaf])
            .is_some_and(|tree_root| bind_root(domain, network, self.count, &tree_root) == *root)
    }
}

fn bind_root(domain: &[u8], network: &Byte32, count: u32, root: &Byte32) -> Byte32 {
    hash(domain, network, &[&count.to_le_bytes(), root.as_slice()])
}
