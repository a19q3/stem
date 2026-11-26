//! Offline batch proof read comparison on one immutable Patricia snapshot.
use anyhow::{Context, Result, ensure};
use ckb_types::prelude::*;
use rayon::prelude::*;
use serde_json::json;
use std::{collections::HashMap, sync::Arc, time::Instant};
use stem_ledger::patricia::{Key, Namespace, Patricia, Proof};
use stem_stake::patricia::{Commitment, Node, canonical_prefix, side};

type Snapshot = HashMap<Key, Arc<[u8]>>;
type Query = (Key, Option<Key>, Proof);
type Counts = (usize, usize);

fn walk(
    commit: &Commitment,
    db: &Snapshot,
    id: Key,
    group: &mut [Query],
    depth: usize,
) -> Result<Counts> {
    if group.is_empty() || id == commit.empty_root() {
        return Ok((0, 0));
    }
    let raw = db.get(&id).context("missing Patricia node")?;
    ensure!(commit.node_hash(raw) == id, "wrong stored hash");
    let node = Node::decode(raw)?;
    for (_, _, proof) in group.iter_mut() {
        proof.nodes.push(raw.to_vec());
    }
    let mut counts = (1, raw.len());
    match node {
        Node::Leaf { key, value } => {
            for (query, result, _) in group.iter_mut() {
                if *query == key {
                    *result = Some(value);
                }
            }
        }
        Node::Branch {
            prefix,
            bits,
            children,
        } => {
            ensure!(usize::from(bits) >= depth, "decreasing branch depth");
            // The sorted interval with this prefix is contiguous. Divergent
            // queries finish with the branch already appended to their path.
            let lo = group.partition_point(|(key, _, _)| canonical_prefix(key, bits) < prefix);
            let hi = group.partition_point(|(key, _, _)| canonical_prefix(key, bits) <= prefix);
            let matches = &mut group[lo..hi];
            let split = matches.partition_point(|(key, _, _)| side(key, bits) == 0);
            let (left, right) = matches.split_at_mut(split);
            for (child, queries) in children.into_iter().zip([left, right]) {
                let (reads, bytes) = walk(commit, db, child, queries, usize::from(bits) + 1)?;
                counts.0 += reads;
                counts.1 += bytes;
            }
        }
    }
    Ok(counts)
}

fn probe(size: usize, prefix: bool) -> Result<()> {
    let network = [5; 32];
    let tree = Patricia::new(network.pack(), Namespace::Cells);
    let commit = Commitment::new(network, Namespace::Cells);
    let key = |i: usize| {
        let mut k = tree.record_key(&(i as u64).to_le_bytes());
        if prefix {
            k[..8].fill(0xab);
        }
        k
    };
    let mut changes: Vec<_> = (0..size).map(|i| (key(i), Some(key(i + size)))).collect();
    changes.sort_unstable_by_key(|x| x.0);
    let mut edit = tree.edit(tree.empty_root(), |_| Ok(None::<Vec<u8>>));
    edit.set_batch(&changes, |_, _, _| {})?;
    let root = edit.root();
    let db: Snapshot = edit
        .into_writes()
        .into_iter()
        .filter_map(|(k, v)| Some((k, Arc::from(v?))))
        .collect();
    drop(changes);
    for count in [128, 1024, 4096] {
        let input: Vec<_> = (0..count)
            .map(|i| {
                let mut k = key(if i % 3 == 0 {
                    size + i
                } else {
                    i.wrapping_mul(65537) % size
                });
                if prefix && i % 13 == 0 {
                    k[0] ^= 1;
                }
                k
            })
            .collect();
        let mut expected = Vec::with_capacity(count);
        let mut reads = 0;
        let mut bytes = 0;
        let start = Instant::now();
        let mut oracle = tree.edit(root, |id| {
            reads += 1;
            if let Some(raw) = db.get(id) {
                bytes += raw.len();
                Ok(Some(raw.as_ref()))
            } else {
                Ok(None)
            }
        });
        for query in &input {
            expected.push(oracle.prove(query)?);
        }
        let oracle_ns = start.elapsed().as_nanos();
        for ((value, proof), query) in expected.iter().zip(&input) {
            ensure!(
                tree.verify(&root, query, value.as_ref(), proof),
                "invalid oracle proof"
            );
        }
        for workers in [1, 2, 4, 8, 16, 32] {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .build()?;
            let start = Instant::now();
            // Preserve caller order for output comparison, including duplicate keys.
            let mut order: Vec<_> = (0..count).collect();
            order.sort_unstable_by_key(|&i| input[i]);
            let mut rows: Vec<Query> = order
                .iter()
                .map(|&i| (input[i], None, Proof { nodes: vec![] }))
                .collect();
            let chunks: Vec<Counts> = pool.install(|| {
                rows.par_chunks_mut(count.div_ceil(workers))
                    .map(|chunk| walk(&commit, &db, root, chunk, 0))
                    .collect::<Result<Vec<_>>>()
            })?;
            let mut scattered: Vec<_> = (0..count).map(|_| None).collect();
            for (row, i) in rows.into_iter().zip(&order) {
                scattered[*i] = Some((row.1, row.2));
            }
            let elapsed = start.elapsed().as_nanos();
            for (row, (v, p)) in scattered.into_iter().zip(&expected) {
                let (value, proof) = row.unwrap();
                ensure!(
                    value == *v && proof.encode()? == p.encode()?,
                    "batch read differed"
                );
            }
            println!(
                "{}",
                json!({"live_keys":size,"shared_prefix":prefix,"queries":count,"workers":workers,
                "oracle_reads":reads,"oracle_bytes":bytes,"oracle_ns":oracle_ns,"batch_ns":elapsed,
                "batch_reads":chunks.iter().map(|v|v.0).sum::<usize>(),"batch_bytes":chunks.iter().map(|v|v.1).sum::<usize>(),
                "root":hex::encode(root)})
            );
        }
    }
    Ok(())
}

fn snapshot_regression() -> Result<()> {
    let network = [5; 32];
    let tree = Patricia::new(network.pack(), Namespace::Cells);
    let commit = Commitment::new(network, Namespace::Cells);
    let keys = [[0; 32], [1; 32], [2; 32], [0xff; 32]];
    let seed = [(keys[0], Some([11; 32])), (keys[1], Some([12; 32]))];
    let mut db = Snapshot::new();
    let mut roots = vec![tree.empty_root()];
    let batches = [
        seed.to_vec(),
        vec![(keys[0], None), (keys[2], Some([13; 32]))],
        vec![(keys[0], Some([14; 32])), (keys[1], None), (keys[2], None)],
    ];
    for change in batches {
        let mut edit = tree.edit(*roots.last().unwrap(), |k| Ok(db.get(k).cloned()));
        edit.set_batch(&change, |_, _, _| {})?;
        roots.push(edit.root());
        // The fixture retains old immutable roots. Reclamation belongs to the
        // storage editor, which this read-only candidate never modifies.
        for (k, bytes) in edit.into_writes() {
            if let Some(value) = bytes {
                db.insert(k, value.into());
            }
        }
    }
    for root in roots {
        let mut queries: Vec<_> = keys
            .into_iter()
            .map(|k| (k, None, Proof { nodes: vec![] }))
            .collect();
        walk(&commit, &db, root, &mut queries, 0)?;
        let mut oracle = tree.edit(root, |k| Ok(db.get(k).cloned()));
        for (key, value, proof) in queries {
            let (v, p) = oracle.prove(&key)?;
            ensure!(
                v == value
                    && p.encode()? == proof.encode()?
                    && tree.verify(&root, &key, v.as_ref(), &p),
                "retained root mismatch"
            );
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    snapshot_regression()?;
    for n in [1024, 65536, 1_000_000] {
        for prefix in [false, true] {
            probe(n, prefix)?;
        }
    }
    Ok(())
}
