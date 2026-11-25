//! Replay commitments for a unique CKB Intercell mailbox.

use alloc::vec::Vec;

use crate::{
    Error, Hash32, Result, digest,
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
