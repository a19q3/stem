# Batch reads on an immutable authenticated Patricia tree

The example builds the native Cell tree at 1,024, 65,536 and 1,000,000 live keys. For each size it tests hashed keys and keys sharing a 64-bit prefix. The reference uses one ordinary proof traversal per query. The candidate sorts queries, walks the shared prefix once per worker partition, verifies every loaded node hash and restores caller order. Each slice has its own output paths. Proof encoding and the durable editor are the repository implementations.

The 108 cases span 128, 1,024 and 4,096 queries, presence and absence, repeated keys, prefix divergence and 1–32 Rayon workers. All returned values and encoded proof bytes matched the oracle. Additional checks cover the empty root and retained roots across insert, delete and reinsert. A read-only candidate has no write journal or reclamation behavior to validate.

For 1,024 queries on one million hashed keys, the reference read **21,431** immutable node records. The candidate read **11,321** with one partition and **11,353** with four. The shared-prefix workload dropped from **19,904** to **10,671** with four partitions. The example charges array preparation, query sorting, result scattering and worker dispatch to the candidate. The fixture used an in-memory map and four allowed CPUs, so 8–32 workers were oversubscribed. The warmed second traversal and single sample per cell make wall time order-sensitive. The archive includes individual read bytes, times, roots and process resource usage.

**Initial decision.** Test batched immutable reads on RocksDB. Keep subtree mutation and reclamation serial during the read experiment.

## RocksDB and native RPC

The native Store reads shared prefixes once per batch and emits a normal `PatriciaPath` for every input position. `Store::prove_cells` accepts up to 1,024 OutPoints. The `/stem/cells` proof query accepts 1–12 packed OutPoints against the current Header. Each returned row has the same Cell encoding and proof bytes as a standalone query. [Query format](../../OPERATIONS.md#wallet-and-proofs) · [Traversal](../../ledger/src/patricia.rs)

A flushed RocksDB snapshot with 16,384 Live Cells served 1,024 queries with present and absent IDs. The standalone traversals fetched 15,461 nodes. Shared-prefix traversal fetched 5,378, which is 65.2% fewer logical node reads. All paths matched byte for byte. The pinned view also returned the same proofs after the writer committed a deletion, and the current root authenticated the deleted Cell's absence.

Four native replicas exercised the query path after a two-Type-script spend. The response proved the new output's membership and returned the same absence proof for both occurrences of the spent input. The reopened-store test also compared batched and standalone results in caller order. The read-count reduction measures proof lookup. [Issue #4](https://github.com/a19q3/stem/issues/4)

### Batched reads for shared-prefix proofs

`native-batched-patricia-proofs` adds a traversal for immutable native
Cell proof batches. It walks the same shared prefixes and groups
requested node IDs from each tree frontier in chunks of up to 64.
The Store resolves branch entries and resident node hits before
sending remaining requests to RocksDB with the same read options.
Four or more disk lookups use one MultiGet call. Smaller sets use
point reads.

Each returned node is authenticated against its requested ID and tree.
Each emitted record is checked against the leaf value. Membership,
absence, duplicate requests and caller order retain the same
`PatriciaPath` bytes. The compile-checked fixtures compare full
paths across grouped reads and cover a pinned RocksDB view after
its newer tip commits. The archived JMT proof path has its own reader.

Compare the existing shared-prefix traversal against this feature at
widths 1, 12, 32, 128 and 1,024 on identical committed states.
Use uniform and clustered keys with resident nodes, warm RocksDB and
cold RocksDB. Record storage calls, keys per call, proof wall time, CPU,
allocations and exact output equality. This traversal consumes fetched
nodes directly. The [earlier two-pass edit experiment](#bounded-rocksdb-prefetch-before-editing)
found a regression even with fewer storage calls. Fully resident or
narrow proof batches may also see little benefit from the additional
buffering. Runtime and replay evidence are pending.

### Grouping the Live Cell records behind proofs

`native-batched-cell-proof-records` groups the actual Live Cell lookup
after proof traversal. The requested OutPoints keep their original
positions. Each unique OutPoint uses the same branch, resident and
decoded-cache precedence as a single lookup. Remaining committed
records use point reads or MultiGet in groups of up to eight. The
Store reuses the key from proof traversal and checks the record hash
once per OutPoint. Duplicate queries still return separate proof
paths, and a missing leaf still requires checking record absence.

Committed records from disk enter the decoded cache after proof
binding succeeds. The feature supports both the ordinary and owned
RocksDB record decoders. The compile-checked fixtures cover duplicate
positions, a disk row present behind an absence proof, and batch
proofs from a pinned view after the next block commits.

Compare with and without the option at the same committed root,
using uniform and repeated or clustered OutPoints. Hold node
batching and the owned record decoder constant for the comparison.
Record live-record point and batch calls, duplicate share, lookup
CPU, RSS and total response time and verify exact output bytes.
Small requests or resident state may gain little. MultiGet holds
multiple raw values until each group is decoded, while a group never
exceeds eight unique records. Runtime and replay evidence are pending.

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

Both configurations missed the sustained rate, acknowledgment, backlog and tail-latency gates. The candidate's mean Cell commitment fell on both Leader and Follower paths. Selection accounted for 65.6% of Leader proposal wall time in the candidate, compared with 53.4% in the reference.

SHA-256 values were `b55d73ce78c3ba11fd7ed74174e84edb338695c5acb46479ca989677c2bebc18` for the reference, `9247273b15680b172ae43dc4764670915adc116c525eff53984a2c5111776732` for the candidate, `8769948cc9ffcf7cbd96997e75bcfd3a6794442c01ea6770792be786451a3846` for the driver and replay harness, and `2fb0207c35b3416b36fde01361966c2bc09dbbdfb3806061fd1e7004558d4cbe` for genesis. The signed workload BLAKE2b digest was `5ecfc85a430edd167c04f6b568235785ddf6fb61d5dad6b89fec3328f1985939`.

#### Reverse-order subtree comparison

The repeat ran parallel subtrees then serial, with identical executable, genesis and signed-workload digests and fresh databases. The loopback ports changed. Hardware affinity, worker count, cache and profile settings matched. Both wrote the entire corpus, and another 635 archived blocks independently replayed.

| Metric | Parallel Cell subtrees | Serial |
| --- | ---: | ---: |
| Common measurement TPS | 2,241.42 | 1,877.33 |
| Measurement writes without ACK | 102,322 | 176,780 |
| Peak acknowledged backlog | 90,143 | 95,190 |
| Backlog growth from a measurement boundary | 38,905 | 27,023 |
| Acknowledged payments pending at drain | 0 | 11,637 |
| Conditional p99 | 191.236 s | 197.935 s |
| Mean Leader Cell commitment | 95.77 ms | 198.14 ms |
| Mean Follower Cell commitment | 109.39 ms | 213.53 ms |
| Total validator process CPU | 1,327.90 s | 1,159.30 s |

The parallel candidate confirmed 19.4% more payments in the reverse run and drained all 617,678 acknowledgments. The p99 populations were 437,733 and 355,135. Peak RSS ranged from 328.9 to 332.0 MiB per validator for the candidate and 320.8 to 329.6 MiB for the reference. Both runs recorded zero local timeout votes. All four runs across both pairs failed the sustained profile. The repeated gain supports evaluating the optional candidate with the larger verification cache. Worker contention and the backlog and tail-latency gates remain separate adoption criteria.

### Cell subtree and verification cache matrix

Reuse the preceding node builds and `prepare-v1 --profile sustainable` funding and measurement settings. For each run, set `verification_cache_entries` in every validator's node configuration to the tested capacity. Keep the prepared topology and signed workload fixed. All runs use 720,000 independent payments, two VM workers per validator, 4B cycles and 2,048 transactions per block. Four loopback validators shared the same Ryzen 9 7950X3D as the pinned driver. Fresh databases and the same retention settings were used for each run. The run order was serial with 65,536 entries, parallel with 65,536, parallel with 4,096, then serial with 4,096. The first small-cache parallel attempt encountered an I/O error before the offer. The retry used the same prepared inputs and new stores.

Compare common observations in the 180 seconds after warmup. The p99 population consists of payments written in those 180 seconds with a successful acknowledgment and common visibility by drain. Hash-band comparisons use the first byte of the signed-transaction BLAKE2b digest, which is the leading byte of the proposer's ordering key. Total validator process CPU includes funding, measurement, drain and archive capture.

| Run order | Cache entries | Common measurement TPS | Measurement writes without ACK | Peak acknowledged backlog | Backlog growth | Pending at drain | p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Serial | 65,536 | 2,173.16 | 109,218 | 85,662 | 40,311 | 0 | 194.075 s |
| Parallel subtrees | 65,536 | 2,911.69 | 0 | 30,588 | 15,850 | 0 | 75.338 s |
| Parallel subtrees | 4,096 | 1,979.73 | 144,277 | 92,361 | 43,429 | 0 | 195.822 s |
| Serial | 4,096 | 1,843.20 | 185,220 | 98,330 | 28,185 | 9,059 | 198.460 s |

All four runs wrote 720,000 payments. Independent replay authenticated 1,564 observed blocks and the terminal finality proofs. Acknowledgments totaled 610,782, 720,000, 575,723 and 534,780 in run order. The corresponding p99 populations were 430,782, 539,980, 395,723 and 348,534. Each configuration missed the sustained throughput, peak backlog, backlog growth and latency gates. The joint configuration also passed the write and acknowledgment gates and drained all 720,000 payments. Its minute counts were 173,896, 172,032 and 178,176 common confirmations.

Parallel subtrees improved throughput by 34.0% at 65,536 entries and 7.4% at 4,096 entries. The joint configuration was 58.0% faster than the serial small-cache control. Cache hits accounted for 29.36%, 38.55%, 13.91% and 12.71% of verification lookups in run order. Every cache reached capacity, with zero wrong-parent misses. At 65,536 entries the observed mean Leader Cell commitment moved from 188.82 to 64.36 ms and the Follower mean from 202.81 to 72.52 ms. Total CPU rose from 1,214.30 to 1,357.52 process seconds. These means cover blocks produced by each live run, with block sizes and counts varying. One run per configuration leaves the interaction magnitude and run-order effects open.

For the 430,782 measurement payments acknowledged and confirmed in both large-cache runs, p99 fell from 194.075 to 73.855 seconds. In the joint configuration the lowest signed-hash band had a median of 3.009 seconds and p99 of 5.707 seconds. The highest band had a median of 12.887 seconds and p99 of 154.943 seconds. Compare that gap under the existing bounded age-priority lane. Keep both optimizations selectable pending replication and an acceptable sustained tail. [Age-priority control](../storage/admission-order/README.md#bounded-priority-for-aged-payments)

The node, harness and genesis SHA-256 values and signed corpus BLAKE2b digest match the [earlier subtree comparisons](#single-host-sustained-payment-comparison). Replay each report with the standard `drive.py verify` command against the prepared trial. The same qualifier bounds apply to each run. [Driver and replay](../storage/external/README.md#measure-and-replay) · [Acceptance bounds](../storage/external/qualification.md#sustainable-load-and-overload)

### Mixed Type Group ablation and default

On the `6327b9b` node source, four loopback validators ran serial, parallel, parallel and serial Cell commitments on fresh stores. The feature was the only build difference. Each run used two VM workers, a 65,536-entry verification cache, a 100 ms proposal interval, 2,048 items and 4 billion cycles. The externally paced driver offered 55,000 independent payments at 2,200/s for 25 seconds, then drained for 20 seconds. The workload cycled through zero to twelve Type Groups. Every run acknowledged and confirmed the complete signed corpus. Independent replay authenticated 927 observed blocks, twelve sampled receipts and the four terminal certificates.

| Run | Common TPS | p99 | Mean Leader Cell phase | Mean Follower Cell phase | Validator CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| Serial first | 1,991.04 | 2.618 s | 8.727 ms | 9.255 ms | 96.57 s |
| Parallel first | 2,094.76 | 2.412 s | 5.295 ms | 5.713 ms | 98.01 s |
| Parallel second | 2,042.72 | 2.374 s | 5.099 ms | 5.568 ms | 96.80 s |
| Serial second | 2,097.44 | 3.996 s | 8.724 ms | 9.433 ms | 95.97 s |

Both pairings lowered Leader and Follower Cell phase time by approximately 38–42% and lowered p99 on the same 55,000 payments. Window throughput changed by +5.2% and −2.6%. Process CPU rose by 0.9–1.5%. There were zero cycle rejections in the first pair. The second pair saw 701 rejections with parallel commitments and 2,693 with serial commitments. The second pair's end-to-end latency includes that resource pressure. The comparison covers one same-host workload with one Lock and varying Type Groups.

Enable parallel Cell subtree mutation in the default build based on the repeated Cell phase savings, the payment comparisons in both run orders, and the matched root, ordered batch, receipt and reopened-proof checks. Keep the existing serial fallback for smaller batches and a single worker. Build the serial reference with `cargo build --release --locked --no-default-features --bin stem`. This selection is independent of subsequent throughput qualification.

For reproduction, use `drive.py prepare --payment-type-groups 12 --rate 2200 --seconds 25 --drain 20 --senders 64 --block-transactions 2048 --block-cycles 4000000000 --ready-senders --reuse-submissions --complete-volume` with a pinned four-replica topology, then run the same prepared trial and the existing independent verifier for every arm. The workload identifier was `85f80e6e8a2448944da69b7faaa3e662d6e8506c12749a0dc2ed4aa1ba7f652e`. The serial node, parallel node and measuring harness SHA-256 values were `ab7b9b6f117bb3771f74d1030b48019eeaf51a6c11755bca34df1cf902689423`, `105ce8fb9af2781efd897898bc95362efe1b6e0042e061bddca3940d15cde8c6` and `09908d1f00bf406952ea21968793c80d898ddf6d4a0d65926e6df04aad99ded0`. [Driver and replay](../storage/external/README.md#measure-and-replay)

## Bounded RocksDB prefetch before editing

The probe at `5c697c2` compared direct `get_opt` calls with two RocksDB prefetch frontiers. The frontier walked compressed prefixes on a pinned snapshot, issuing at most 256 reads at once with `multi_get_opt` or `batched_multi_get_cf_opt`. It passed the fetched canonical bytes to the existing authenticated editor. The comparison charges planning, buffering, canonical revalidation, edit and prefetch-buffer release. The probe uses compact nodes on one pinned core, a 128 MiB write buffer and no speculative ancestry. Twelve warm samples per configuration follow three warmup rounds in rotating method order.

| Live leaves | Removed and inserted, each | Storage | Logical reads | Prefetch calls | Point edit | MultiGet edit | Pinned batch edit |
| ---: | ---: | --- | ---: | ---: | ---: | ---: | ---: |
| 8,192 | 512 | Memtable | 4,120 | 30 | 3.746 ms | 4.427 ms | 4.627 ms |
| 8,192 | 512 | Warm SST | 4,120 | 30 | 5.074 ms | 5.518 ms | 5.155 ms |
| 262,144 | 2,048 | Memtable | 28,474 | 127 | 44.033 ms | 53.045 ms | 55.043 ms |
| 262,144 | 2,048 | Warm SST | 28,474 | 127 | 63.073 ms | 71.987 ms | 68.528 ms |

Each of the 144 measured edits matched the canonical root and exact node writes and deletes. Both prefetch methods read the same key set as the direct editor. Fewer RocksDB calls still led to longer total edits in every fixture. Retain point reads. The result covers the two-pass prefetch and edit design. An editor that consumes fetched nodes directly would need a separate comparison.

The experiment retained its patch and raw samples outside the repository. The executable SHA-256 is `637ce66750643cbf7987ef4c27f65f3ad4a4886295c3df532fde3ff2d4d7ac80` and the sample JSON SHA-256 is `07a8ba85f481b5f3d62158c1136608d5a189dfc10ead495d2cfef02cad746860`.

## Editing directly from batched node reads

`native-patricia-frontier-edit` gives large parallel Cell updates a
bounded batch of authenticated upper-branch reads. Requests use the
same branch, resident and committed-snapshot lookup path as the
batched proof reader. The editor retains each checked node and its
change slice, then uses those nodes to settle the planned branches.
It passes already-checked subtree roots to the ordinary editor at
terminal pieces. This skips the second lookup and authentication of
planned nodes from the earlier prefetch experiment.

The experiment retains up to 511 pieces and issues at most 64 node
IDs per read. A branch with fewer than eight edits in either child,
or a diverging compressed prefix, uses the existing editor below
that point. The original threshold still governs parallel joins.
Each worker produces a separate journal, with left work joined
before right and the parent. The serial callback order for touched
leaves is preserved. The compiled differential fixture checks every
node edit and callback, and rejects repeated reads of planned nodes.

Compare the option over `native-verified-cell-journal` with the same
worker count, caches, node journal and authenticated state. Use
payments, clustered mixed contracts and skewed edit sets at fixed
block sizes. Record call count, node reads, tree CPU and wall time,
allocations, committed batch equality and reopened proofs. Planning
the upper levels serially can delay the first child work, and the
frontier adds buffered nodes even with full residency. Runtime and
independent replay are pending.

## Original in-memory experiment

```sh
CXXFLAGS='-include cstdint' cargo build --locked --release --example research_patricia
taskset -c 0-3 target/release/examples/research_patricia
```
