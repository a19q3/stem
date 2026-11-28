# Durable authenticated state comparison

## Workload and implementations

All arms start from 1,440,002 canonical Live Cell records in the same
authenticated payment archive at height 1,148. The records originate from
the [fixed Follower corpus](../follower-preparation/README.md). Seeding
recreates the exact source Cell root in both Patricia stores. The JMT store
authenticates the same OutPoints and record bytes using its own root format.

Compare three engines against the same RocksDB build, default compression,
256 MiB block cache, two pinned physical cores and two available workers.

- Canonical Patricia nodes, with the production parallel batch editor.
- Compact Patricia nodes, with the same editor and the production physical
  prefix encoding. This uses the deployed canonical root and proof format.
- Penumbra JMT 0.12.0 with SHA-256, the upstream mutable value-set API, a
  big-endian version prefix and the upstream Borsh node encoding. The same
  Stem key and canonical record digest feed its application layer. The
  upstream deletion-capable value-set path is serial.
  [Pinned upstream source](https://github.com/penumbra-zone/jmt/tree/943647ff7751904f7b29e52702804ce7cb213430)

Each arm starts on a separate reflink of its engine's seed. Warm the
clone's SSTs before timing. Run 128 batches of 2,048 spends and 2,048
creations at constant population, in forward and reverse engine order.
Select distinct slots at `(i × N / 2048 + 733 × round) % N`, using integer
division. Spend the selected OutPoint. Derive its replacement ID from the
old packed OutPoint and the next little-endian version and preserve its
Cell body and origin. Sort the resulting mutations by their Stem Cell keys.
All arms use one synchronous WAL write per batch for deleted records, new
records, node edits, obsolete node reclamation and the new authenticated
tip. RocksDB retains prior versions for pinned readers until they release
their snapshots.

Generate 1,024 membership and absence proofs every eighth batch and at
the end. Patricia uses its native shared-prefix batch traversal. JMT
uses the upstream point-proof API. Each query reads or returns the
corresponding record where present. Verify every serialized proof against
its own engine's new root and verify sampled transitions against the pinned
parent. Proc-wide CPU and peak RSS include placement, warmup, proof
verification, churn checks and compaction. The timed mutation path includes
record hashing, tree edits, write-batch construction and WAL synchronization.
The reported replacement rate uses those intervals after the first eight
batches.

Count application bytes as the exact record keys and inserted values.
Write amplification includes RocksDB WAL bytes and SST output reported by
file creation events, including the final forced compaction. The rate is
a durable state replacement rate from a single process. The fixture
does not run Jolteon, VM execution or the settlement protocol.

## Churn results

| Run order | Durable replacements/s | Median tree edit | Median WAL sync | Process CPU | Peak RSS | Final SST | Write amplification |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Canonical Patricia, first | 14,265 | 93.98 ms | 32.25 ms | 38.90 s | 670 MiB | 491 MiB | 57.43× |
| Compact Patricia, first | 11,986 | 116.50 ms | 33.81 ms | 43.57 s | 663 MiB | 469 MiB | 53.23× |
| JMT, first | 13,612 | 102.06 ms | 27.51 ms | 25.97 s | 598 MiB | 360 MiB | 38.47× |
| JMT, reverse | 16,890 | 85.86 ms | 18.63 ms | 22.20 s | 590 MiB | 360 MiB | 38.47× |
| Compact Patricia, reverse | 13,269 | 87.44 ms | 35.41 ms | 36.26 s | 667 MiB | 469 MiB | 50.89× |
| Canonical Patricia, reverse | 13,494 | 93.57 ms | 34.92 ms | 38.91 s | 680 MiB | 491 MiB | 57.43× |

JMT moved 13.6–27.3% more durable replacements per second than compact
Patricia and used 22.2–26.0 process CPU seconds against 36.3–43.6. The
canonical Patricia comparison changed direction between the two orders.
Each JMT run ended with 23.2% fewer SST bytes than the compact layout.
It wrote 852.4 MB of WAL against 728.4 MB for compact Patricia. The total
amplification fell because flush and compaction generated substantially
fewer bytes. Timings varied on the shared host. Final SST size and WAL
bytes repeated exactly within each engine.

The during-churn proofs include 512 present keys and 512 absent keys
sharing a prefix. JMT serialized an aggregate 1.36 MiB per 1,024-proof
batch against 2.10 MiB for either Patricia layout. JMT fetched a median
of 12,116 physical nodes against 6,969 shared-prefix Patricia requests.
Generation took 35.56–43.11 ms for JMT and 29.36–37.91 ms for compact
Patricia. Verification took 7.21–8.65 ms for JMT and 9.61–10.79 ms for compact
Patricia. The batch proof formats and node widths differ.

### Uniformly distributed proof queries

Use the same full seed and two orders. For each width, collect 20 rounds
after four warmup rounds. Alternate existing slots with absent keys
drawn from SHA-256 over the query index and round. Measurements cover proof
construction and serialized bytes. The 12-query case matches the current
RPC's highest batch width.

| Engine | Generation time at 12 queries | Generation time at 1,024 queries | Bytes at 1,024 |
| --- | ---: | ---: | ---: |
| Canonical Patricia | 0.85–0.88 ms | 33.49–34.76 ms | 2,198,442 |
| Compact Patricia | 0.77–0.78 ms | 30.83–31.06 ms | 2,198,442 |
| JMT | 0.49–0.51 ms | 32.32–32.87 ms | 1,421,862 |

JMT serialized 35.3% fewer bytes for the large batch. Its large read
path loaded approximately 7.31 MB of node data, versus 0.81 MB for
compact Patricia with shared prefixes. The smaller JMT batch also
generated faster. At the larger width the sequential JMT generator was
slightly slower, and its serialized proof verifier was faster. This
round verifies both membership and absence without concentrating misses
in a single prefix.

## Verified recovery

Every engine independently rebuilt its authenticated root from all
1,440,002 durable records after reopening. For JMT the independent
implementation folds the sorted keys using its binary grouping, leaf
compression and SHA-256 domains. A separate canonical Patricia fold over
each recovered image matched across all engines. This checked application
records independently of each engine's root encoding.

Each trial checked its spending and creation records after every round.
The runners compared all 128 rolling record digests between engines and
the final independently rebuilt canonical Cell root
`f965275ca831a87d432f700e5988bd396f49e13840106b266988b09bcd81dede`.
All stored engines also passed proof verification for both sides of
sampled transitions, including readers pinned before obsolete node
reclamation.

For each engine, a separate process was killed before issuing the next
atomic write and after the write returned with synchronous WAL enabled.
The first recovery retained version 303 and the second recovered version
304. All six restarts authenticated their complete recovered image and
the post-write canonical digests agreed across engines. After a normal
reopen, full-record verification including 1,024 proofs took 3.05–3.29 s
for compact Patricia and 3.43–3.49 s for JMT, excluding the database open.
The test exercises process interruption. Device or power-loss behavior
requires a separate qualification.

## Adoption decision

Keep the deployed compact Patricia engine. The isolated JMT results
justify an integrated research comparison for execution and finality.
Its root and proof format are different and would require a versioned
activation and a verified CKB and Intercell compatibility path. The
standalone durability and proof figures do not establish a network
throughput gain or a change in finality. The compact Patricia and JMT
storage savings here also depend on restart compaction, cache policy
and the warm, fixed-population payment trace.

For reproduction use SHA-256 JMT 0.12.0 and the pinned Stem RocksDB
dependency. Persist nodes under version and nibble path for JMT and
under canonical content hash for Patricia. For JMT prune stale node
keys in the same batch as the new root and retain the unchanged nodes.
Store the full canonical Live Cell bytes once per key in every engine.
Read earlier versions only using the corresponding pinned DB snapshot.
Rebuild both roots independently from sorted records after process
restart. The mutation, proof and recovery sections specify the common
corpus, run order and counters required to compare results.

## Replicated development comparison

The `native-research-jmt` feature builds both engines into the same executable.
Use a separate directory for each replica and engine. The JMT arm uses
`STEM_RESEARCH_JMT=1`. The Patricia arm leaves that variable unset. Both use
the existing finality journal and synchronous WAL. The database layout marker
includes the engine and genesis identity. The trial is limited to development
genesis without a CKB vault or Stake Registry. The JMT arm uses experimental
Cell proofs, while its System tree, receipt hashing and finality proof format
use the existing implementations.

On initialization the JMT arm builds only JMT Cell nodes from the same
canonical records as the Patricia arm. Both engines recompute the genesis
commitment on reopen from the ordered allocation keys and value digests,
without building a replacement node set or buffering the genesis Cell rows
for writing. The JMT startup check also compares the stored root node at
the committed height with the durable Header. Enabling the bounded Live Cell
index independently checks the complete committed record stream against the
selected engine's root.

Build a single release binary with the same feature and worker settings for
each round. Compare disk-backed resolution first, then enable the same
Live Cell index budget in both arms. Hold the RocksDB cache, VM workers,
resource limits, cadence, network topology, seeds and signed workload
constant. For this paired comparison omit the Patricia-specific resident
node cache from both arms. Use the resident Patricia configuration as a
separate optimized control and record its memory budget. Hold the cache
sizes and other features fixed across the original resident layout,
`native-verified-cell-nodes`, and `native-verified-cell-journal`. The
[generated journal comparison](resident-patricia.md#generated-cell-node-journals)
separates traversal, serialized WAL preparation and publication. The
read-only audit reads durable nodes in every arm. Run the paired
comparison in both orders and compare actual offer, common confirmation,
finality latency, backlog, RSS and the RocksDB metrics. Record genesis IDs
and signed workload digests separately. Chain-specific Cell origins and
Header IDs differ across engines after genesis, so any corpus with Header
Deps needs engine-specific references and a documented equivalent workload.

Use the same research binary to audit a stopped replica, with the exact
durable tip ID obtained from independently verified finality evidence:

```sh
cargo build --locked --release --features native-research-jmt --bin stem
target/release/stem research-cell-audit --home "$replica_home" \
    --genesis "$genesis_file" --expected-header "$verified_tip_id"
```

The audit opens RocksDB read-only. It independently folds all canonical
Cell records into the selected root, then generates, encodes, decodes and
verifies up to 512 membership paths and 512 absence paths. It reports
the proof bytes and separate open, root rebuild, proof construction and
verification timings. The Cell content digest covers the sorted keys and
Cell bodies and excludes chain-specific origins. An identical transition
trace can compare this digest across engines, while each arm still
authenticates its exact canonical bytes and origin. The digest alone
does not establish equivalent receipts, ordering, origins or resource charges.

The replicated implementation and its restart regressions are compile-checked.
The isolated measurements above remain the measured comparison. Replicated
throughput, finality, recovery after interruption and proof costs on the
same funded corpus await runtime qualification.

### Matched proof batch widths and JMT node reuse

The earlier proof measurements compare shared-path Patricia with the
upstream per-query JMT traversal. The research audit now offers a
batch-local JMT reader with bounded decoded-node reuse. It indexes each
node by the full JMT version and nibble path and retains up to the
configured number of successfully decoded nodes. Once full, it reads
remaining misses directly. The cache is emptied after each batch. The
upstream JMT proof builder still owns traversal and encoding semantics,
and its reader API clones the cached node on a hit. Cached reads can
save database work and decoding while retaining hashing and clone costs.

Set `--proof-batch-width` to the same value for both engines. The default
width is 1,024. A width of 12 exercises batches of up to the ordinary
Cell-proof RPC limit. The audit uses the same ordered membership and
absence samples at either width. The JMT arm accepts
`--jmt-proof-cache-nodes` from zero to 16,384. Zero selects the uncached
reader and is the default. Patricia uses its existing shared-path
traversal for each group.

Use independently authenticated terminal Headers for both copies of the
same stopped transition trace. Keep the executable, CPU placement,
RocksDB read options and initial OS page-cache conditions fixed. Repeat
the following with widths 12 and 1,024 and with the engine order reversed.
Set `cache_nodes` to zero for the paired uncached comparison. For the JMT
cache ablation, repeat on the same JMT image with several bounded values
and record the allowance and peak RSS in addition to the proof timings.

```sh
/usr/bin/time -f 'peak_rss_kib=%M' "$stem" research-cell-audit \
    --home "$patricia_home" --genesis "$genesis" \
    --expected-header "$patricia_tip" --proof-batch-width "$width"
/usr/bin/time -f 'peak_rss_kib=%M' "$stem" research-cell-audit \
    --home "$jmt_home" --genesis "$genesis" \
    --expected-header "$jmt_tip" --proof-batch-width "$width" \
    --jmt-proof-cache-nodes "$cache_nodes"
```

Compare `proof_reads.nodes` and `proof_reads.cells` for RocksDB calls and
returned value bytes. They count reads during proof construction and
exclude the independent full-state rebuild, absence sampling and JMT
root precheck. These are logical database calls and bytes. They cannot
be used as physical SSD reads. Compare the reported cache hits and peak
entries, proof byte totals, generation and verification wall time, and
process RSS. State the extra JMT cache allowance in the optimized arm.
An entry count does not measure the decoded node memory or allocator
overhead. Include the same-memory uncached control when reporting
cross-engine results.

The SHA-256 proof digest covers the length-prefixed encoded proofs in
query order. It must match across batch widths and cache settings for
the same database. Each encoded path is still decoded and checked
against the certified Cell root and expected presence or absence.
Compare full Cell content digests across engines and verify their
respective Headers independently. JMT and Patricia have different
proof encodings.

The compile-checked reopen regression exercises both engines, both
batch widths, bounded caches including a one-node limit, exact proof
digests and accounting of avoided database calls. No runtime
performance or recovery results have been collected for this change.
