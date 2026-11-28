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
