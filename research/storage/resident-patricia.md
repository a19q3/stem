# Resident Cell Patricia nodes

A Stem Cell root refers to immutable nodes by canonical hash. The native
opt-in node index retains one decoded copy of each durable Cell node.
Candidate writes take precedence and unchanged nodes can be shared by
different roots. Reads that miss the index use the same RocksDB snapshot
as before. Readers pinned to an earlier root still have their durable
snapshot after superseded nodes leave the index.

The index checks durable node hashes before activation. It applies the
existing ordered node edits only after acknowledgement of the atomic
certified WAL write. It uses a separate memory limit and shard-local
read locks so readers on different shards can proceed during publication.
Allocation checks include a shard's possible replacement table.
Exhaustion retires the index and keeps the ordinary authenticated reader
available. The node encoding, commitments, proof bytes, WAL transaction and
CKB settlement stay canonical.

## Churn and storage

The standalone prototype reopens the same compact Patricia seed from the
[durable state comparison](authenticated-state-comparison.md), with
1,440,002 live Cells and 128 batches of 2,048 spends and 2,048 creations.
All arms use identical RocksDB persistence, two workers and the same
physical CPU affinity. The PPS totals count replacements within mutation
and synchronous-write intervals after the first eight batches. The
standalone resident arm updates a decoded hash table on publication.
The production index shards the table for concurrent readers, so the
standalone timings measure the data path before that integration.

| Mode | Durable replacements/s | Tree edit | Index publication | WAL sync | Peak RSS |
| --- | ---: | ---: | ---: | ---: | ---: |
| RocksDB 256 MiB, first | 14,751 | 87.39 ms | <0.01 ms | 33.45 ms | 670 MiB |
| Resident nodes, first | 27,198 | 22.57 ms | 10.77 ms | 30.08 ms | 1,179 MiB |
| RocksDB 836 MiB, first | 15,858 | 78.83 ms | <0.01 ms | 32.55 ms | 1,213 MiB |
| RocksDB 836 MiB, second | 15,899 | 79.70 ms | <0.01 ms | 31.46 ms | 1,210 MiB |
| Resident nodes, second | 23,325 | 22.89 ms | 11.26 ms | 31.52 ms | 1,175 MiB |
| RocksDB 256 MiB, second | 14,690 | 88.13 ms | <0.01 ms | 31.93 ms | 671 MiB |

The resident prototype held 2,880,003 verified nodes in a 580 MiB hash
table under a 768 MiB allowance. Its measured peak RSS stayed below that
of the larger RocksDB-cache control. Both resident runs resolved all
39,607 median node requests per edit from memory. The tree stage took
around 23 ms under either run order. The memory publication stage took
11 ms. Replacement rates improved by 47–72% against the higher-memory
RocksDB control, including synchronous writes. Process CPU was around
23.3 s for either resident arm, versus 33.8–33.9 s for the larger cache.

All six runs wrote identical WAL bytes and recovered the same Cell
records and exact canonical root. Live SST size was identical after
compaction. Durable state continued to reference the same canonical
Patricia nodes. The prototype was killed before issuing its write at
version 304 and after that write. It recovered expected versions 303 and
304 and rebuilt the memory index on the next write. Pinned and current
proofs were checked during churn and after reopen. The pressure test
below exercises the native different-shard locks and consensus path.

## Native four-replica comparison

Build the current default features and run the four loopback replicas on
logical CPUs `2–7,10–15,18–23,26–31`, with the driver on `0–1,16–17`.
Keep two VM workers, 131,072 verification entries, a 100 ms cadence,
a 2.4 s view timeout, four billion cycles, 2,048 transactions per block,
and a 256 MiB RocksDB cache. Use the signed payment corpus and authenticated
1,440,002-Cell funded image from the
[Live Cell qualification](live-cell-index.md). Offer 4,000 transactions/s
over 60 seconds. The score uses the final 40 seconds, with 35 seconds of
drain. Use an archive and three replicas retaining 64 blocks. The external
driver independently verifies each archive and terminal certificate.

Toggle `--live-cell-index-mib 1536` and `--cell-node-index-mib 768`
independently. Run the four configurations in forward and reverse order.
The observed block sizes can differ by arm. Use the fixed churn comparison
for equal-size tree costs. The p99 population includes acknowledged
measurement writes commonly confirmed by the end of drain.

| Run | Common TPS | Conditional p99 | Common backlog at window start → end | Follower preparation | Certified commit | CPU | Peak RSS per replica |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| RocksDB 1 | 2,355 | 68.38 s | 69,144 → 98,359 | 461.2 ms | 54.1 ms | 224.5 s | 710–735 MiB |
| Both indexes 1 | 3,994 | 1.31 s | 2,859 → 3,091 | 78.1 ms | 41.0 ms | 187.0 s | 1434–1489 MiB |
| Patricia node index 1 | 3,974 | 3.67 s | 8,974 → 9,727 | 133.2 ms | 64.5 ms | 221.7 s | 1145–1185 MiB |
| Live Cell index 1 | 3,738 | 59.43 s | 65,270 → 75,766 | 280.2 ms | 97.8 ms | 231.4 s | 1208–1248 MiB |
| Live Cell index 2 | 3,738 | 56.15 s | 63,390 → 73,913 | 281.3 ms | 68.9 ms | 233.5 s | 1222–1242 MiB |
| Patricia node index 2 | 3,866 | 9.28 s | 3,305 → 8,470 | 163.7 ms | 77.7 ms | 220.2 s | 1146–1162 MiB |
| Both indexes 2 | 4,013 | 2.41 s | 2,591 → 2,161 | 89.4 ms | 56.6 ms | 189.3 s | 1473–1558 MiB |
| RocksDB 2 | 3,174 | 65.53 s | 60,911 → 87,648 | 329.0 ms | 57.3 ms | 246.2 s | 713–757 MiB |

Both combined runs acknowledged and confirmed all 240,000 payments.
The two RocksDB controls acknowledged and finalized 203,392 and
233,691. Each admitted payment in all eight runs was confirmed by the
drain cutoff. The p99 population in the two controls was 123,392 and
153,691. Both combined p99 populations contained all 160,000 measurement
writes. The common TPS includes confirmation of some warmup traffic.
The 4,013 result therefore describes the window's score under a
4,000/s offer.

The node index on its own sharply reduced backlog and p99 in both
orders. The additional Live Cell index brought p99 to 1.31–2.41 s and
lowered aggregate CPU from 220–222 s to 187–189 s in the comparable
40-second windows. The combined runs resolved approximately
4.0 million Cell node lookups per replica through memory during
startup and load, with zero fallbacks in the first combined sample.
Every combined replica remained indexed.

The typical combined block held 913–1,023 payments, compared with
2,048 in the saturated controls. Its smaller Leader and Follower times
reflect both faster work and smaller batches. The fixed-size churn
test isolates the effect on equal-sized edits. Certified commit time
includes durable persistence, index publication and Replica scheduling.
The inline WAL work alone averaged 29.7 and 44.2 ms in the combined
runs. The measurements are from shared hardware and short windows.
The sustained operating envelope remains a separate qualification.

## Mixed Type Group control

The established generator cycles through zero to 12 Type Groups with
a total of 55,000 offered payments. The driver offers 2,200/s for 25
seconds, excludes five seconds of warmup and gives 20 seconds to drain.
Both builds use the same topology, worker allocation, block budgets
and optional index settings as in the payment comparison.

| Configuration | Common TPS | Conditional p99 | Follower preparation | Validator CPU |
| --- | ---: | ---: | ---: | ---: |
| RocksDB, first | 2,194 | 0.99 s | 92.9 ms | 80.8 s |
| Both indexes, first | 2,193 | 0.95 s | 72.9 ms | 73.4 s |
| Both indexes, reverse | 2,232 | 4.38 s | 143.2 ms | 81.1 s |
| RocksDB, reverse | 2,184 | 1.47 s | 101.4 ms | 83.5 s |

All four arms acknowledged and finalized 55,000 payments. Each archive
passed independent replay. Throughput tracked the offered load. Tail
latency changed direction by run order, and the second combined run
had longer Follower preparation and higher p99 than its control.
Keep this workload separate from the overloaded payment result.

## Native recovery and regressions

Run the [certified write fault fixture](../execution-pipeline/README.md#integrated-commit-faults-with-live-payments)
with both index flags on one replica and the ordinary reader on its
three peers. Each of the three process-crash modes finished with all 24
payments finalized and an independently authenticated common Header.

| Interrupted point | Durable height at reopen | Common final height |
| --- | ---: | ---: |
| Before issuing the certified write | 3 | 10 |
| Before WAL synchronization | 4 | 11 |
| After WAL synchronization | 4 | 11 |

Before acknowledgement, the target withheld the pending Header and
finality proof. After restart the two indexes held 25 Live Cells and
49 Patricia nodes. The before-sync result includes the surviving kernel
cache. The fault model is process termination on Linux.

The all-features release workspace passed 553 tests across 26 suites.
Targeted checks reject altered durable node bytes, exercise an exhausted
budget, read a reclaimed node from the earlier pinned RocksDB snapshot,
and compare exact signed branch Headers, receipts and proof bytes with
both indexes enabled.

## Operation

The node index is opt-in. Operators can size it independently of the
Live Cell index and RocksDB cache. The combined run's peak was
1,434–1,558 MiB per replica, compared with 710–757 MiB in the controls.
Use `--cell-node-index-mib 768` for the measured node index, and add
`--live-cell-index-mib 1536` to reproduce the combined arm. Watch the two
`active` status fields and account separately for each cache. Host size,
workload and retention policy affect the operational memory budget.

## Verified decoded resident nodes

The `native-verified-cell-nodes` candidate stores decoded immutable Cell
nodes in the resident index. The index checks the canonical encoding and
content hash during recovery and during publication of committed inserts.
A cache hit during traversal carries its content ID and network/namespace
domain. The traversal validates that binding and reuses the decoded node.
Disk reads, speculative overrides and staged edits keep the ordinary
hash and encoding checks. Retired entries still fall back to the reader's
pinned RocksDB snapshot. Proof generation encodes cached nodes using the
same canonical representation and independent proof verification still
checks every node hash.

Each shard stores one domain for its whole table. Index budgeting uses
the decoded entry width and the actual table allocation sizes. Stored
RocksDB rows, the atomic WAL operation, Header commitments and proofs
use the existing formats.

The earlier fixed-size churn study recorded 39,607 median node requests
per edit, with every resident request resolving from memory. The new
path removes repeated cryptographic work from those resident hits.
It also adds content authentication to publishing new rows, currently
under each shard's write lock. Measure the read savings alongside
publication time, write-lock contention and fallback rate. Proof
generation re-encodes the decoded rows, so compare it separately.

Use a same-source Release build with this feature off and on. Use the
same signed corpus, `--cell-node-index-mib`, RocksDB cache, worker count
and Live Cell settings. Include payments and mixed Type Groups, and
collect matched fixed-block traversal timings before comparing the
network runs. Vary `jolteon-resident-node-publication` separately to
measure which side of the durable acknowledgement absorbs the extra
publish work. Compare exact encoded Header, receipts, proof paths,
charged cycles and node journals with the byte-reader control.

The compiled regressions cover mismatched node IDs and domains,
malformed nodes, mixed resident and raw reads, ordered reclamation,
exact proof bytes and fallback to an older pinned snapshot. This
candidate has build evidence. Runtime correctness and the net change
in traversal, publication and proof costs remain to be measured.

## Generated Cell node journals

The `native-verified-cell-journal` candidate carries immutable Cell nodes
from the Patricia updater into speculative descendants and the certified
writer. The updater hashes each new node in canonical form and retains
the decoded value and its content ID. Only the updater can create a
generated journal. Each journal is scoped to the exact network and
namespace. Its original content IDs and tombstones remain sorted in the
same order as the byte journal.

A descendant checks its ancestors newest first, including deletion
entries. Generated nodes carry their ID and domain into the tree reader.
The committed resident index publishes the same decoded nodes after
WAL acknowledgement. Publication compares the ID and domain for each
entry and skips byte decoding and cryptographic rehashing of generated
nodes. Disk and recovery reads still authenticate canonical bytes.
Old branches keep their pinned snapshots and state-proof output remains
canonical.

At the original position in the atomic WriteBatch, the writer encodes
Cell nodes into one reusable stack buffer and writes the same compact
RocksDB keys and values. The original write order and synchronous WAL
call apply to both layouts. The prepared block retains a single typed
Cell node journal in this mode. The normal byte journal remains the
control, with the earlier verified resident lookup enabled in both builds.

Use identical resource envelopes, worker counts, index budgets and
signed corpora to compare `native-verified-cell-nodes` and
`native-verified-cell-journal`. Collect the block's traversal and batch
timings alongside certified write time, publication time, serialized
WAL bytes and RSS at multiple in-flight depths. Account for encoding
moving into durable write preparation. Measure memory under churn since
the decoded journal stores fixed-width node values alongside tombstones.
Compare a disk-only arm and both choices for certified writer
publication separately.

The compile-checked regressions compare the generated journal against
the byte journal for mixed updates, exact Cell roots and proof paths.
They also compare the durable compact bytes and reclaimed keys, and
read the prior state from its pinned snapshot during certified write.
Performance, memory impact and runtime parity remain to be measured.

### Compact generated node journal

The generated-journal control stores a decoded node slot alongside every
content ID, including deletions. It builds a sorted BTreeMap after
collecting the per-subtree edits. The `native-compact-patricia-journal`
candidate builds the content IDs and tombstones in a separate vector
from the generated nodes. A put stores an index into the node vector.
A deletion stores only its content ID and empty slot. Subtree joins
adjust the right-hand indexes and concatenate the vectors. One stable
sort orders the lightweight entries and retains the last edit per ID.
Ancestor reads use binary search on that index, and the writer and
resident publication iterate the same sorted edits.

The option changes the in-memory representation of the verified
journal, including intermediate subtree output. The Patricia updater
still hashes every generated node in canonical form. The domain and
content ID checks, compact RocksDB encoding, durable write order and
the synchronous WAL boundary follow the existing generated-journal
path. The same layout backs serial and parallel updates. The
compile-checked regressions cover subtree joins, absent IDs,
tombstone precedence, exact previous-value callbacks, proof paths
and byte-level durable parity.

Compare `native-verified-cell-journal` against the same build with
`native-compact-patricia-journal` added. Keep the worker, resident index,
RocksDB cache, writer publication options and signed workloads fixed.
Record allocations and peak RSS during subtree joins and at different
in-flight branch depths. Measure Patricia edit and ordering time,
speculative lookup, certified write time, and committed node
publication. Compare the full node delta, proofs and durable bytes
against the control and run the existing recovery and replay checks.
Sorting can allocate temporary index storage, and binary search may
change the cost of sparse reads. Compare peak memory as well as the
retained journal footprint. Runtime validation and performance
measurements remain pending.

### Streaming verified edits into the resident shards

The normal publisher groups every edit into shard vectors. When the
certified writer publication option is enabled, it also counts the
edits first to preallocate those vectors. With
`native-stream-node-publication`, the resident index uses the high bits
of the content ID for shard assignment. The verified journal already
uses ascending content IDs, so each shard receives one contiguous run.
The publisher consumes that run directly under one shard lock and
continues with the next run. Stored byte edits still use the grouping
path. The streaming path accepts both verified journal layouts.

The shard operation still applies ordered deletions and insertions,
validates generated IDs and domains, checks the existing allocation
limit and retires the index on failure. Publication runs after the
synchronous WAL succeeds, either on the certified writer or during
installation on the Replica. An older pinned view can read reclaimed
IDs from its RocksDB snapshot. The new shard mapping also applies to
startup and point lookup.

Toggle `native-stream-node-publication` while holding
`native-verified-cell-journal`, the journal layout, writer publication
setting and all memory budgets fixed. Compare allocation counts,
`commit_jobs.cell_node_publication` for the certified writer arm,
publish and pinned lookup latency, RSS and common finality. Repeat with
the normal and compact generated journals. Different high and low
hash bits can change per-shard capacity rounding, so record accounted
bytes and residency for both controls. The change removes grouping
work from publication. Hash-table updates and shard lock holds still
need measurement under concurrent proof and execution reads.

The compile-checked regression compares streamed edits with grouped
byte edits on the same shard layout. It covers resident inserts and
tombstones, exact membership and absence proofs, an older snapshot
across the write, and both publication locations. Runtime checks and
performance measurements remain pending.

### Segmented generated node journals

`native-segmented-patricia-journal` builds on the compact generated
journal. A parallel subtree join transfers ownership of the populated
child node buffers. Only the compact lookup entries are appended and
their segment positions adjusted. New parent nodes use a separate
buffer. The stable content-ID sort keeps the same last edit, including
tombstones. Reads and publication locate the same authenticated node
through its segment and row position.

The layout changes how locally generated node bodies are retained and
merged. Every content ID still comes from canonical node encoding. The
journal presents the same sorted bytes and tombstones to the existing
read, compact RocksDB writer and resident publication paths. The WAL
synchronization point follows the existing certified write.

Compare `native-compact-patricia-journal` with
`native-segmented-patricia-journal` on the same funded parent, signed
workload, pinned state and cache allowances. Use one worker as the serial
control and 2, 4 and 8 for the parallel comparison. Measure subtree join
and edit CPU, allocation traffic, peak RSS, speculative read time and
certified batch construction. Hold stream publication and write-buffer
reuse fixed. Check exact root, previous-value callbacks, journal bytes,
proof paths and recovered state before comparing latency and finality.
A segmented location stores one extra machine word per entry. Multiple
buffers can retain spare capacity, and indirect lookups can cost more in
read-heavy or single-worker cases.

The compile-checked merge regression checks nested joins, retained
allocation identity, duplicate IDs and deletions. The existing Patricia
mutation differential covers serial and parallel roots, ordered
callbacks, exact node writes and proofs. Runtime and performance results
remain pending.
