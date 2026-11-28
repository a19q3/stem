//! Incremental authenticated state. Node values contain only record hashes.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::Entity};

use crate::{
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

use stem_stake::patricia::{
    Commitment, MAX_NODE_BYTES, MAX_NODES, MAX_PATH_BYTES, Node, canonical_prefix, decode_path, side,
};
pub use stem_stake::patricia::{Key, Namespace};

#[cfg(feature = "batched-patricia-proofs")]
mod batched_proof;
#[cfg(feature = "patricia-frontier-edit")]
mod frontier_edit;
mod node_value;
use node_value::checked_node;
pub use node_value::NodeValue;
#[cfg(feature = "verified-patricia-cache")]
pub use node_value::{GeneratedNode, NodeRead, VerifiedChanges, VerifiedNodes};
#[cfg(feature = "compact-verified-journal")]
mod compact_journal;
#[cfg(feature = "compact-verified-journal")]
type GeneratedJournal = compact_journal::CompactJournal;
#[cfg(all(feature = "verified-patricia-cache", not(feature = "compact-verified-journal")))]
type GeneratedJournal = Vec<(Key, Option<Node>)>;

#[derive(Clone, Debug)]
pub struct Patricia {
    commitment: Commitment,
}

impl Patricia {
    pub fn new(network: Byte32, namespace: Namespace) -> Self {
        Self {
            commitment: Commitment::new(key(network.as_slice()), namespace),
        }
    }

    pub fn empty_root(&self) -> Key {
        self.commitment.empty_root()
    }

    fn node_hash(&self, encoded: &[u8]) -> Key {
        self.commitment.node_hash(encoded)
    }

    pub fn value_hash(&self, bytes: &[u8]) -> Key {
        self.commitment.value_hash(bytes)
    }

    pub(crate) fn value_hash_parts(&self, parts: &[&[u8]]) -> Key {
        self.commitment.value_hash_parts(parts)
    }

    pub fn record_key(&self, name: &[u8]) -> Key {
        self.commitment.record_key(name)
    }

    /// Rebuild the canonical root from sorted records in one pass. The frontier
    /// holds at most one unfinished branch per bit of the key.
    pub fn root_from_sorted_records(
        &self,
        records: impl IntoIterator<Item = Result<(Key, Key)>>,
    ) -> Result<Key> {
        let mut records = records.into_iter();
        let Some((mut previous, value)) = records.next().transpose()? else {
            return Ok(self.empty_root());
        };
        let leaf = |key, value| self.node_hash(&Node::Leaf { key, value }.encode());
        let branch = |prefix: &Key, bits, left, right| {
            self.node_hash(
                &Node::Branch {
                    prefix: canonical_prefix(prefix, bits),
                    bits,
                    children: [left, right],
                }
                .encode(),
            )
        };
        let mut right = leaf(previous, value);
        let mut frontier: Vec<(u8, Key)> = Vec::new();
        for record in records {
            let (key, value) = record?;
            ensure!(previous < key, "unordered or duplicate Patricia records");
            let split = difference(&previous, &key).expect("different record keys");
            while frontier.last().is_some_and(|(bits, _)| *bits > split) {
                let (bits, left) = frontier.pop().expect("unfinished Patricia branch");
                right = branch(&previous, bits, left, right);
            }
            frontier.push((split, right));
            previous = key;
            right = leaf(key, value);
        }
        while let Some((bits, left)) = frontier.pop() {
            right = branch(&previous, bits, left, right);
        }
        Ok(right)
    }

    pub fn edit<Read, Value>(&self, root: Key, read: Read) -> Edit<'_, Read>
    where
        Read: FnMut(&Key) -> Result<Option<Value>>,
        Value: NodeValue,
    {
        Edit {
            root,
            nodes: Nodes {
                tree: self,
                read,
                writes: BTreeMap::new(),
            },
        }
    }

    /// Update an immutable root from a single sorted change set. The traversal
    /// reads children of the original nodes and builds replacements bottom-up.
    /// Only subsequent edits need an index over those replacements.
    pub fn batch_update<Read, Value>(
        &self,
        root: Key,
        read: Read,
        changes: &[(Key, Option<Key>)],
        previous: impl FnMut(Key, Key, Option<Key>),
    ) -> Result<(Key, BTreeMap<Key, Option<Vec<u8>>>)>
    where
        Read: FnMut(&Key) -> Result<Option<Value>>,
        Value: NodeValue,
    {
        let (root, writes): (_, Vec<_>) = self.update_batch(root, read, changes, previous)?;
        Ok((root, writes.into_iter().collect()))
    }

    fn update_batch<Read, Value, Writes: BatchWrites>(
        &self,
        root: Key,
        read: Read,
        changes: &[(Key, Option<Key>)],
        previous: impl FnMut(Key, Key, Option<Key>),
    ) -> Result<(Key, Writes)>
    where
        Read: FnMut(&Key) -> Result<Option<Value>>,
        Value: NodeValue,
    {
        let mut nodes = Nodes {
            tree: self,
            read,
            writes: Writes::with_capacity(changes.len()),
        };
        let root = nodes.apply_batch(root, changes, previous)?;
        Ok((root, nodes.writes))
    }

    /// Update from a single immutable view. Parallel branches buffer their
    /// writes and replay previous values in serial traversal order.
    #[cfg(feature = "parallel-patricia")]
    pub fn batch_update_parallel<Read, Value>(
        &self,
        root: Key,
        read: Read,
        changes: &[(Key, Option<Key>)],
        previous: impl FnMut(Key, Key, Option<Key>),
    ) -> Result<(Key, BTreeMap<Key, Option<Vec<u8>>>)>
    where
        Read: Fn(&Key) -> Result<Option<Value>> + Sync,
        Value: NodeValue,
    {
        let (root, writes): (_, Vec<_>) = self.update_batch_parallel(root, read, changes, previous)?;
        Ok((root, writes.into_iter().collect()))
    }

    #[cfg(feature = "parallel-patricia")]
    fn update_batch_parallel<Read, Value, Writes: BatchWrites + Send>(
        &self,
        root: Key,
        read: Read,
        changes: &[(Key, Option<Key>)],
        mut previous: impl FnMut(Key, Key, Option<Key>),
    ) -> Result<(Key, Writes)>
    where
        Read: Fn(&Key) -> Result<Option<Value>> + Sync,
        Value: NodeValue,
    {
        let workers = rayon::current_num_threads();
        if workers < 2 || changes.len() < 2 * MIN_PARALLEL_CHANGES {
            return self.update_batch(root, read, changes, previous);
        }
        check_change_order(changes)?;
        let mut nodes = Nodes { tree: self, read: &read, writes: Writes::with_capacity(0) };
        let mut prior = Vec::new();
        let next = nodes.update_parallel(root, changes, 0, workers, &mut prior)?;
        for (key, value, replacement) in prior {
            previous(key, value, replacement);
        }
        Ok((next, nodes.writes))
    }

    /// Retain the immutable nodes that were hashed in this edit for later reads and publication.
    #[cfg(feature = "verified-patricia-cache")]
    pub fn batch_update_verified<Read, Value>(
        &self,
        root: Key,
        read: Read,
        changes: &[(Key, Option<Key>)],
        previous: impl FnMut(Key, Key, Option<Key>),
        parallel: bool,
    ) -> Result<(Key, VerifiedChanges)>
    where
        Read: Fn(&Key) -> Result<Option<Value>> + Sync,
        Value: NodeValue,
    {
        #[cfg(feature = "parallel-patricia")]
        let updated = if parallel {
            self.update_batch_parallel(root, read, changes, previous)
        } else {
            self.update_batch(root, read, changes, previous)
        };
        #[cfg(not(feature = "parallel-patricia"))]
        let updated = {
            let _ = parallel;
            self.update_batch(root, read, changes, previous)
        };
        let (next, rows): (_, GeneratedJournal) = updated?;
        Ok((next, VerifiedChanges::from_update(self, rows)))
    }

    pub fn verify(&self, root: &Key, query: &Key, expected: Option<&Key>, proof: &Proof) -> bool {
        self.commitment
            .verify(root, query, expected, proof.nodes.iter().map(Vec::as_slice))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// The path from the root to the matching leaf or the point of divergence.
    pub nodes: Vec<Vec<u8>>,
}

impl Proof {
    pub fn encode(&self) -> Result<Bytes> {
        ensure!(self.nodes.len() <= MAX_NODES, "oversized Patricia path");
        let nodes = self
            .nodes
            .iter()
            .map(|node| {
                Node::decode(node)?;
                Ok(wire::Bytes::from_raw_data(node)?)
            })
            .collect::<Result<_>>()?;
        Ok(wire::PatriciaPath::new_builder()
            .set(nodes)
            .build()
            .as_bytes())
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_PATH_BYTES, "oversized Patricia proof");
        Ok(Self {
            nodes: decode_path(bytes)?
                .into_iter()
                .map(<[u8]>::to_vec)
                .collect(),
        })
    }
}

pub struct Edit<'tree, Read> {
    root: Key,
    nodes: Nodes<'tree, Read, BTreeMap<Key, Option<Vec<u8>>>>,
}

struct Nodes<'tree, Read, Writes> {
    tree: &'tree Patricia,
    read: Read,
    writes: Writes,
}

trait NodeWrites {
    fn pending(&self, _: &Key) -> Option<&Option<Vec<u8>>> {
        None
    }
    fn record_insert(&mut self, id: Key, node: Node, bytes: &[u8]);
    fn record_delete(&mut self, id: Key);
}

trait BatchWrites: NodeWrites {
    fn with_capacity(rows: usize) -> Self;

    #[cfg(feature = "parallel-patricia")]
    fn reserve(&mut self, rows: usize);

    #[cfg(feature = "parallel-patricia")]
    fn append(&mut self, other: Self);
}

trait JournalRow {
    fn new(node: Node, bytes: &[u8]) -> Self;
}

impl JournalRow for Vec<u8> {
    fn new(_: Node, bytes: &[u8]) -> Self {
        bytes.to_vec()
    }
}

#[cfg(all(feature = "verified-patricia-cache", not(feature = "compact-verified-journal")))]
impl JournalRow for Node {
    fn new(node: Node, _: &[u8]) -> Self {
        node
    }
}

#[cfg(feature = "parallel-patricia")]
const MIN_PARALLEL_CHANGES: usize = 128;

#[cfg(feature = "parallel-patricia")]
type PreviousValue = (Key, Key, Option<Key>);

#[cfg(feature = "parallel-patricia")]
impl<Read, Value, Writes: BatchWrites + Send> Nodes<'_, &Read, Writes>
where
    Read: Fn(&Key) -> Result<Option<Value>> + Sync,
    Value: NodeValue,
{
    fn update_parallel(
        &mut self,
        root: Key,
        changes: &[(Key, Option<Key>)],
        depth: usize,
        workers: usize,
        previous: &mut Vec<PreviousValue>,
    ) -> Result<Key> {
        let old = self.load_optional(root)?;
        self.update_parallel_loaded(old, changes, depth, workers, previous)
    }

    fn update_parallel_loaded(
        &mut self,
        old: Option<(Key, Node)>,
        changes: &[(Key, Option<Key>)],
        depth: usize,
        workers: usize,
        previous: &mut Vec<PreviousValue>,
    ) -> Result<Key> {
        if workers >= 2
            && let Some((fork, edits)) = matching_fork(&old, changes, depth, MIN_PARALLEL_CHANGES)
        {
            return self.join_fork(fork, workers, previous, |child, side, budget, prior| {
                child.update_parallel(fork.children[side], edits[side], fork.depth(), budget, prior)
            });
        }
        self.writes.reserve(changes.len());
        self.update(old, changes, depth, &mut |key, value, next| {
            previous.push((key, value, next));
        })
    }

    fn join_fork(
        &mut self,
        fork: Fork,
        workers: usize,
        previous: &mut Vec<PreviousValue>,
        apply: impl Fn(&mut Self, usize, usize, &mut Vec<PreviousValue>) -> Result<Key> + Sync,
    ) -> Result<Key> {
        let (tree, read) = (self.tree, self.read);
        let apply_side = |side, budget, prior: &mut Vec<PreviousValue>| {
            let mut child = Nodes { tree, read, writes: Writes::with_capacity(0) };
            let root = apply(&mut child, side, budget, prior)?;
            Ok::<_, anyhow::Error>((root, child.writes))
        };
        let mut right_previous = Vec::new();
        let left_workers = workers / 2;
        let (left, right) = rayon::join(
            || apply_side(0, left_workers, previous),
            || apply_side(1, workers - left_workers, &mut right_previous),
        );
        let (left, left_writes) = left?;
        let (right, right_writes) = right?;
        previous.append(&mut right_previous);
        // Forks own disjoint journals. Join left before right for serial order.
        self.writes = left_writes;
        self.writes.append(right_writes);
        Ok(self.finish_branch(fork.id, &fork.prefix, fork.bits, fork.children, [left, right]))
    }
}

#[cfg(feature = "parallel-patricia")]
#[derive(Clone, Copy)]
struct Fork {
    id: Key,
    prefix: Key,
    bits: u8,
    children: [Key; 2],
}

#[cfg(feature = "parallel-patricia")]
impl Fork {
    fn depth(self) -> usize {
        usize::from(self.bits) + 1
    }
}

#[cfg(feature = "parallel-patricia")]
fn matching_fork<'a>(
    old: &Option<(Key, Node)>,
    changes: &'a [(Key, Option<Key>)],
    depth: usize,
    minimum: usize,
) -> Option<(Fork, [&'a [(Key, Option<Key>)]; 2])> {
    let Some((id, Node::Branch { prefix, bits, children })) = old else {
        return None;
    };
    if changes.len() < 2 * minimum
        || usize::from(*bits) < depth
        || canonical_prefix(&changes.first()?.0, *bits) != *prefix
        || canonical_prefix(&changes.last()?.0, *bits) != *prefix
    {
        return None;
    }
    let (left, right) = split_changes(changes, *bits);
    (left.len().min(right.len()) >= minimum).then_some((
        Fork { id: *id, prefix: *prefix, bits: *bits, children: *children },
        [left, right],
    ))
}

impl NodeWrites for BTreeMap<Key, Option<Vec<u8>>> {
    fn pending(&self, id: &Key) -> Option<&Option<Vec<u8>>> {
        self.get(id)
    }
    fn record_insert(&mut self, id: Key, _: Node, bytes: &[u8]) {
        self.insert(id, Some(bytes.to_vec()));
    }
    fn record_delete(&mut self, id: Key) {
        self.insert(id, None);
    }
}

impl<Row: JournalRow> NodeWrites for Vec<(Key, Option<Row>)> {
    fn record_insert(&mut self, id: Key, node: Node, bytes: &[u8]) {
        self.push((id, Some(Row::new(node, bytes))));
    }
    fn record_delete(&mut self, id: Key) {
        self.push((id, None));
    }
}

impl<Row: JournalRow> BatchWrites for Vec<(Key, Option<Row>)> {
    fn with_capacity(rows: usize) -> Self {
        Vec::with_capacity(rows)
    }

    #[cfg(feature = "parallel-patricia")]
    fn reserve(&mut self, rows: usize) {
        self.reserve(rows);
    }

    #[cfg(feature = "parallel-patricia")]
    fn append(&mut self, other: Self) {
        self.extend(other);
    }
}

impl<Read, Value> Edit<'_, Read>
where
    Read: FnMut(&Key) -> Result<Option<Value>>,
    Value: NodeValue,
{
    pub fn root(&self) -> Key {
        self.root
    }

    pub fn set(&mut self, key: Key, value: Option<Key>) -> Result<Option<Key>> {
        let mut previous = None;
        let old = self.nodes.load_optional(self.root)?;
        let root = self
            .nodes
            .update(old, &[(key, value)], 0, &mut |_, value, _| {
                previous = Some(value)
            })?;
        self.root = root;
        Ok(previous)
    }

    /// Apply unique changes in key order and report the touched leaves that existed.
    pub fn set_batch(
        &mut self,
        changes: &[(Key, Option<Key>)],
        previous: impl FnMut(Key, Key, Option<Key>),
    ) -> Result<()> {
        self.root = self.nodes.apply_batch(self.root, changes, previous)?;
        Ok(())
    }

    pub fn prove(&mut self, key: &Key) -> Result<(Option<Key>, Proof)> {
        let mut nodes = Vec::new();
        let mut id = self.root;
        let mut depth = 0;
        let mut value = None;
        while id != self.nodes.tree.empty_root() {
            let node = self.nodes.load(&id, |node, bytes| {
                nodes.push(bytes.map_or_else(|| node.encode(), <[u8]>::to_vec));
                node
            })?;
            match node {
                Node::Leaf {
                    key: found,
                    value: hash,
                } => {
                    value = (found == *key).then_some(hash);
                    break;
                }
                Node::Branch {
                    prefix,
                    bits,
                    children,
                } => {
                    ensure!(usize::from(bits) >= depth, "invalid Patricia path depth");
                    if canonical_prefix(key, bits) != prefix {
                        break;
                    }
                    id = children[side(key, bits)];
                    depth = usize::from(bits) + 1;
                }
            }
        }
        Ok((value, Proof { nodes }))
    }

    /// Read each shared prefix once and return ordinary proofs in caller order.
    /// Repeated queries produce repeated proofs against the same root.
    pub fn prove_many(&mut self, keys: &[Key]) -> Result<Vec<(Option<Key>, Proof)>> {
        let mut queries: Vec<_> = keys.iter().copied().zip(0..keys.len()).collect();
        queries.sort_unstable_by_key(|&(key, _)| key);
        let mut proofs = vec![(None, Proof { nodes: Vec::new() }); keys.len()];
        self.nodes.prove_sorted(self.root, &queries, 0, &mut proofs)?;
        Ok(proofs)
    }

    pub fn into_writes(self) -> BTreeMap<Key, Option<Vec<u8>>> {
        self.nodes.writes
    }
}

impl<Read, Value, Writes> Nodes<'_, Read, Writes>
where
    Read: FnMut(&Key) -> Result<Option<Value>>,
    Value: NodeValue,
    Writes: NodeWrites,
{
    fn prove_sorted(
        &mut self,
        id: Key,
        queries: &[(Key, usize)],
        depth: usize,
        proofs: &mut [(Option<Key>, Proof)],
    ) -> Result<()> {
        if queries.is_empty() || id == self.tree.empty_root() {
            return Ok(());
        }
        let node = self.load(&id, |node, bytes| {
            append_proof_node(&node, bytes, queries, proofs);
            node
        })?;
        match node {
            Node::Leaf { key, value } => {
                for &(candidate, index) in queries {
                    if candidate == key {
                        proofs[index].0 = Some(value);
                    }
                }
            }
            Node::Branch {
                prefix,
                bits,
                children,
            } => {
                ensure!(usize::from(bits) >= depth, "invalid Patricia path depth");
                let (left, right) = split_proof_queries(queries, prefix, bits);
                let child_depth = usize::from(bits) + 1;
                self.prove_sorted(children[0], left, child_depth, proofs)?;
                self.prove_sorted(children[1], right, child_depth, proofs)?;
            }
        }
        Ok(())
    }

    fn apply_batch(
        &mut self,
        root: Key,
        changes: &[(Key, Option<Key>)],
        mut previous: impl FnMut(Key, Key, Option<Key>),
    ) -> Result<Key> {
        if changes.is_empty() {
            return Ok(root);
        }
        check_change_order(changes)?;
        let old = self.load_optional(root)?;
        self.update(old, changes, 0, &mut previous)
    }

    fn load<T>(&mut self, id: &Key, use_node: impl FnOnce(Node, Option<&[u8]>) -> T) -> Result<T> {
        let loaded;
        let (node, bytes) = match self.writes.pending(id) {
            Some(entry) => checked_node(
                entry.as_ref().context("missing Patricia node")?,
                self.tree,
                id,
            )?,
            None => {
                loaded = (self.read)(id)?;
                checked_node(
                    loaded.as_ref().context("missing Patricia node")?,
                    self.tree,
                    id,
                )?
            }
        };
        Ok(use_node(node, bytes))
    }

    fn insert(&mut self, node: Node) -> Key {
        let mut buffer = [0; MAX_NODE_BYTES];
        let bytes = node.encode_into(&mut buffer);
        let id = self.tree.node_hash(bytes);
        self.writes.record_insert(id, node, bytes);
        id
    }

    fn load_optional(&mut self, id: Key) -> Result<Option<(Key, Node)>> {
        if id == self.tree.empty_root() {
            Ok(None)
        } else {
            Ok(Some((id, self.load(&id, |node, _| node)?)))
        }
    }

    fn branch(&mut self, key: &Key, bits: u8, children: [Key; 2]) -> Key {
        let empty = self.tree.empty_root();
        if children[0] == empty {
            children[1]
        } else if children[1] == empty {
            children[0]
        } else {
            self.insert(Node::Branch {
                prefix: canonical_prefix(key, bits),
                bits,
                children,
            })
        }
    }

    fn finish_branch(&mut self, id: Key, prefix: &Key, bits: u8, children: [Key; 2], updated: [Key; 2]) -> Key {
        if updated == children {
            return id;
        }
        self.writes.record_delete(id);
        self.branch(prefix, bits, updated)
    }

    fn update(
        &mut self,
        old: Option<(Key, Node)>,
        changes: &[(Key, Option<Key>)],
        depth: usize,
        previous: &mut impl FnMut(Key, Key, Option<Key>),
    ) -> Result<Key> {
        let empty = self.tree.empty_root();
        let Some((first, _)) = changes.first() else {
            return Ok(old.map_or(empty, |(id, _)| id));
        };
        let last = &changes.last().expect("nonempty changes").0;
        let Some((id, node)) = old else {
            if let Some(bit) = difference(first, last) {
                ensure!(usize::from(bit) >= depth, "invalid Patricia update depth");
                let (left, right) = split_changes(changes, bit);
                let children = [
                    self.update(None, left, usize::from(bit) + 1, previous)?,
                    self.update(None, right, usize::from(bit) + 1, previous)?,
                ];
                return Ok(self.branch(first, bit, children));
            }
            return Ok(changes[0].1.map_or(empty, |value| {
                self.insert(Node::Leaf { key: *first, value })
            }));
        };
        let (prefix, bits) = node.prefix();
        ensure!(bits >= depth, "invalid Patricia path depth");
        let split = difference(prefix, first)
            .into_iter()
            .chain(difference(prefix, last))
            .min();
        if let Some(bit) = split.filter(|&bit| usize::from(bit) < bits) {
            ensure!(
                usize::from(bit) >= depth,
                "Patricia child escapes its parent"
            );
            let mut children = [empty; 2];
            let old_side = side(prefix, bit);
            let (left, right) = split_changes(changes, bit);
            let halves = [left, right];
            let depth = usize::from(bit) + 1;
            children[1 - old_side] = self.update(None, halves[1 - old_side], depth, previous)?;
            children[old_side] =
                self.update(Some((id, node)), halves[old_side], depth, previous)?;
            return Ok(self.branch(first, bit, children));
        }
        Ok(match node {
            Node::Leaf { key, value } => {
                previous(key, value, changes[0].1);
                if Some(value) == changes[0].1 {
                    return Ok(id);
                }
                self.writes.record_delete(id);
                changes[0]
                    .1
                    .map_or(empty, |value| self.insert(Node::Leaf { key, value }))
            }
            Node::Branch {
                prefix,
                bits,
                children,
            } => {
                let (left, right) = split_changes(changes, bits);
                let mut updated = children;
                for (index, edits) in [left, right].into_iter().enumerate() {
                    if !edits.is_empty() {
                        let child = self.load_optional(children[index])?;
                        updated[index] =
                            self.update(child, edits, usize::from(bits) + 1, previous)?;
                    }
                }
                self.finish_branch(id, &prefix, bits, children, updated)
            }
        })
    }
}

fn append_proof_node(
    node: &Node,
    bytes: Option<&[u8]>,
    queries: &[(Key, usize)],
    proofs: &mut [(Option<Key>, Proof)],
) {
    let mut append = |bytes: &[u8]| {
        for &(_, index) in queries {
            proofs[index].1.nodes.push(bytes.to_vec());
        }
    };
    if let Some(bytes) = bytes {
        append(bytes);
    } else {
        let mut buffer = [0; MAX_NODE_BYTES];
        append(node.encode_into(&mut buffer));
    }
}

fn split_proof_queries(
    queries: &[(Key, usize)],
    prefix: Key,
    bits: u8,
) -> (&[(Key, usize)], &[(Key, usize)]) {
    let prefix_for = |&(key, _): &(Key, usize)| canonical_prefix(&key, bits);
    let start = queries.partition_point(|query| prefix_for(query) < prefix);
    let end = queries.partition_point(|query| prefix_for(query) <= prefix);
    let matches = &queries[start..end];
    let pivot = matches.partition_point(|(key, _)| side(key, bits) == 0);
    matches.split_at(pivot)
}

fn check_change_order(changes: &[(Key, Option<Key>)]) -> Result<()> {
    ensure!(changes.windows(2).all(|pair| pair[0].0 < pair[1].0), "unordered or duplicate Patricia changes");
    Ok(())
}

fn split_changes(changes: &[(Key, Option<Key>)], bit: u8) -> (&[(Key, Option<Key>)], &[(Key, Option<Key>)]) {
    changes.split_at(changes.partition_point(|(key, _)| side(key, bit) == 0))
}

fn key(bytes: &[u8]) -> Key {
    bytes.try_into().expect("32 byte hash")
}

fn difference(left: &Key, right: &Key) -> Option<u8> {
    left.iter()
        .zip(right)
        .enumerate()
        .find_map(|(index, (&left, &right))| {
            (left != right).then(|| (index * 8 + (left ^ right).leading_zeros() as usize) as u8)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_root_matches_the_canonical_batch_builder() -> Result<()> {
        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let mut leaves: Vec<_> = (0..2048u16)
            .map(|value| (tree.record_key(&value.to_le_bytes()), tree.value_hash(&value.to_be_bytes())))
            .collect();
        leaves.sort_unstable_by_key(|(key, _)| *key);
        for count in [0, 1, 2, 3, 31, 256, 513, 2048] {
            let records = &leaves[..count];
            let changes: Vec<_> = records.iter().map(|&(key, value)| (key, Some(value))).collect();
            let (root, _) = tree.batch_update(
                tree.empty_root(), |_| Ok(None::<Vec<u8>>), &changes, |_, _, _| {},
            )?;
            assert_eq!(tree.root_from_sorted_records(records.iter().copied().map(Ok))?, root);
        }
        assert!(tree.root_from_sorted_records([Ok(leaves[1]), Ok(leaves[0])]).is_err());
        assert!(tree.root_from_sorted_records([Ok(leaves[1]), Ok(leaves[1])]).is_err());
        Ok(())
    }

    #[test]
    fn batch_rewrites_shared_paths_once_and_preserves_proofs() -> Result<()> {
        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let keys: Vec<_> = (0..1024u16)
            .map(|i| tree.record_key(&i.to_le_bytes()))
            .collect();
        let old = tree.value_hash(b"old");
        let new = tree.value_hash(b"new");
        let mut genesis = tree.edit(tree.empty_root(), |_| Ok(None::<Vec<u8>>));
        for &key in &keys[..700] {
            genesis.set(key, Some(old))?;
        }
        let root = genesis.root();
        let mut storage: BTreeMap<_, _> = genesis
            .into_writes()
            .into_iter()
            .filter_map(|(id, value)| value.map(|bytes| (id, bytes)))
            .collect();
        let mut changes: Vec<_> = keys
            .iter()
            .enumerate()
            .filter_map(|(index, &key)| match index % 5 {
                0 => Some((key, None)),
                1 | 2 => Some((key, Some(new))),
                3 => Some((key, Some(old))),
                _ => None,
            })
            .collect();
        changes.sort_unstable_by_key(|(key, _)| *key);
        let (batch_result, batch_writes) = {
            let mut reads = 0;
            let mut batch = tree.edit(root, |id| {
                reads += 1;
                Ok(storage.get(id).cloned())
            });
            let mut seen = BTreeMap::new();
            batch.set_batch(&changes, |key, before, after| {
                assert!(seen.insert(key, before).is_none());
                assert_eq!(
                    after,
                    changes[changes.binary_search_by_key(&key, |(key, _)| *key).unwrap()].1
                );
            })?;
            let mut sequential_reads = 0;
            let mut sequential = tree.edit(root, |id| {
                sequential_reads += 1;
                Ok(storage.get(id).cloned())
            });
            for &(key, value) in &changes {
                assert_eq!(sequential.set(key, value)?, seen.get(&key).copied());
            }
            let result = batch.root();
            assert_eq!(result, sequential.root());
            let writes = batch.into_writes();
            assert!(writes.len() < sequential.into_writes().len());
            assert!(reads < sequential_reads);
            (result, writes)
        };
        for (id, value) in batch_writes {
            if let Some(bytes) = value {
                storage.insert(id, bytes);
            } else {
                storage.remove(&id);
            }
        }
        let mut verify = tree.edit(batch_result, |id| Ok(storage.get(id).cloned()));
        for (index, key) in keys.iter().enumerate() {
            let expected = match index % 5 {
                0 => None,
                1 | 2 => Some(new),
                3 => Some(old),
                _ => (index < 700).then_some(old),
            };
            let (value, proof) = verify.prove(key)?;
            assert_eq!(value, expected);
            assert!(tree.verify(&batch_result, key, expected.as_ref(), &proof));
        }
        let leaves = (0..keys.len())
            .filter(|index| index % 5 != 0 && (index % 5 != 4 || *index < 700))
            .count();
        assert_eq!(storage.len(), 2 * leaves - 1);
        assert!(
            verify
                .set_batch(&[changes[0], changes[0]], |_, _, _| {})
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn immutable_batch_preserves_indexed_journal_and_previous_values() -> Result<()> {
        for namespace in [Namespace::Cells, Namespace::System] {
            let tree = Patricia::new(Byte32::default(), namespace);
            let mut root = tree.empty_root();
            let mut stored: BTreeMap<Key, Vec<u8>> = BTreeMap::new();
            let read_missing = |_: &Key| Ok(None::<Vec<u8>>);
            for generation in 0..9 {
                // Combine long shared prefixes, dispersed keys, and inserts, deletes and no-ops.
                let mut changes = BTreeMap::new();
                for i in 0..192u8 {
                    let mut key = [0x7f; 32];
                    key[31] = i;
                    let value = match (i as usize + generation) % 4 {
                        0 => None,
                        1 => Some([i; 32]),
                        _ => Some([generation as u8; 32]),
                    };
                    changes.insert(key, value);
                    changes.insert(tree.record_key(&[i]), value);
                }
                let changes = changes.into_iter().collect::<Vec<_>>();
                let read = |id: &Key| Ok(stored.get(id).cloned());
                let mut indexed = tree.edit(root, read);
                let mut seen = Vec::new();
                indexed.set_batch(&changes, |k, before, after| seen.push((k, before, after)))?;
                let mut actual = Vec::new();
                let (next, writes) =
                    tree.batch_update(root, read, &changes, |k, before, after| {
                        actual.push((k, before, after));
                    })?;
                assert_eq!(next, indexed.root());
                assert_eq!(writes, indexed.into_writes());
                assert_eq!(actual, seen);
                assert_eq!(
                    tree.batch_update(next, read_missing, &[], |_, _, _| {})?,
                    (next, BTreeMap::new())
                );
                assert!(
                    tree.batch_update(next, read_missing, &changes, |_, _, _| {})
                        .is_err()
                );
                assert!(
                    tree.batch_update(root, read, &[changes[0], changes[0]], |_, _, _| {})
                        .is_err()
                );
                root = next;
                for (id, value) in writes {
                    if let Some(bytes) = value {
                        stored.insert(id, bytes);
                    } else {
                        stored.remove(&id);
                    }
                }
            }
        }
        Ok(())
    }

    #[cfg(feature = "parallel-patricia")]
    #[test]
    fn parallel_mutations_preserve_reclamation_and_proofs() -> Result<()> {
        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
        let keys: Vec<Key> = (0..4)
            .flat_map(|prefix| {
                (0..512u16).map(move |suffix| {
                    let mut key = [0; 32];
                    key[0] = prefix << 6;
                    key[30..].copy_from_slice(&suffix.to_be_bytes());
                    key
                })
            })
            .collect();
        let mut root = tree.empty_root();
        let mut storage = BTreeMap::<Key, Vec<u8>>::new();
        let mut values = BTreeMap::<Key, Key>::new();
        for generation in 0..6 {
            let value = tree.value_hash(&[generation]);
            let changes = keys
                .iter()
                .enumerate()
                .filter_map(|(i, key)| {
                    let prefix = key[0] >> 6;
                    let suffix = i % 512;
                    let next = match generation {
                        0 if prefix < 2 && suffix < 320 => Some(value),
                        0 => return None,
                        1 if suffix < 256 => Some(value),
                        1 => None,
                        2 if prefix >= 2 => None,
                        2 if suffix % 2 == 0 => values.get(key).copied(),
                        2 => Some(value),
                        3 if prefix == 0 && suffix < 300 => Some(value),
                        3 => return None,
                        4 => None,
                        _ if suffix % 3 == 0 => Some(value),
                        _ => None,
                    };
                    Some((*key, next))
                })
                .collect::<Vec<_>>();
            let read = |id: &Key| Ok(storage.get(id).map(Vec::as_slice));
            let mut serial_prior = Vec::new();
            let (serial, writes) = tree.batch_update(root, read, &changes, |key, before, after| {
                serial_prior.push((key, before, after));
            })?;
            let mut parallel_prior = Vec::new();
            let (parallel, parallel_writes) = pool.install(|| {
                tree.batch_update_parallel(root, read, &changes, |key, before, after| {
                    parallel_prior.push((key, before, after));
                })
            })?;
            assert_eq!(parallel, serial);
            assert_eq!(parallel_writes, writes);
            assert_eq!(parallel_prior, serial_prior);

            #[cfg(feature = "verified-patricia-cache")]
            {
                let mut cache = VerifiedNodes::new(tree.clone());
                for (id, bytes) in storage.iter().step_by(2) {
                    cache.insert(*id, bytes)?;
                }
                let mixed = |id: &Key| cache.get(id)
                    .or_else(|| storage.get(id).map(|bytes| NodeRead::raw(bytes.as_slice())));
                let mut verified_prior = Vec::new();
                let (verified, journal) = pool.install(|| {
                    tree.batch_update_verified(
                        root, |id| Ok(mixed(id)), &changes, |key, before, after| {
                            verified_prior.push((key, before, after));
                        }, true,
                    )
                })?;
                assert_eq!(verified, serial);
                let mut bytes = [0; MAX_NODE_BYTES];
                let encoded: Vec<_> = journal.iter().map(|(id, node)| {
                    (*id, node.map(|node| node.encode_into(&mut bytes).to_vec()))
                }).collect();
                assert_eq!(encoded, writes.iter().map(|(id, value)| (*id, value.clone())).collect::<Vec<_>>());
                assert_eq!(verified_prior, serial_prior);
                let mut mixed_view = tree.edit(verified, |id| {
                    Ok(journal.get(id).unwrap_or_else(|| mixed(id)))
                });
                let mut raw_view = tree.edit(serial, |id| {
                    Ok(writes.get(id).map_or_else(|| storage.get(id).map(Vec::as_slice), |write| write.as_deref()))
                });
                let sampled: Vec<_> = keys.iter().step_by(43).copied().collect();
                assert_eq!(mixed_view.prove_many(&sampled)?, raw_view.prove_many(&sampled)?);
            }

            // Read both resulting views from the pinned parent and independent journals.
            let mut expected =
                tree.edit(serial, |id| Ok(writes.get(id).map_or_else(|| storage.get(id).map(Vec::as_slice), |data| data.as_deref())));
            let mut actual = tree.edit(parallel, |id| {
                Ok(parallel_writes.get(id).map_or_else(|| storage.get(id).map(Vec::as_slice), |data| data.as_deref()))
            });
            for (key, value) in changes {
                match value {
                    Some(value) => {
                        values.insert(key, value);
                    }
                    None => {
                        values.remove(&key);
                    }
                }
            }
            for key in keys.iter().step_by(11) {
                let (expected_value, expected_proof) = expected.prove(key)?;
                let (actual_value, actual_proof) = actual.prove(key)?;
                assert_eq!(actual_value, values.get(key).copied());
                assert_eq!(expected_value, actual_value);
                assert_eq!(expected_proof.encode()?, actual_proof.encode()?);
                assert!(tree.verify(&parallel, key, actual_value.as_ref(), &actual_proof));
            }
            root = parallel;
            for (id, value) in parallel_writes {
                match value {
                    Some(data) => {
                        storage.insert(id, data);
                    }
                    None => {
                        storage.remove(&id);
                    }
                }
            }
            assert_eq!(storage.len(), (2 * values.len()).saturating_sub(1));
        }
        let duplicated = vec![(keys[0], None); 2 * MIN_PARALLEL_CHANGES];
        assert!(
            pool.install(|| tree.batch_update_parallel(root, |id| Ok(storage.get(id).map(Vec::as_slice)), &duplicated, |_, _, _| {},))
                .is_err()
        );
        let updates = keys.iter().copied().map(|key| (key, None)).collect::<Vec<_>>();
        let mut events = Vec::new();
        let missing = pool.install(|| {
            tree.batch_update_parallel(
                root,
                |id| Ok((id == &root).then(|| storage[id].as_slice())),
                &updates,
                |key, _, _| events.push(key),
            )
        });
        assert!(missing.is_err());
        assert!(events.is_empty());
        Ok(())
    }

    #[cfg(feature = "parallel-patricia")]
    #[test]
    #[ignore = "large immutable Patricia mutation timing matrix"]
    fn measure_immutable_mutations_by_size_and_workers() -> Result<()> {
        use std::{
            sync::atomic::{AtomicUsize, Ordering},
            time::Instant,
        };

        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let key = |i: u32| tree.record_key(&i.to_le_bytes());
        let value = Some(tree.value_hash(b"payment cell"));
        let pools = [1, 2, 4, 8, 16, 32]
            .map(|workers| rayon::ThreadPoolBuilder::new().num_threads(workers).build())
            .into_iter().collect::<Result<Vec<_>, _>>()?;
        println!("live_keys,changes,workers,median_us,logical_read_nodes,logical_read_bytes");
        for live in [1_024u32, 65_536, 1_000_000] {
            let mut initial = (0..live).map(|i| (key(i), value)).collect::<Vec<_>>();
            initial.sort_unstable_by_key(|entry| entry.0);
            let (root, nodes) = tree.batch_update(
                tree.empty_root(), |_| Ok(None::<Vec<u8>>), &initial, |_, _, _| {},
            )?;
            drop(initial);
            let read = |id: &Key| Ok(nodes.get(id).and_then(|data| data.as_deref()));
            for changes in [64u32, 512, 2_048] {
                let half = changes / 2;
                let mut edits = (0..half).map(|i| (key(i * (live / half)), None))
                    .chain((live..live + half).map(|i| (key(i), value)))
                    .collect::<Vec<_>>();
                edits.sort_unstable_by_key(|entry| entry.0);
                let mut expected_reads = 0;
                let mut expected_bytes = 0;
                let expected = tree.batch_update(root, |id| {
                    let data = read(id)?;
                    expected_reads += 1;
                    expected_bytes += data.map_or(0, <[u8]>::len);
                    Ok(data)
                }, &edits, |_, _, _| {})?;
                let mut samples = vec![Vec::new(); pools.len()];
                for round in 0..7 {
                    for offset in 0..pools.len() {
                        let index = (round + offset) % pools.len();
                        let pool = &pools[index];
                        let (elapsed, actual) = pool.install(|| {
                            let started = Instant::now();
                            let actual = tree.batch_update_parallel(root, read, &edits, |_, _, _| {});
                            (started.elapsed().as_micros(), actual)
                        });
                        assert_eq!(actual?, expected);
                        if round > 0 {
                            samples[index].push(elapsed);
                        }
                    }
                }
                for (pool, mut times) in pools.iter().zip(samples) {
                    times.sort_unstable();
                    let reads = AtomicUsize::new(0);
                    let bytes = AtomicUsize::new(0);
                    let counted = pool.install(|| tree.batch_update_parallel(root, |id| {
                        let data = read(id)?;
                        reads.fetch_add(1, Ordering::Relaxed);
                        bytes.fetch_add(data.map_or(0, <[u8]>::len), Ordering::Relaxed);
                        Ok(data)
                    }, &edits, |_, _, _| {}))?;
                    assert_eq!(counted, expected);
                    assert_eq!(reads.into_inner(), expected_reads);
                    assert_eq!(bytes.into_inner(), expected_bytes);
                    let median = (times[2] + times[3]) / 2;
                    println!(
                        "{live},{changes},{},{median},{expected_reads},{expected_bytes}",
                        pool.current_num_threads(),
                    );
                }
            }
        }
        Ok(())
    }

    #[test]
    fn incremental_roots_proofs_and_reclamation() -> Result<()> {
        let tree = Patricia::new(Byte32::default(), Namespace::Cells);
        let other = Patricia::new(Byte32::default(), Namespace::System);
        let mut storage = BTreeMap::new();
        let keys: Vec<_> = (0..512u16)
            .map(|i| tree.record_key(&i.to_le_bytes()))
            .collect();
        let value = tree.value_hash(b"record");
        let mut first = tree.edit(tree.empty_root(), |_| Ok(None::<Vec<u8>>));
        let mut second = tree.edit(tree.empty_root(), |_| Ok(None::<Vec<u8>>));
        for (&a, &b) in keys.iter().zip(keys.iter().rev()) {
            assert!(first.set(a, Some(value))?.is_none());
            assert!(second.set(b, Some(value))?.is_none());
        }
        let root = first.root();
        assert_eq!(root, second.root());
        for (id, data) in first.into_writes() {
            storage.extend(data.map(|bytes| (id, bytes)));
        }
        assert_eq!(storage.len(), 2 * keys.len() - 1);
        let absent = tree.record_key(b"absent");
        let mut reads = 0;
        let mut third = tree.edit(root, |id| {
            reads += 1;
            Ok(storage.get(id).map(Vec::as_slice))
        });
        let (actual, included) = third.prove(&keys[71])?;
        assert_eq!(actual, Some(value));
        assert!(tree.verify(&root, &keys[71], Some(&value), &included));
        assert!(!tree.verify(&root, &keys[71], None, &included));
        assert!(!other.verify(&root, &keys[71], Some(&value), &included));
        let (actual, excluded) = third.prove(&absent)?;
        assert!(actual.is_none() && tree.verify(&root, &absent, None, &excluded));
        assert_eq!(third.set(keys[71], None)?, Some(value));
        let after = third.root();
        let (_, new_proof) = third.prove(&keys[71])?;
        assert!(tree.verify(&after, &keys[71], None, &new_proof));
        let writes = third.into_writes();
        assert!(reads < 64);
        assert!(writes.len() < 64);
        for (id, data) in writes {
            match data {
                Some(bytes) => {
                    storage.insert(id, bytes);
                }
                None => {
                    storage.remove(&id);
                }
            }
        }
        assert_eq!(storage.len(), 2 * (keys.len() - 1) - 1);
        let mut final_edit = tree.edit(after, |id| Ok(storage.get(id).cloned()));
        for &id in &keys {
            final_edit.set(id, None)?;
        }
        assert_eq!(final_edit.root(), tree.empty_root());
        assert!(final_edit.into_writes().values().all(Option::is_none));
        Ok(())
    }
}
