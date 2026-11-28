//! Development-only comparison using versioned JMT nodes and the native write path.

use std::{borrow::Cow, cell::Cell, collections::BinaryHeap, path::Path, time::Instant};

#[cfg(feature = "native-jmt-mutation-memo")]
use std::cell::RefCell;

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
    light::FinalityVerifier,
    patricia::{Key, Namespace, Patricia},
    record,
};

use super::{
    Batch, HEADER, JMT_NODE, JmtMutationReads, MARKER, MAX_CELL_PROOF_BATCH, STATE_RECORD, STORAGE_LAYOUT, Spec,
    PreparedCells, Store, TIP, index_key, state_key,
};

mod proof_audit;

pub(super) const LAYOUT: &[u8] = b"Stem research JMT 0.12 only\0";
pub(super) const FIXED_ROOT_LAYOUT: &[u8] = b"Stem research JMT 0.12 fixed root\0";
pub(super) const FIXED_ROOT_KEY: &[u8] = &[JMT_NODE];
pub(super) const MUTATION_MEMO_NODES: usize = 1024;

fn key(node: &NodeKey) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(51);
    out.push(JMT_NODE);
    out.extend(node.version().to_be_bytes());
    borsh::to_writer(&mut out, node.nibble_path())?;
    Ok(out)
}

fn is_root(node: &NodeKey) -> bool {
    node.nibble_path().num_nibbles() == 0
}

fn read_key(node: &NodeKey, fixed_root: bool, height: Version) -> Result<Cow<'static, [u8]>> {
    if fixed_root && is_root(node) {
        ensure!(node.version() == height, "JMT root read outside the exact state view");
        return Ok(Cow::Borrowed(FIXED_ROOT_KEY));
    }
    Ok(Cow::Owned(key(node)?))
}

enum Reader<'a> {
    Empty,
    Branch {
        store: &'a Store,
        retirement: Option<&'a Retirement>,
        reads: Cell<JmtMutationReads>,
        #[cfg(feature = "native-jmt-mutation-memo")]
        memo: Option<RefCell<lru::LruCache<NodeKey, Node>>>,
    },
}

impl<'a> Reader<'a> {
    fn branch(store: &'a Store, retirement: Option<&'a Retirement>, _memo_nodes: usize) -> Self {
        Self::Branch {
            store,
            retirement,
            reads: Cell::default(),
            #[cfg(feature = "native-jmt-mutation-memo")]
            memo: (_memo_nodes != 0).then(|| RefCell::new(lru::LruCache::new(_memo_nodes))),
        }
    }

    fn reads(&self) -> JmtMutationReads {
        match self {
            Self::Branch { reads, .. } => reads.get(),
            Self::Empty => JmtMutationReads::default(),
        }
    }
}

impl TreeReader for Reader<'_> {
    fn get_node_option(&self, node: &NodeKey) -> Result<Option<Node>> {
        let Self::Branch {
            store,
            retirement,
            reads,
            #[cfg(feature = "native-jmt-mutation-memo")]
            memo,
        } = self else {
            return Ok(None);
        };
        let mut counts = reads.get();
        counts.requests += 1;
        #[cfg(feature = "native-jmt-mutation-memo")]
        if let Some(memo) = memo
            && let Some(found) = memo.borrow_mut().get(node).cloned()
        {
            counts.memo_hits += 1;
            reads.set(counts);
            return Ok(Some(found));
        }
        let slot = read_key(node, store.cell_storage.fixed_jmt_root(), store.tip.context.height)?;
        let result = store.read(slot)?.as_deref().map(borsh::from_slice).transpose()?;
        counts.backing_reads += 1;
        if let (Some(retirement), Some(Node::Leaf(leaf))) = (retirement, &result) {
            retirement.observe(leaf.key_hash().0);
        }
        #[cfg(feature = "native-jmt-mutation-memo")]
        if let (Some(memo), Some(found)) = (memo, &result) {
            let mut memo = memo.borrow_mut();
            memo.put(node.clone(), found.clone());
            counts.peak_nodes = counts.peak_nodes.max(memo.len() as u64);
        }
        reads.set(counts);
        Ok(result)
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
    check_reader_root(store, &Reader::branch(store, None, 0)).map(drop)
}

fn check_reader_root(store: &Store, reader: &Reader<'_>) -> Result<RootHash> {
    let version = store.tip.context.height;
    let root = Sha256Jmt::new(reader).get_root_hash(version)?;
    ensure!(
        root.0.as_slice() == store.tip.state.cell_root.as_slice(),
        "JMT nodes differ from the state view root at height {version}"
    );
    Ok(root)
}

pub(super) fn update(
    store: &Store,
    changes: &[(Key, Option<Key>)],
    version: u64,
    observe_retirement: bool,
    memo_nodes: usize,
) -> Result<Update> {
    let keep_root = store.cell_storage.fixed_jmt_root() && changes.is_empty();
    let retirement = observe_retirement.then(|| Retirement::plan(changes));
    let reader = Reader::branch(store, retirement.as_ref(), if keep_root { 0 } else { memo_nodes });
    let parent_root = check_reader_root(store, &reader)?;
    if keep_root {
        return Ok(Update { root: parent_root, edits: None, retirement: None, reads: reader.reads() });
    }
    let (root, edits) = Sha256Jmt::new(&reader).put_value_set(
        changes
            .iter()
            .map(|(id, value)| (KeyHash(*id), value.map(|value| value.to_vec()))),
        version,
    )?;
    let reads = reader.reads();
    Ok(Update { root, edits: Some(edits), retirement, reads })
}

struct Retirement {
    // The caller supplies distinct keys in hash order. Keep one marker per
    // requested deletion, with no storage proportional to the tree size.
    entries: Vec<(Key, Cell<bool>)>,
}

impl Retirement {
    fn plan(changes: &[(Key, Option<Key>)]) -> Self {
        let entries = changes
            .iter()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| (*key, Cell::new(false)))
            .collect();
        Self { entries }
    }

    fn observe(&self, key: Key) {
        if let Ok(slot) = self.entries.binary_search_by(|(candidate, _)| candidate.cmp(&key)) {
            self.entries[slot].1.set(true);
        }
    }
}

pub(super) struct Update {
    pub(super) root: RootHash,
    pub(super) reads: JmtMutationReads,
    edits: Option<TreeUpdateBatch>,
    retirement: Option<Retirement>,
}

impl Update {
    pub(super) fn write(
        self,
        store: &Store,
        batch: &mut Batch,
        live: &PreparedCells,
        version: u64,
    ) -> Result<()> {
        let Some(edits) = self.edits else {
            return Ok(());
        };
        if let Some(retirement) = self.retirement {
            // A TreeReader load comes from the exact parent. The upstream
            // mutation cache handles nodes generated by this update. A
            // deleted key moved within the tree must have been read from
            // the parent before it could be moved.
            for (id, present) in retirement.entries {
                if present.into_inner() {
                    batch.delete(state_key(STATE_RECORD, Namespace::Cells, &id));
                }
            }
        } else {
            let mut retired = Vec::new();
            for (point, _) in live.iter().filter(|(_, row)| row.is_none()) {
                let id = record::cell_key(&store.cells, point);
                let key = state_key(STATE_RECORD, Namespace::Cells, &id);
                // Speculative Cell rows can live only in the prepared delta.
                let present = match store.branch_cell(point) {
                    Some(row) => row.is_some(),
                    None => store.read(key)?.is_some(),
                };
                if present {
                    retired.push(id);
                }
            }
            retired.sort_unstable();
            for id in retired {
                batch.delete(state_key(STATE_RECORD, Namespace::Cells, &id));
            }
        }
        write(batch, edits, version, store.cell_storage.fixed_jmt_root())
    }
}

pub(super) fn write(batch: &mut Batch, edits: TreeUpdateBatch, version: u64, fixed_root: bool) -> Result<()> {
    let mut has_stale_root = false;
    for stale in edits.stale_node_index_batch {
        let root = is_root(&stale.node_key);
        if fixed_root && root {
            continue;
        }
        has_stale_root |= root;
        batch.delete(key(&stale.node_key)?);
    }
    let mut wrote_root = false;
    for (node, data) in edits.node_batch.nodes() {
        if is_root(node) {
            if fixed_root {
                ensure!(!wrote_root && node.version() == version, "unexpected JMT root write");
                batch.put_owned(FIXED_ROOT_KEY, borsh::to_vec(data)?);
                wrote_root = true;
                continue;
            }
            if version > 0 && !has_stale_root {
                let old_root = NodeKey::new(version - 1, node.nibble_path().clone());
                batch.delete(key(&old_root)?);
            }
        }
        batch.put_owned(key(node)?, borsh::to_vec(data)?);
    }
    ensure!(!fixed_root || wrote_root, "missing JMT root write");
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
    /// Use the same bounded record reader on both engines.
    pub shared_record_batches: bool,
    /// Use the native upper-frontier reader for Patricia proofs.
    pub patricia_frontier: bool,
}

impl Default for CellAuditProofOptions {
    fn default() -> Self {
        Self {
            batch_width: MAX_CELL_PROOF_BATCH,
            jmt_cache_nodes: 0,
            shared_record_batches: false,
            patricia_frontier: false,
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
    audit_cell_state_at(path, network, expected_header, proof_options, None)
}

/// Authenticate the recovered tip of a development database and its ancestry
/// from the independently replayed measurement Header before auditing Cells.
pub fn audit_recovered_cell_state(
    path: &Path,
    spec: &Spec,
    genesis: &Byte32,
    measured_height: u64,
    measured_header: &Byte32,
    options: CellAuditProofOptions,
) -> Result<serde_json::Value> {
    ensure!(
        spec.stake.is_none() && spec.bridge.is_none(),
        "recovery comparison requires an isolated development chain"
    );
    let members = spec
        .development_validators
        .iter()
        .map(|(key, power)| {
            Ok(stem_stake::Member {
                key: *key,
                power: u64::try_from(*power)?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let verifier = FinalityVerifier::new(spec.network.clone(), genesis.clone(), &members)?;
    audit_cell_state_at(
        path,
        spec.network.clone(),
        measured_header,
        options,
        Some((genesis, measured_height, verifier)),
    )
}

fn audit_cell_state_at(
    path: &Path,
    network: Byte32,
    expected_header: &Byte32,
    proof_options: CellAuditProofOptions,
    recovery: Option<(&Byte32, u64, FinalityVerifier)>,
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
    let fixed_root = marker.starts_with(FIXED_ROOT_LAYOUT);
    let is_jmt = fixed_root || marker.starts_with(LAYOUT);
    ensure!(
        !proof_options.patricia_frontier
            || (!is_jmt && cfg!(feature = "native-batched-patricia-proofs")),
        "frontier audit requires a Patricia store and native-batched-patricia-proofs"
    );
    let layout = if fixed_root {
        FIXED_ROOT_LAYOUT
    } else if is_jmt {
        LAYOUT
    } else {
        STORAGE_LAYOUT
    };
    let genesis = marker
        .strip_prefix(layout)
        .context("unknown Stem database layout")?;
    let genesis = Byte32::from_slice(genesis)?;
    let tip = Header::decode(&database.get(TIP)?.context("missing durable Stem tip")?)?;
    let tip_id = tip.id(&network);
    let recovery_started = Instant::now();
    let measured_height = if let Some((expected_genesis, height, verifier)) = recovery {
        ensure!(&genesis == expected_genesis, "wrong recovered genesis");
        ensure!(
            tip.context.height >= height,
            "recovered tip predates the measurement"
        );
        let certified = crate::jolteon::verify_development_recovery_tip(
            &verifier,
            tip.context.height,
            |key| Ok(database.get(index_key(b'j', key))?.map(|raw| raw.to_vec())),
        )?;
        ensure!(certified == tip, "durable tip differs from its certificate");
        let mut parent = if height == tip.context.height {
            tip_id.clone()
        } else {
            tip.context.parent.clone()
        };
        for current in (height..tip.context.height).rev() {
            let raw = database
                .get(index_key(HEADER, &current.to_be_bytes()))?
                .context("recovered database lacks the ancestry of the measured height")?;
            let header = Header::decode(&raw)?;
            ensure!(
                header.context.height == current && header.id(&network) == parent,
                "broken recovered Header ancestry"
            );
            parent = header.context.parent;
        }
        ensure!(
            &parent == expected_header,
            "recovered tip does not descend from the measured Header"
        );
        Some(height)
    } else {
        ensure!(
            &tip_id == expected_header,
            "recovered Stem tip differs from the expected Header"
        );
        None
    };
    let recovery_us = measured_height.map(|_| recovery_started.elapsed().as_micros());

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

    let mut query_digest = Sha256::new_with_prefix(b"Stem research proof query order v1");
    for (id, digest) in &queries {
        query_digest.update(id);
        query_digest.update([u8::from(digest.is_some())]);
    }
    let version = tip.context.height;
    let stored = proof_audit::ProofStorage::new(&database, &options);
    let stats = if is_jmt {
        proof_audit::jmt(&stored, &cells, version, root, &queries, proof_options, fixed_root)?
    } else {
        proof_audit::patricia(&stored, &cells, root, &queries, proof_options)?
    };
    Ok(serde_json::json!({
        "engine": if is_jmt { "sha256-jmt-0.12" } else { "compact-patricia" },
        "height": version,
        "jmt_fixed_root": fixed_root,
        "header": hex::encode(tip_id.as_slice()),
        "measured_height": measured_height,
        "measured_header": measured_height.map(|_| hex::encode(expected_header.as_slice())),
        "recovery_authentication_us": recovery_us,
        "genesis": hex::encode(genesis.as_slice()),
        "cell_root": hex::encode(root),
        "system_root": hex::encode(tip.state.system_root.as_slice()),
        "receipt_root": hex::encode(tip.receipt_root.as_slice()),
        "content_digest_v1": hex::encode(content.finalize()),
        "live_cells": count,
        "membership_queries": members,
        "absence_queries": MEMBER_SAMPLES,
        "proof_batch_width": proof_options.batch_width,
        "shared_proof_record_batches": proof_options.shared_record_batches,
        "patricia_proof_frontier": proof_options.patricia_frontier,
        "jmt_proof_cache_limit_nodes": if is_jmt { proof_options.jmt_cache_nodes } else { 0 },
        "jmt_proof_cache_peak_nodes": stats.peak_cached_nodes,
        "jmt_proof_cache_hits": stats.cache_hits,
        "proof_reads": stored.metrics(),
        "proof_query_digest": hex::encode(query_digest.finalize()),
        "shared_record_fetch_us": proof_options.shared_record_batches.then(|| stats.record_fetch.as_micros()),
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
