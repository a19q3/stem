//! Patricia readers can return raw bytes or a node authenticated for this tree.

use anyhow::{Result, ensure};
use stem_stake::patricia::Node;

use super::{Key, Patricia};

mod sealed {
    use super::*;

    pub trait Sealed {
        fn resolve(&self, tree: &Patricia, id: &Key) -> Result<(Node, Option<&[u8]>)>;
    }

    impl<T: AsRef<[u8]>> Sealed for T {
        #[inline]
        fn resolve(&self, tree: &Patricia, id: &Key) -> Result<(Node, Option<&[u8]>)> {
            let bytes = self.as_ref();
            ensure!(tree.node_hash(bytes) == *id, "Patricia node hash mismatch");
            Ok((Node::decode(bytes)?, Some(bytes)))
        }
    }
}

/// A raw node value, or an authenticated handle from a verified node table.
/// Implementations are sealed so a reader cannot make unchecked nodes trusted.
pub trait NodeValue: sealed::Sealed {}
impl<T: sealed::Sealed> NodeValue for T {}

#[inline]
pub(super) fn checked_node<'a>(
    value: &'a impl NodeValue,
    tree: &Patricia,
    id: &Key,
) -> Result<(Node, Option<&'a [u8]>)> {
    value.resolve(tree, id)
}

#[cfg(feature = "verified-patricia-cache")]
pub use verified::{GeneratedNode, NodeRead, VerifiedChanges, VerifiedNodes};

#[cfg(feature = "verified-patricia-cache")]
mod verified {
    use std::{collections::BTreeMap, hash::RandomState};

    use hashbrown::{HashMap, TryReserveError};
    use stem_stake::patricia::MAX_NODE_BYTES;

    use super::*;

    enum Source<Raw> {
        Raw(Raw),
        Verified { domain: Key, id: Key, node: Node },
    }

    /// A node read that can fall back to canonical bytes from an unchecked source.
    pub struct NodeRead<Raw>(Source<Raw>);

    impl<Raw> NodeRead<Raw> {
        pub fn raw(bytes: Raw) -> Self {
            Self(Source::Raw(bytes))
        }
    }

    impl<Raw: AsRef<[u8]>> sealed::Sealed for NodeRead<Raw> {
        #[inline]
        fn resolve(&self, tree: &Patricia, expected: &Key) -> Result<(Node, Option<&[u8]>)> {
            match &self.0 {
                Source::Raw(raw) => raw.resolve(tree, expected),
                Source::Verified { domain, id, node } => {
                    ensure!(
                        *domain == tree.empty_root() && id == expected,
                        "verified Patricia node differs from its requested domain or ID"
                    );
                    Ok((node.clone(), None))
                }
            }
        }
    }

    /// An immutable node produced and hashed by a Patricia update.
    #[derive(Clone, Copy)]
    pub struct GeneratedNode<'a> {
        domain: &'a Key,
        id: &'a Key,
        node: &'a Node,
    }

    impl GeneratedNode<'_> {
        pub fn read<Raw>(self) -> NodeRead<Raw> {
            NodeRead(Source::Verified {
                domain: *self.domain,
                id: *self.id,
                node: self.node.clone(),
            })
        }

        pub fn encode_into(self, bytes: &mut [u8; MAX_NODE_BYTES]) -> &mut [u8] {
            self.node.encode_into(bytes)
        }
    }

    /// A sorted batch of generated nodes and tombstones, scoped to one tree.
    /// Only the Patricia updater can create a journal with authenticated rows.
    #[derive(Debug, PartialEq, Eq)]
    pub struct VerifiedChanges {
        domain: Key,
        rows: BTreeMap<Key, Option<Node>>,
    }

    impl VerifiedChanges {
        pub(in crate::patricia) fn from_update(
            tree: &Patricia,
            rows: BTreeMap<Key, Option<Node>>,
        ) -> Self {
            Self {
                domain: tree.empty_root(),
                rows,
            }
        }

        pub fn get<Raw>(&self, id: &Key) -> Option<Option<NodeRead<Raw>>> {
            let (id, node) = self.rows.get_key_value(id)?;
            Some(self.generated(id, node.as_ref()).map(GeneratedNode::read))
        }

        pub fn iter(&self) -> impl Iterator<Item = (&Key, Option<GeneratedNode<'_>>)> {
            self.rows
                .iter()
                .map(|(id, node)| (id, self.generated(id, node.as_ref())))
        }

        fn generated<'a>(
            &'a self,
            id: &'a Key,
            node: Option<&'a Node>,
        ) -> Option<GeneratedNode<'a>> {
            node.map(|node| GeneratedNode {
                domain: &self.domain,
                id,
                node,
            })
        }
    }

    /// Content IDs and the network/namespace domain are checked on insertion.
    /// The domain is stored once per table and immutable nodes can be reused
    /// from any authenticated root that refers to the same content ID.
    pub struct VerifiedNodes {
        tree: Patricia,
        rows: HashMap<Key, Node, RandomState>,
    }

    impl VerifiedNodes {
        pub fn new(tree: Patricia) -> Self {
            Self {
                tree,
                rows: HashMap::default(),
            }
        }

        pub fn get<T>(&self, id: &Key) -> Option<NodeRead<T>> {
            let node = self.rows.get(id)?.clone();
            Some(NodeRead(Source::Verified {
                domain: self.tree.empty_root(),
                id: *id,
                node,
            }))
        }

        pub fn insert(&mut self, id: Key, bytes: &[u8]) -> Result<()> {
            let (node, _) = checked_node(&bytes, &self.tree, &id)?;
            self.rows.insert(id, node);
            Ok(())
        }

        pub fn insert_generated(
            &mut self,
            expected: &Key,
            generated: GeneratedNode<'_>,
        ) -> Result<()> {
            ensure!(
                *generated.domain == self.tree.empty_root() && generated.id == expected,
                "generated Patricia node differs from its requested domain or ID"
            );
            self.rows.insert(*generated.id, generated.node.clone());
            Ok(())
        }

        pub fn remove(&mut self, id: &Key) {
            self.rows.remove(id);
        }

        pub fn contains_key(&self, id: &Key) -> bool {
            self.rows.contains_key(id)
        }

        pub fn len(&self) -> usize {
            self.rows.len()
        }

        pub fn is_empty(&self) -> bool {
            self.rows.is_empty()
        }

        pub fn capacity(&self) -> usize {
            self.rows.capacity()
        }

        pub fn try_reserve(&mut self, additional: usize) -> Result<(), TryReserveError> {
            self.rows.try_reserve(additional)
        }

        pub const fn entry_size() -> usize {
            std::mem::size_of::<(Key, Node)>()
        }

        pub fn allocation_size(&self) -> usize {
            self.rows.allocation_size()
        }

        pub fn release(&mut self) {
            self.rows = HashMap::default();
        }
    }
}

#[cfg(all(test, feature = "verified-patricia-cache"))]
mod tests {
    use ckb_types::{packed::Byte32, prelude::*};

    use super::*;
    use crate::patricia::Namespace;

    #[test]
    fn rejects_corrupt_rows_and_cross_domain_or_wrong_id_reads() -> Result<()> {
        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let mut cache = VerifiedNodes::new(tree.clone());
        let query = [7; 32];
        let value = [9; 32];
        let leaf = Node::Leaf { key: query, value }.encode();
        let id = tree.node_hash(&leaf);
        let wrong_id = [2; 32];
        assert!(cache.insert(wrong_id, &leaf).is_err());

        let malformed = [0; 64];
        let malformed_id = tree.node_hash(&malformed);
        assert!(cache.insert(malformed_id, &malformed).is_err());
        assert!(cache.is_empty());
        cache.insert(id, &leaf)?;
        let read = |_: &Key| Ok(cache.get::<Vec<u8>>(&id));

        assert!(tree.edit(wrong_id, read).prove(&query).is_err());
        for other in [
            Patricia::new(Byte32::default(), Namespace::System),
            Patricia::new([5; 32].pack(), Namespace::Cells),
        ] {
            assert!(other.edit(id, read).prove(&query).is_err());
        }
        let (found, proof) = tree.edit(id, read).prove(&query)?;
        assert_eq!(found, Some(value));
        assert_eq!(proof.nodes, [leaf]);
        assert!(tree.verify(&id, &query, Some(&value), &proof));
        Ok(())
    }
}
