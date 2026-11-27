# Batch reads on an immutable authenticated Patricia tree

The example builds the native Cell tree at 1,024, 65,536 and 1,000,000 live keys. For each size it tests hashed keys and keys sharing a 64-bit prefix. The reference uses one ordinary proof traversal per query. The candidate sorts queries, walks the shared prefix once per worker partition, verifies every loaded node hash and restores caller order. Each slice has its own output paths. Proof encoding and the durable editor are the repository implementations.

The 108 cases span 128, 1,024 and 4,096 queries, presence and absence, repeated keys, prefix divergence and 1–32 Rayon workers. All returned values and encoded proof bytes matched the oracle. Additional checks cover the empty root and retained roots across insert, delete and reinsert. A read-only candidate has no write journal or reclamation behavior to validate.

For 1,024 queries on one million hashed keys, the reference read **21,431** immutable node records. The candidate read **11,321** with one partition and **11,353** with four. The shared-prefix workload dropped from **19,904** to **10,671** with four partitions. The example charges array preparation, query sorting, result scattering and worker dispatch to the candidate. The fixture used an in-memory map and four allowed CPUs, so 8–32 workers were oversubscribed. The warmed second traversal and single sample per cell make wall time order-sensitive. The archive includes individual read bytes, times, roots and process resource usage.

**Initial decision.** Test batched immutable reads on RocksDB. Keep subtree mutation and reclamation serial during the read experiment.

## RocksDB and native RPC

The native Store reads shared prefixes once per batch and emits a normal `PatriciaPath` for every input position. `Store::prove_cells` accepts up to 1,024 OutPoints. The `/stem/cells` proof query accepts 1–12 packed OutPoints against the current Header. Each returned row has the same Cell encoding and proof bytes as a standalone query. [Query format](../../OPERATIONS.md#wallet-and-proofs) · [Traversal](../../ledger/src/patricia.rs)

A flushed RocksDB snapshot with 16,384 Live Cells served 1,024 queries with present and absent IDs. The standalone traversals fetched 15,461 nodes. Shared-prefix traversal fetched 5,378, which is 65.2% fewer logical node reads. All paths matched byte for byte. The pinned view also returned the same proofs after the writer committed a deletion, and the current root authenticated the deleted Cell's absence.

Four native replicas exercised the query path after a two-Type-script spend. The response proved the new output's membership and returned the same absence proof for both occurrences of the spent input. The reopened-store test also compared batched and standalone results in caller order. The read-count reduction measures proof lookup. [Issue #4](https://github.com/a19q3/stem/issues/4)

## Parallel Cell subtree mutation

The optional `native-cell-subtree-parallel` feature gives Cell commitment work to the existing Rayon pool used by the Leader and Follower. A partition can run concurrently when every change fits the existing compressed prefix and each child has at least 128 changes. The two children receive disjoint key ranges and bounded shares of the available workers. Each child reads the same immutable parent view and buffers node edits and previous values. The parent joins left before right, then applies the existing compression rule. Small or one-sided batches and changes extending an existing prefix use the serial editor. The System tree, node encoding, durable batch ordering and state proof format use the existing paths.

The differential test compares exact roots, previous-value order, all node writes and deletions, membership and absence proof bytes, and retained-node counts. It includes shared prefixes, inserts, no-ops, deletion of a complete subtree and reconstruction from an empty root. The native signed test compares receipts and roots on speculative parents and checks proofs after reopening RocksDB. The ordered-batch test also prepares against an old pinned RocksDB snapshot after the certified writer has persisted and reclaimed nodes in the current view.

The in-memory timing matrix used hashed keys and evenly interleaved deletions and insertions. All 54 combinations matched the serial root, writes, logical node reads and canonical bytes read. Each median uses six measured trials after warmup, with the worker order rotated. The aggregate process used 4.48 CPU seconds and 502 MiB peak RSS including the one-million-key fixture. These are immutable BTreeMap reads.

| Live keys | Changes | 1 worker | 2 workers | 4 workers | 8 workers | Logical nodes read |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1,024 | 512 | 0.744 ms | 0.471 ms | 0.632 ms | 0.640 ms | 1,165 |
| 1,024 | 2,048 | 1.381 ms | 0.993 ms | 0.750 ms | 0.494 ms | 2,047 |
| 65,536 | 512 | 3.439 ms | 1.925 ms | 1.792 ms | 1.656 ms | 4,037 |
| 65,536 | 2,048 | 10.344 ms | 7.147 ms | 4.627 ms | 3.791 ms | 12,291 |
| 1,000,000 | 512 | 5.314 ms | 3.728 ms | 3.316 ms | 3.146 ms | 6,096 |
| 1,000,000 | 2,048 | 24.746 ms | 12.902 ms | 8.362 ms | 5.834 ms | 20,220 |

The 64-change cases stayed serial. At one million keys and 2,048 changes the 16- and 32-worker pools took 6.181 and 7.151 ms. Every worker setting read 1,922,622 canonical node bytes at that size. Larger pools and these local timings need separate validation against the replicated RocksDB workload.

```sh
cargo test --release --locked -p stem-ledger --features parallel-patricia \
  parallel_mutations_preserve_reclamation_and_proofs
cargo test --release --locked -p stem-ledger --features parallel-patricia \
  measure_immutable_mutations_by_size_and_workers -- --ignored --nocapture
cargo test --release --locked --features native-cell-subtree-parallel,jolteon-commit-pipeline \
  namespace_commitments_keep_the_ordered_write_batch
cargo test --release --locked --features native-cell-subtree-parallel \
  parallel_commitments_match_serial_roots_and_reopened_proofs
```

Two serial and two subtree-feature replicas confirmed all 1,024 signed payments through a restart and authenticated common heights 8 and 9. Using only the subtree feature, the CKB 0.210.0 stake fixture accepted the same Checkpoint format at heights 10, 20 and 24. The joining key voted at height 11 and retired at height 21. These are compatibility and recovery controls. [Mixed runner](../../tools/jolteon_localnet.py) · [CKB reproduction](../../OPERATIONS.md#discovery-mode)

### Single-host sustained payment comparison

Use `drive.py prepare-v1 --profile sustainable` and the same signed 720,000-payment corpus for each run. Toggle only `native-cell-subtree-parallel`. Both runs use two VM workers per validator, the 4,096-entry verification cache, fresh databases, and the established four-loopback-replica placement on the 7950X3D. The offer is 3,000/s with 60 seconds of warmup, 180 seconds of measurement and 45 seconds of drain. The full score, process resources, and `proposal_commitments.cells` and `candidate_commitments.cells` deltas distinguish throughput, queueing, and Cell work. [Measurement and replay](../storage/external/README.md#measure-and-replay) · [Profile acceptance](../storage/external/qualification.md#sustainable-load-and-overload)

| Metric | Serial | Parallel Cell subtrees |
| --- | ---: | ---: |
| Common measurement TPS | 1,911.47 | 2,218.67 |
| Measurement writes without ACK | 175,390 | 106,216 |
| Peak acknowledged backlog | 96,302 | 89,406 |
| Backlog growth from a measurement boundary | 24,394 | 35,510 |
| Acknowledged payments pending at drain | 15,284 | 0 |
| Conditional p99 | 197.051 s | 192.313 s |
| Mean Leader Cell commitment | 198.95 ms | 102.94 ms |
| Mean Follower Cell commitment | 211.90 ms | 120.55 ms |
| Mean full proposal | 433.73 ms | 307.33 ms |
| Mean full Follower preparation | 465.62 ms | 349.59 ms |
| Total validator process CPU | 1,157.04 s | 1,321.91 s |

Both runs wrote the entire corpus and independently replayed a total of 615 archived blocks. The p99 populations were 354,090 and 433,784 acknowledged measurement writes commonly visible by the cutoff. The candidate drained all 613,784 acknowledged payments. It commonly confirmed 16.1% more payments in the measurement window and used 14.2% more validator process CPU. Peak RSS was 323.6–333.7 MiB per validator in the reference and 332.1–338.9 MiB in the candidate. The candidate recorded four local timeout votes versus zero in the reference. Both recorded zero block-cycle rejections.

Both configurations missed the sustained rate, acknowledgment, backlog and tail-latency gates. The candidate's mean Cell commitment fell on both Leader and Follower paths. Selection then accounted for 65.6% of Leader proposal wall time in the candidate, compared with 53.4% in the reference. The single run order establishes a candidate for repeat testing with the larger verification cache and other stages. Keep the feature selectable while checking reverse order, tail latency and worker contention.

SHA-256 values were `b55d73ce78c3ba11fd7ed74174e84edb338695c5acb46479ca989677c2bebc18` for the reference, `9247273b15680b172ae43dc4764670915adc116c525eff53984a2c5111776732` for the candidate, `8769948cc9ffcf7cbd96997e75bcfd3a6794442c01ea6770792be786451a3846` for the driver and replay harness, and `2fb0207c35b3416b36fde01361966c2bc09dbbdfb3806061fd1e7004558d4cbe` for genesis. The signed workload BLAKE2b digest was `5ecfc85a430edd167c04f6b568235785ddf6fb61d5dad6b89fec3328f1985939`.

## Original in-memory experiment

```sh
CXXFLAGS='-include cstdint' cargo build --locked --release --example research_patricia
taskset -c 0-3 target/release/examples/research_patricia
```
