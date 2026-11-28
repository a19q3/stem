//! Development-only comparison using versioned JMT nodes and the native write path.

use std::{collections::BinaryHeap, path::Path, time::Instant};

use anyhow::{Context, Result, bail, ensure};
use ckb_db::internal::{Options, ReadOnlyDB, ReadOptions, prelude::*};
use ckb_types::packed::Byte32;
use ckb_types::prelude::*;
use jmt::{
    KeyHash, RootHash, Sha256Jmt, Version,
    storage::{LeafNode, Node, NodeKey, TreeReader, TreeUpdateBatch},
};
use sha2::{Digest, Sha256};
use stem_ledger::{
    block::Header,
    patricia::{Key, Namespace, Patricia},
    record,
};

use super::{
    Batch, JMT_NODE, MARKER, MAX_CELL_PROOF_BATCH, STATE_RECORD, STORAGE_LAYOUT, Store, TIP,
    state_key,
};

mod proof_audit;

pub(super) const LAYOUT: &[u8] = b"Stem research JMT 0.12 only\0";

fn key(node: &NodeKey) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(51);
    out.push(JMT_NODE);
    out.extend(node.version().to_be_bytes());
    borsh::to_writer(&mut out, node.nibble_path())?;
    Ok(out)
}

enum Reader<'a> {
    Empty,
    Branch(&'a Store),
}

impl TreeReader for Reader<'_> {
    fn get_node_option(&self, node: &NodeKey) -> Result<Option<Node>> {
        let slot = key(node)?;
        let decode = |bytes: &[u8]| Ok(borsh::from_slice(bytes)?);
        match self {
            Self::Empty => Ok(None),
            Self::Branch(store) => store.read(slot)?.as_deref().map(decode).transpose(),
        }
    }

    fn get_value_option(&self, _: Version, _: KeyHash) -> Result<Option<Vec<u8>>> {
        bail!("JMT mutation reader does not supply proof values")
    }

    fn get_rightmost_leaf(&self) -> Result<Option<(NodeKey, LeafNode)>> {
        bail!("state restoration is outside the local JMT trial")
    }
}

pub(super) fn bootstrap(allocations: Vec<(Key, Key)>) -> Result<(RootHash, TreeUpdateBatch)> {
    let tree = Sha256Jmt::new(&Reader::Empty);
    if allocations.is_empty() {
        return tree.put_value_set([], 0);
    }
    let initial = allocations
        .into_iter()
        .map(|(id, digest)| (KeyHash(id), digest.to_vec()))
        .collect();
    let (roots, batch) = tree.batch_put_value_sets(vec![initial], None, 0)?;
    let [root] = roots.as_slice() else {
        bail!("expected one JMT genesis root");
    };
    Ok((*root, batch))
}

pub(super) fn check_view_root(store: &Store) -> Result<()> {
    let reader = Reader::Branch(store);
    let version = store.tip.context.height;
    ensure!(
        Sha256Jmt::new(&reader).get_root_hash(version)?.0.as_slice()
            == store.tip.state.cell_root.as_slice(),
        "JMT nodes differ from the state view root at height {version}"
    );
    Ok(())
}

pub(super) fn update(
    store: &Store,
    changes: &[(Key, Option<Key>)],
    version: u64,
) -> Result<(RootHash, TreeUpdateBatch)> {
    check_view_root(store)?;
    let reader = Reader::Branch(store);
    Sha256Jmt::new(&reader).put_value_set(
        changes
            .iter()
            .map(|(id, value)| (KeyHash(*id), value.map(|value| value.to_vec()))),
        version,
    )
}

pub(super) fn write(batch: &mut Batch, edits: TreeUpdateBatch, version: u64) -> Result<()> {
    let mut has_stale_root = false;
    for stale in edits.stale_node_index_batch {
        has_stale_root |= stale.node_key.nibble_path().num_nibbles() == 0;
        batch.delete(key(&stale.node_key)?);
    }
    for (node, data) in edits.node_batch.nodes() {
        if version > 0 && !has_stale_root && node.nibble_path().num_nibbles() == 0 {
            let old_root = NodeKey::new(version - 1, node.nibble_path().clone());
            batch.delete(key(&old_root)?);
        }
        batch.put_owned(key(node)?, borsh::to_vec(data)?);
    }
    Ok(())
}

const EMPTY: Key = *b"SPARSE_MERKLE_PLACEHOLDER_HASH__";

fn authenticate(domain: &[u8], left: &Key, right: &Key) -> Key {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(left);
    hash.update(right);
    hash.finalize().into()
}

fn branch(left: &Key, right: &Key) -> Key {
    authenticate(jmt::proof::INTERNAL_DOMAIN_SEPARATOR, left, right)
}

struct Subtree {
    key: Key,
    root: Key,
    depth: Option<usize>,
}

impl Subtree {
    fn leaf(key: Key, digest: Key) -> Self {
        Self {
            key,
            root: authenticate(
                jmt::proof::LEAF_DOMAIN_SEPARATOR,
                &key,
                &Sha256::digest(digest).into(),
            ),
            depth: None,
        }
    }

    fn at_depth(mut self, target: usize) -> Key {
        if let Some(depth) = self.depth {
            for bit in (target..depth).rev() {
                self.root = if self.key[bit / 8] & (0x80 >> (bit % 8)) == 0 {
                    branch(&self.root, &EMPTY)
                } else {
                    branch(&EMPTY, &self.root)
                };
            }
        }
        self.root
    }

    fn join(left: Self, right: Self, depth: usize) -> Self {
        Self {
            key: left.key,
            root: branch(&left.at_depth(depth + 1), &right.at_depth(depth + 1)),
            depth: Some(depth),
        }
    }
}

pub(super) fn root_from_sorted(
    records: impl IntoIterator<Item = Result<(Key, Key)>>,
) -> Result<Key> {
    let mut records = records.into_iter();
    let Some((mut previous, digest)) = records.next().transpose()? else {
        return Ok(EMPTY);
    };
    let mut right = Subtree::leaf(previous, digest);
    let mut frontier = Vec::<(usize, Subtree)>::new();
    for row in records {
        let (key, digest) = row?;
        ensure!(previous < key, "unordered or duplicate JMT records");
        let depth = previous
            .iter()
            .zip(key)
            .enumerate()
            .find_map(|(index, (&a, b))| {
                (a != b).then(|| index * 8 + (a ^ b).leading_zeros() as usize)
            })
            .expect("different JMT records");
        while frontier.last().is_some_and(|(bit, _)| *bit > depth) {
            let (bit, left) = frontier.pop().expect("unfinished JMT subtree");
            right = Subtree::join(left, right, bit);
        }
        frontier.push((depth, right));
        right = Subtree::leaf(key, digest);
        previous = key;
    }
    while let Some((bit, left)) = frontier.pop() {
        right = Subtree::join(left, right, bit);
    }
    Ok(right.at_depth(0))
}

const MEMBER_SAMPLES: usize = 512;

#[derive(Clone, Copy)]
pub struct CellAuditProofOptions {
    /// Number of queries sharing one proof-generation batch.
    pub batch_width: usize,
    /// Maximum decoded JMT nodes retained within one batch.
    pub jmt_cache_nodes: usize,
}

impl Default for CellAuditProofOptions {
    fn default() -> Self {
        Self {
            batch_width: MAX_CELL_PROOF_BATCH,
            jmt_cache_nodes: 0,
        }
    }
}

/// Audit a stopped replica at an independently authenticated Header ID.
/// Open the recovered image read-only and compare the leaf stream with its
/// authenticated Cell root before collecting bounded proof samples.
pub fn audit_cell_state(
    path: &Path,
    network: Byte32,
    expected_header: &Byte32,
) -> Result<serde_json::Value> {
    audit_cell_state_with_proofs(
        path,
        network,
        expected_header,
        CellAuditProofOptions::default(),
    )
}

pub fn audit_cell_state_with_proofs(
    path: &Path,
    network: Byte32,
    expected_header: &Byte32,
    proof_options: CellAuditProofOptions,
) -> Result<serde_json::Value> {
    ensure!(
        (1..=MAX_CELL_PROOF_BATCH).contains(&proof_options.batch_width),
        "audit batch width must be between 1 and {MAX_CELL_PROOF_BATCH}"
    );
    const MAX_JMT_CACHE_NODES: usize = 16_384;
    ensure!(
        proof_options.jmt_cache_nodes <= MAX_JMT_CACHE_NODES,
        "audit node cache is limited to {MAX_JMT_CACHE_NODES} nodes"
    );
    let opened = Instant::now();
    let database = ReadOnlyDB::open(&Options::default(), path)?;
    let open_us = opened.elapsed().as_micros();
    let marker = database
        .get(MARKER)?
        .context("missing Stem database layout")?;
    let is_jmt = marker.starts_with(LAYOUT);
    let layout = if is_jmt { LAYOUT } else { STORAGE_LAYOUT };
    let genesis = marker
        .strip_prefix(layout)
        .context("unknown Stem database layout")?;
    let genesis = Byte32::from_slice(genesis)?;
    let tip = Header::decode(&database.get(TIP)?.context("missing durable Stem tip")?)?;
    ensure!(
        &tip.id(&network) == expected_header,
        "recovered Stem tip differs from the expected Header"
    );

    let cells = Patricia::new(network, Namespace::Cells);
    let prefix = [STATE_RECORD, Namespace::Cells.tag()];
    let mut options = ReadOptions::default();
    options.fill_cache(false);
    let mut cursor = database.get_raw_iter(&options);
    cursor.seek(prefix);
    let rebuild_started = Instant::now();
    let mut count = 0u64;
    let mut content = Sha256::new_with_prefix(b"Stem research live cell content v1");
    let mut samples = BinaryHeap::<(u64, Key, Key)>::with_capacity(MEMBER_SAMPLES);
    let records = std::iter::from_fn(|| {
        let raw_key = cursor.key()?.strip_prefix(&prefix)?;
        let result = (|| {
            let id: Key = raw_key.try_into()?;
            let raw = cursor.value().context("missing durable Live Cell record")?;
            let (point, record) = record::decode_cell(raw)?;
            ensure!(
                record::cell_key(&cells, &point) == id,
                "miskeyed durable Live Cell"
            );
            let digest = cells.value_hash(raw);
            // Exclude the chain-specific origin. The independently rebuilt
            // commitment still authenticates the full canonical record.
            content.update(id);
            content.update(Sha256::digest(record.cell.as_slice()));
            count += 1;
            let priority = u64::from_be_bytes(id[24..].try_into()?);
            let sample = (priority, id, digest);
            if samples.len() < MEMBER_SAMPLES {
                samples.push(sample);
            } else if let Some(mut highest) = samples.peek_mut()
                && sample < *highest
            {
                *highest = sample;
            }
            Ok((id, digest))
        })();
        cursor.next();
        Some(result)
    });
    let root = if is_jmt {
        root_from_sorted(records)?
    } else {
        cells.root_from_sorted_records(records)?
    };
    ensure!(
        tip.state.cell_root.as_slice() == root,
        "durable Cell records differ from certified root"
    );
    let rebuild_us = rebuild_started.elapsed().as_micros();
    let members = samples.len();
    let mut samples = samples.into_sorted_vec().into_iter();
    let mut queries = Vec::with_capacity(members + MEMBER_SAMPLES);
    for index in 0..MEMBER_SAMPLES {
        if let Some((_, id, digest)) = samples.next() {
            queries.push((id, Some(digest)));
        }
        let mut hasher = Sha256::new_with_prefix(b"Stem research absent Live Cell v1");
        hasher.update((index as u32).to_le_bytes());
        let id: Key = hasher.finalize().into();
        ensure!(
            database
                .get_pinned_opt(state_key(STATE_RECORD, Namespace::Cells, &id), &options)?
                .is_none(),
            "a control query unexpectedly addresses a Live Cell"
        );
        queries.push((id, None));
    }

    let version = tip.context.height;
    let stored = proof_audit::ProofStorage::new(&database, &options);
    let stats = if is_jmt {
        proof_audit::jmt(&stored, &cells, version, root, &queries, proof_options)?
    } else {
        proof_audit::patricia(&stored, &cells, root, &queries, proof_options.batch_width)?
    };
    Ok(serde_json::json!({
        "engine": if is_jmt { "sha256-jmt-0.12" } else { "compact-patricia" },
        "height": version,
        "header": hex::encode(expected_header.as_slice()),
        "genesis": hex::encode(genesis.as_slice()),
        "cell_root": hex::encode(root),
        "system_root": hex::encode(tip.state.system_root.as_slice()),
        "receipt_root": hex::encode(tip.receipt_root.as_slice()),
        "content_digest_v1": hex::encode(content.finalize()),
        "live_cells": count,
        "membership_queries": members,
        "absence_queries": MEMBER_SAMPLES,
        "proof_batch_width": proof_options.batch_width,
        "jmt_proof_cache_limit_nodes": if is_jmt { proof_options.jmt_cache_nodes } else { 0 },
        "jmt_proof_cache_peak_nodes": stats.peak_cached_nodes,
        "jmt_proof_cache_hits": stats.cache_hits,
        "proof_reads": stored.metrics(),
        "proof_bytes": stats.bytes,
        "proof_digest": hex::encode(stats.digest.finalize()),
        "open_us": open_us,
        "rebuild_us": rebuild_us,
        "proof_generate_encode_us": stats.generate.as_micros(),
        "proof_decode_verify_us": stats.verify.as_micros()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_folds_match_upstream_jmt_roots() -> Result<()> {
        for count in [0usize, 1, 2, 9, 64, 512, 4096] {
            let mut pairs = Vec::new();
            for i in 0..count {
                let mut key: Key = Sha256::digest(i.to_le_bytes()).into();
                if i % 3 == 0 {
                    key[..24].fill(0x31);
                }
                let digest: Key = Sha256::digest(key).into();
                pairs.push((key, digest));
            }
            pairs.sort_unstable_by_key(|(key, _)| *key);
            let changes = pairs
                .iter()
                .map(|&(key, digest)| (KeyHash(key), Some(digest.to_vec())));
            let expected = Sha256Jmt::new(&Reader::Empty).put_value_set(changes, 0)?.0;
            assert_eq!(
                bootstrap(pairs.clone())?.0,
                expected,
                "{count} bulk records"
            );
            let recomputed = root_from_sorted(pairs.into_iter().map(Ok))?;
            assert_eq!(expected.0, recomputed, "{count} records");
        }
        let duplicate = ([0; 32], [5; 32]);
        assert!(root_from_sorted([Ok(duplicate), Ok(duplicate)]).is_err());
        Ok(())
    }
}
