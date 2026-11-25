//! Replay commitments for a unique CKB Intercell mailbox.

use alloc::vec::Vec;

use crate::{
    Error, Hash32, Result, digest,
    finality::{NATIVE_HEADER_BYTES, native_height, native_id},
    history::Frontier,
    sparse::{Domain, EMPTY, Tree},
};

fn domain(mailbox: Hash32) -> Domain {
    Domain {
        network: mailbox,
        node: b"Stem Intercell CKB inbox node\0",
    }
}

fn leaf(mailbox: &Hash32, id: &Hash32) -> Hash32 {
    digest(b"Stem Intercell CKB inbox leaf\0", &[mailbox, id])
}

pub fn consume(mailbox: &Hash32, id: &Hash32, root: &Hash32, path: &[u8]) -> Result<Hash32> {
    domain(*mailbox).update(id, root, EMPTY, leaf(mailbox, id), path)
}

/// Return the SystemRoot authorized by the checkpoint. An empty history proof
/// selects the checkpoint itself. A historical proof supplies the older Header,
/// the checkpoint's frontier and the minimal inclusion path inside its peak.
pub fn outbox_system_root(
    network: &Hash32,
    checkpoint: &[u8; NATIVE_HEADER_BYTES],
    history: &[u8],
) -> Result<Hash32> {
    let header = if history.is_empty() {
        checkpoint
    } else {
        let (header, proof) = history
            .split_first_chunk::<NATIVE_HEADER_BYTES>()
            .ok_or(Error::Encoding)?;
        let count = native_height(checkpoint);
        let (frontier, proof) = proof
            .split_at_checked(8 + 32 * count.count_ones() as usize)
            .ok_or(Error::Encoding)?;
        let frontier = Frontier::decode(*network, frontier)?;
        let (siblings, tail) = proof.as_chunks::<32>();
        if !tail.is_empty()
            || frontier.count() != count
            || frontier.root() != checkpoint[144..176]
            || !frontier.verify_inclusion(
                native_height(header),
                &native_id(network, header),
                siblings,
            )
        {
            return Err(Error::Anchor);
        }
        header
    };
    Ok(header[112..144].try_into().unwrap())
}

/// Off-chain index of already consumed IDs. A relayer checks its root against CKB.
pub struct Inbox {
    mailbox: Hash32,
    tree: Tree,
}

impl Inbox {
    pub fn new(mailbox: Hash32) -> Self {
        Self {
            mailbox,
            tree: Tree::new(domain(mailbox)),
        }
    }

    pub fn root(&self) -> Hash32 {
        self.tree.root()
    }

    pub fn insert(&mut self, id: Hash32) -> Result<()> {
        if self.tree.value(&id) != EMPTY {
            return Err(Error::Sequence);
        }
        self.tree.set(id, leaf(&self.mailbox, &id));
        Ok(())
    }

    pub fn path(&self, id: &Hash32) -> Vec<u8> {
        self.tree.proof(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_selects_only_the_checkpoint_or_a_proven_ancestor() -> Result<()> {
        let network = [1; 32];
        let mut genesis = [0; NATIVE_HEADER_BYTES];
        genesis[112..144].fill(2);
        let mut older = [0; NATIVE_HEADER_BYTES];
        older[..8].copy_from_slice(&1u64.to_le_bytes());
        older[112..144].fill(3);
        let mut checkpoint = [0; NATIVE_HEADER_BYTES];
        checkpoint[..8].copy_from_slice(&2u64.to_le_bytes());
        checkpoint[112..144].fill(4);

        let mut frontier = Frontier::empty(network);
        let genesis_id = native_id(&network, &genesis);
        frontier.append(0, &genesis_id)?;
        frontier.append(1, &native_id(&network, &older))?;
        checkpoint[144..176].copy_from_slice(&frontier.root());
        let path = crate::history::leaf(&network, 0, &genesis_id);
        let mut proof = [&older[..], &frontier.encode(), &path].concat();
        assert_eq!(outbox_system_root(&network, &checkpoint, &[])?, [4; 32]);
        assert_eq!(outbox_system_root(&network, &checkpoint, &proof)?, [3; 32]);
        assert!(outbox_system_root(&[9; 32], &checkpoint, &proof).is_err());
        assert!(outbox_system_root(&network, &older, &proof).is_err());
        assert!(outbox_system_root(&network, &checkpoint, &proof[..proof.len() - 1]).is_err());
        for position in [
            0,
            112,
            NATIVE_HEADER_BYTES,
            NATIVE_HEADER_BYTES + 8,
            proof.len() - 1,
        ] {
            proof[position] ^= 1;
            assert!(outbox_system_root(&network, &checkpoint, &proof).is_err());
            proof[position] ^= 1;
        }
        proof.extend_from_slice(&[0; 32]);
        assert!(outbox_system_root(&network, &checkpoint, &proof).is_err());
        Ok(())
    }

    #[test]
    fn replay_root_is_mailbox_specific_and_cannot_be_rolled_back() -> Result<()> {
        let mailbox = [1; 32];
        let first = [2; 32];
        let second = [3; 32];
        let mut index = Inbox::new(mailbox);
        let empty = index.root();
        let first_path = index.path(&first);
        let root = consume(&mailbox, &first, &empty, &first_path)?;
        index.insert(first)?;
        assert_eq!(root, index.root());
        assert!(consume(&mailbox, &first, &root, &index.path(&first)).is_err());
        let second_path = index.path(&second);
        assert!(consume(&[9; 32], &second, &root, &second_path).is_err());
        assert!(consume(&mailbox, &second, &root, &first_path).is_err());
        let root = consume(&mailbox, &second, &root, &second_path)?;
        index.insert(second)?;
        assert_eq!(root, index.root());
        Ok(())
    }
}
