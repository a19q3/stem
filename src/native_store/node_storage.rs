//! The local RocksDB representation of canonical Patricia nodes.

use anyhow::{Result, bail, ensure};

const PREFIX_BYTES: usize = 32;
const CHILD_BYTES: usize = 64;
const PREFIX_START: usize = 2;
const CHILD_START: usize = PREFIX_START + PREFIX_BYTES;
const BRANCH_BYTES: usize = CHILD_START + CHILD_BYTES;
const LEAF_BYTES: usize = 1 + CHILD_BYTES;

pub(super) fn compact(mut node: Vec<u8>) -> Vec<u8> {
    // The Patricia editor produces canonical bytes. All omitted prefix
    // bytes are zero, including when the prefix ends at bit zero.
    if let [1, bits, ..] = node.as_slice() {
        let end = PREFIX_START + usize::from(*bits).div_ceil(8);
        node.copy_within(CHILD_START.., end);
        node.truncate(end + CHILD_BYTES);
    }
    node
}

pub(super) struct CanonicalNode {
    bytes: [u8; BRANCH_BYTES],
    len: usize,
}

impl AsRef<[u8]> for CanonicalNode {
    fn as_ref(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

pub(super) fn canonical(stored: &[u8]) -> Result<CanonicalNode> {
    let mut bytes = [0; BRANCH_BYTES];
    let len = match stored {
        [0, ..] if stored.len() == LEAF_BYTES => {
            bytes[..LEAF_BYTES].copy_from_slice(stored);
            LEAF_BYTES
        }
        [1, bits, ..] => {
            let end = PREFIX_START + usize::from(*bits).div_ceil(8);
            ensure!(
                stored.len() == end + CHILD_BYTES,
                "invalid stored Patricia branch length"
            );
            bytes[..end].copy_from_slice(&stored[..end]);
            bytes[CHILD_START..].copy_from_slice(&stored[end..]);
            BRANCH_BYTES
        }
        _ => bail!("invalid stored Patricia node"),
    };
    Ok(CanonicalNode { bytes, len })
}

#[cfg(test)]
mod tests {
    use std::{hint::black_box, time::Instant};

    use ckb_db::internal::{DB, Options, WriteBatch, WriteOptions, prelude::*};
    use ckb_types::prelude::*;
    use serde_json::json;
    use stem_ledger::patricia::{Namespace, Patricia};
    use stem_stake::patricia::{Commitment, Key, Node, canonical_prefix};

    use super::*;

    #[test]
    fn roundtrip_every_branch_prefix_and_reject_wrong_lengths() -> Result<()> {
        for bits in 0..=u8::MAX {
            let node = Node::Branch {
                prefix: canonical_prefix(&[0xff; 32], bits),
                bits,
                children: [[1; 32], [2; 32]],
            };
            let bytes = node.encode();
            let stored = compact(bytes.clone());
            assert_eq!(stored.len(), 66 + usize::from(bits).div_ceil(8));
            assert_eq!(canonical(&stored)?.as_ref(), bytes);
            assert!(canonical(&stored[..stored.len() - 1]).is_err());
            assert!(canonical(&[stored.as_slice(), &[0]].concat()).is_err());
        }
        let leaf = Node::Leaf {
            key: [3; 32],
            value: [4; 32],
        }
        .encode();
        assert_eq!(compact(leaf.clone()), leaf);
        assert_eq!(canonical(&leaf)?.as_ref(), leaf);
        assert!(canonical(&leaf[..leaf.len() - 1]).is_err());
        Ok(())
    }

    #[test]
    #[ignore = "paired warm RocksDB Patricia read, edit and batch measurements"]
    fn measure_patricia_node_storage() -> Result<()> {
        const LIVE: u32 = 8192;
        const CHURN: u32 = 512;
        let network = [6; 32];
        let tree = Patricia::new(network.pack(), Namespace::Cells);
        let commitment = Commitment::new(network, Namespace::Cells);
        let key = |i: u32| tree.record_key(&i.to_le_bytes());
        let value = |i: u32| Some(tree.value_hash(&i.to_le_bytes()));
        let mut initial = (0..LIVE).map(|i| (key(i), value(i))).collect::<Vec<_>>();
        initial.sort_unstable_by_key(|(key, _)| *key);
        let mut seed = tree.edit(tree.empty_root(), |_| Ok(None::<Vec<u8>>));
        seed.set_batch(&initial, |_, _, _| {})?;
        let root = seed.root();
        let nodes = seed.into_writes();
        let mut changes = (0..CHURN)
            .map(|i| (key(i), None))
            .chain((LIVE..LIVE + CHURN).map(|i| (key(i), value(i))))
            .collect::<Vec<_>>();
        changes.sort_unstable_by_key(|(key, _)| *key);
        let directory = tempfile::tempdir()?;
        let mut options = Options::default();
        options.create_if_missing(true);
        let canonical_db = DB::open(&options, directory.path().join("canonical"))?;
        let compact_db = DB::open(&options, directory.path().join("compact"))?;
        let value_bytes = |pack: bool, node: Vec<u8>| if pack { compact(node) } else { node };
        for (db, pack) in [(&canonical_db, false), (&compact_db, true)] {
            let mut batch = WriteBatch::default();
            for (id, node) in &nodes {
                batch.put(id, &value_bytes(pack, node.clone().expect("new node")))?;
            }
            db.write_opt(&batch, &WriteOptions::default())?;
        }
        let mut reference = tree.edit(root, |id| Ok(nodes.get(id).cloned().flatten()));
        reference.set_batch(&changes, |_, _, _| {})?;
        let expected_root = reference.root();
        let expected_writes = reference.into_writes();

        let mut samples = Vec::new();
        for repetition in 0..12 {
            let order = if repetition % 2 == 0 {
                [false, true]
            } else {
                [true, false]
            };
            for pack in order {
                let db = if pack { &compact_db } else { &canonical_db };
                let (mut fetch_ns, mut decode_ns, mut read_bytes, mut reads) = (0, 0, 0, 0);
                let mut editor = tree.edit(root, |id: &Key| {
                    let start = Instant::now();
                    let raw = db.get(id)?.expect("seeded node");
                    fetch_ns += start.elapsed().as_nanos();
                    reads += 1;
                    read_bytes += raw.len();
                    let start = Instant::now();
                    let restored = if pack {
                        canonical(&raw)?
                    } else {
                        let mut bytes = [0; BRANCH_BYTES];
                        bytes[..raw.len()].copy_from_slice(&raw);
                        CanonicalNode {
                            bytes,
                            len: raw.len(),
                        }
                    };
                    decode_ns += start.elapsed().as_nanos();
                    Ok(Some(restored))
                });
                let start = Instant::now();
                editor.set_batch(&changes, |_, _, _| {})?;
                let edit_ns = start.elapsed().as_nanos();
                assert_eq!(editor.root(), expected_root);
                let writes = editor.into_writes();
                assert_eq!(writes, expected_writes);
                let start = Instant::now();
                for node in writes.values().flatten() {
                    black_box(commitment.node_hash(black_box(node)));
                }
                let rehash_ns = start.elapsed().as_nanos();
                let start = Instant::now();
                let mut batch = WriteBatch::default();
                for (id, node) in writes {
                    if let Some(node) = node {
                        batch.put(&id, &value_bytes(pack, node))?;
                    } else {
                        batch.delete(&id)?;
                    }
                }
                let batch_ns = start.elapsed().as_nanos();
                if repetition != 0 {
                    samples.push(json!({
                        "compact": pack, "read_count": reads, "read_bytes": read_bytes,
                        "read_ns": fetch_ns, "restore_ns": decode_ns, "edit_ns": edit_ns,
                        "rehash_written_nodes_ns": rehash_ns, "batch_ns": batch_ns,
                        "node_batch_bytes": batch.size_in_bytes(),
                    }));
                }
            }
        }
        let evidence = json!({
            "seed_cells": LIVE, "removed": CHURN, "inserted": CHURN,
            "root": hex::encode(expected_root), "samples": samples,
        });
        if let Some(path) = std::env::var_os("STEM_PATRICIA_REPORT") {
            std::fs::write(path, serde_json::to_vec(&evidence)?)?;
        }
        println!("{evidence}");
        Ok(())
    }
}
