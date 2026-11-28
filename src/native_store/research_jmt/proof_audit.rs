use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use ckb_db::internal::{DBPinnableSlice, ReadOnlyDB, ReadOptions, prelude::*};
use jmt::{
    KeyHash, RootHash, Sha256Jmt, Version,
    proof::SparseMerkleProof,
    storage::{LeafNode, Node, NodeKey, TreeReader},
};
use sha2::{Digest, Sha256};
use stem_ledger::patricia::{Key, Namespace, Patricia, Proof};

use super::{CellAuditProofOptions, key};
use crate::native_store::{STATE_NODE, STATE_RECORD, node_storage, state_key};

type Query = (Key, Option<Key>);

#[derive(Clone, Copy, Default, serde::Serialize)]
struct Reads {
    calls: u64,
    returned_bytes: u64,
}

#[derive(Clone, Copy, Default, serde::Serialize)]
pub(super) struct ProofReads {
    nodes: Reads,
    cells: Reads,
}

enum ReadKind {
    Node,
    Cell,
}

/// Counts actual RocksDB calls made during proof construction, including misses.
pub(super) struct ProofStorage<'a> {
    database: &'a ReadOnlyDB,
    options: &'a ReadOptions,
    reads: Cell<ProofReads>,
}

impl<'a> ProofStorage<'a> {
    pub(super) fn new(database: &'a ReadOnlyDB, options: &'a ReadOptions) -> Self {
        Self {
            database,
            options,
            reads: Cell::default(),
        }
    }

    pub(super) fn metrics(&self) -> ProofReads {
        self.reads.get()
    }

    fn read(&self, key: impl AsRef<[u8]>, kind: ReadKind) -> Result<Option<DBPinnableSlice<'a>>> {
        let value = self.database.get_pinned_opt(key, self.options)?;
        let mut reads = self.reads.get();
        let tally = match kind {
            ReadKind::Node => &mut reads.nodes,
            ReadKind::Cell => &mut reads.cells,
        };
        tally.calls += 1;
        tally.returned_bytes += value.as_deref().map_or(0, <[u8]>::len) as u64;
        self.reads.set(reads);
        Ok(value)
    }
}

struct JmtReader<'a> {
    storage: &'a ProofStorage<'a>,
    cells: &'a Patricia,
    version: Version,
    limit: usize,
    nodes: RefCell<HashMap<NodeKey, Node>>,
    hits: Cell<usize>,
}

impl TreeReader for JmtReader<'_> {
    fn get_node_option(&self, node: &NodeKey) -> Result<Option<Node>> {
        if self.limit != 0
            && let Some(found) = self.nodes.borrow().get(node)
        {
            self.hits.set(self.hits.get() + 1);
            return Ok(Some(found.clone()));
        }
        let fetched: Option<Node> = self
            .storage
            .read(key(node)?, ReadKind::Node)?
            .as_deref()
            .map(borsh::from_slice)
            .transpose()?;
        if let Some(found) = &fetched
            && self.limit != 0
        {
            let mut nodes = self.nodes.borrow_mut();
            if nodes.len() < self.limit {
                nodes.insert(node.clone(), found.clone());
            }
        }
        Ok(fetched)
    }

    fn get_value_option(&self, version: Version, id: KeyHash) -> Result<Option<Vec<u8>>> {
        ensure!(
            version == self.version,
            "JMT proof value version differs from the view"
        );
        Ok(self
            .storage
            .read(
                state_key(STATE_RECORD, Namespace::Cells, &id.0),
                ReadKind::Cell,
            )?
            .as_deref()
            .map(|bytes| self.cells.value_hash(bytes).to_vec()))
    }

    fn get_rightmost_leaf(&self) -> Result<Option<(NodeKey, LeafNode)>> {
        bail!("state restoration is outside the local JMT trial")
    }
}

#[derive(Default)]
pub(super) struct ProofStats {
    pub(super) bytes: usize,
    pub(super) generate: Duration,
    pub(super) verify: Duration,
    pub(super) digest: Sha256,
    pub(super) peak_cached_nodes: usize,
    pub(super) cache_hits: usize,
}

impl ProofStats {
    fn collect<V, P: AsRef<[u8]>>(&mut self, proofs: Vec<(V, P)>) {
        for (_, proof) in proofs {
            let proof = proof.as_ref();
            self.bytes += proof.len();
            self.digest.update((proof.len() as u64).to_le_bytes());
            self.digest.update(proof);
        }
    }
}

pub(super) fn jmt(
    storage: &ProofStorage<'_>,
    cells: &Patricia,
    version: Version,
    root: Key,
    queries: &[Query],
    options: CellAuditProofOptions,
) -> Result<ProofStats> {
    let mut reader = JmtReader {
        storage,
        cells,
        version,
        limit: 0,
        nodes: RefCell::default(),
        hits: Cell::default(),
    };
    ensure!(
        Sha256Jmt::new(&reader).get_root_hash(version)?.0 == root,
        "stored JMT root differs from durable Header"
    );
    storage.reads.set(ProofReads::default());
    reader.limit = options.jmt_cache_nodes;
    let tree = Sha256Jmt::new(&reader);

    let mut stats = ProofStats::default();
    for group in queries.chunks(options.batch_width) {
        let started = Instant::now();
        let encoded = group
            .iter()
            .map(|&(id, _)| {
                let (value, proof) = tree.get_with_proof(KeyHash(id), version)?;
                Ok((value, borsh::to_vec(&proof)?))
            })
            .collect::<Result<Vec<_>>>()?;
        let mut nodes = reader.nodes.borrow_mut();
        stats.peak_cached_nodes = stats.peak_cached_nodes.max(nodes.len());
        nodes.clear();
        drop(nodes);
        stats.generate += started.elapsed();

        let started = Instant::now();
        for (&(id, expected), (value, proof)) in group.iter().zip(&encoded) {
            ensure!(
                value.as_deref() == expected.as_ref().map(Key::as_slice),
                "incorrect JMT proof value"
            );
            let parsed: SparseMerkleProof<Sha256> = borsh::from_slice(proof)?;
            parsed.verify(
                RootHash(root),
                KeyHash(id),
                expected.as_ref().map(Key::as_slice),
            )?;
        }
        stats.verify += started.elapsed();
        stats.collect(encoded);
    }
    stats.cache_hits = reader.hits.get();
    Ok(stats)
}

pub(super) fn patricia(
    storage: &ProofStorage<'_>,
    cells: &Patricia,
    root: Key,
    queries: &[Query],
    batch_width: usize,
) -> Result<ProofStats> {
    let mut stats = ProofStats::default();
    for group in queries.chunks(batch_width) {
        let started = Instant::now();
        let mut tree = cells.edit(root, |id| {
            storage
                .read(state_key(STATE_NODE, Namespace::Cells, id), ReadKind::Node)?
                .as_deref()
                .map(node_storage::canonical)
                .transpose()
        });
        let keys: Vec<_> = group.iter().map(|&(id, _)| id).collect();
        let paths = tree.prove_many(&keys)?;
        let encoded = keys
            .into_iter()
            .zip(paths)
            .map(|(id, (value, proof))| {
                if let Some(digest) = value {
                    let raw = storage
                        .read(
                            state_key(STATE_RECORD, Namespace::Cells, &id),
                            ReadKind::Cell,
                        )?
                        .context("missing proven Patricia record")?;
                    ensure!(
                        cells.value_hash(&raw) == digest,
                        "proven Patricia record changed"
                    );
                }
                Ok((value, proof.encode()?))
            })
            .collect::<Result<Vec<_>>>()?;
        stats.generate += started.elapsed();

        let started = Instant::now();
        for (&(id, expected), (value, proof)) in group.iter().zip(&encoded) {
            ensure!(*value == expected, "incorrect Patricia proof value");
            ensure!(
                cells.verify(&root, &id, expected.as_ref(), &Proof::decode(proof)?),
                "invalid Patricia proof"
            );
        }
        stats.verify += started.elapsed();
        stats.collect(encoded);
    }
    Ok(stats)
}
