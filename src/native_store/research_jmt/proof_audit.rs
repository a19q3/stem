use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use ckb_db::internal::{DBPinnableSlice, DBVector, ReadOnlyDB, ReadOptions, prelude::*};
use jmt::{
    KeyHash, RootHash, Sha256Jmt, Version,
    proof::SparseMerkleProof,
    storage::{LeafNode, Node, NodeKey, TreeReader},
};
use sha2::{Digest, Sha256};
use stem_ledger::patricia::{Key, Namespace, Patricia, Proof};

use super::{CellAuditProofOptions, read_key};
use crate::native_store::{STATE_NODE, STATE_RECORD, node_storage, state_key};

type Query = (Key, Option<Key>);

#[derive(Clone, Copy, Default, serde::Serialize)]
struct Reads {
    calls: u64,
    keys: u64,
    multi_get_calls: u64,
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
        self.record(kind, 1, 1, 0, value.as_deref().map_or(0, <[u8]>::len));
        Ok(value)
    }

    fn read_many(&self, keys: &[[u8; 34]], kind: ReadKind) -> Result<Vec<Option<DBVector>>> {
        const MIN_MULTI_GET: usize = 4;
        let grouped = keys.len() >= MIN_MULTI_GET;
        let values = if grouped {
            self.database.multi_get_opt(keys, self.options)
        } else {
            keys.iter()
                .map(|key| self.database.get_opt(key, self.options))
                .collect()
        }
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?;
        ensure!(values.len() == keys.len(), "wrong audit batch read length");
        let returned_bytes = values.iter().flatten().map(|value| value.len()).sum();
        let calls = if grouped { 1 } else { keys.len() };
        self.record(
            kind,
            calls,
            keys.len(),
            usize::from(grouped),
            returned_bytes,
        );
        Ok(values)
    }

    fn record(&self, kind: ReadKind, calls: usize, keys: usize, batches: usize, bytes: usize) {
        let mut reads = self.reads.get();
        let tally = match kind {
            ReadKind::Node => &mut reads.nodes,
            ReadKind::Cell => &mut reads.cells,
        };
        tally.calls += calls as u64;
        tally.keys += keys as u64;
        tally.multi_get_calls += batches as u64;
        tally.returned_bytes += bytes as u64;
        self.reads.set(reads);
    }

    fn check_record_batch(
        &self,
        cells: &Patricia,
        group: &[Query],
    ) -> Result<HashMap<Key, Option<Key>>> {
        const RECORD_READ_WIDTH: usize = 8;
        let mut digests = HashMap::with_capacity(group.len());
        for queries in group.chunks(RECORD_READ_WIDTH) {
            let keys: Vec<_> = queries
                .iter()
                .map(|(id, _)| state_key(STATE_RECORD, Namespace::Cells, id))
                .collect();
            for (&(id, expected), raw) in queries.iter().zip(self.read_many(&keys, ReadKind::Cell)?)
            {
                let digest = raw.as_deref().map(|bytes| cells.value_hash(bytes));
                ensure!(
                    digest == expected,
                    "audit Live Cell changed after root reconstruction"
                );
                digests.insert(id, digest);
            }
        }
        Ok(digests)
    }
}

struct JmtReader<'a> {
    storage: &'a ProofStorage<'a>,
    cells: &'a Patricia,
    version: Version,
    fixed_root: bool,
    limit: usize,
    nodes: RefCell<HashMap<NodeKey, Node>>,
    values: RefCell<Option<HashMap<Key, Option<Key>>>>,
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
            .read(read_key(node, self.fixed_root, self.version)?, ReadKind::Node)?
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
        if let Some(values) = self.values.borrow().as_ref() {
            return Ok(values
                .get(&id.0)
                .context("JMT proof value outside audit batch")?
                .map(|digest| digest.to_vec()));
        }
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
    pub(super) record_fetch: Duration,
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
    fixed_root: bool,
) -> Result<ProofStats> {
    let mut reader = JmtReader {
        storage,
        cells,
        version,
        fixed_root,
        limit: 0,
        nodes: RefCell::default(),
        values: RefCell::default(),
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
        if options.shared_record_batches {
            let records = storage.check_record_batch(cells, group)?;
            stats.record_fetch += started.elapsed();
            *reader.values.borrow_mut() = Some(records);
        }
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
        reader.values.borrow_mut().take();
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
    options: CellAuditProofOptions,
) -> Result<ProofStats> {
    let mut stats = ProofStats::default();
    for group in queries.chunks(options.batch_width) {
        let started = Instant::now();
        let records = options
            .shared_record_batches
            .then(|| storage.check_record_batch(cells, group))
            .transpose()?;
        if records.is_some() {
            stats.record_fetch += started.elapsed();
        }
        let mut tree = cells.edit(root, |id| {
            storage
                .read(state_key(STATE_NODE, Namespace::Cells, id), ReadKind::Node)?
                .as_deref()
                .map(node_storage::canonical)
                .transpose()
        });
        let keys: Vec<_> = group.iter().map(|&(id, _)| id).collect();
        #[cfg(feature = "native-batched-patricia-proofs")]
        let batched = options
            .patricia_frontier
            .then(|| {
                cells.prove_many_batched(root, &keys, |ids| {
                    let keys: Vec<_> = ids
                        .iter()
                        .map(|id| state_key(STATE_NODE, Namespace::Cells, id))
                        .collect();
                    storage
                        .read_many(&keys, ReadKind::Node)?
                        .into_iter()
                        .map(|value| value.as_deref().map(node_storage::canonical).transpose())
                        .collect()
                })
            })
            .transpose()?;
        #[cfg(not(feature = "native-batched-patricia-proofs"))]
        let batched = None;
        let paths = match batched {
            Some(paths) => paths,
            None => tree.prove_many(&keys)?,
        };
        let encoded = keys
            .into_iter()
            .zip(paths)
            .map(|(id, (value, proof))| {
                if let Some(records) = &records {
                    ensure!(
                        records.get(&id) == Some(&value),
                        "proven Patricia record changed"
                    );
                } else if let Some(digest) = value {
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
        drop(records);
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
