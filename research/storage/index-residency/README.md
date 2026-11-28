# Cell Node Index residency under sustained churn

The native resident Patricia index can retire after a shard cannot
reserve its next allocation. Earlier 60-second offers observed this
transition during drain. The stable node count and growing table size
motivated testing whether a bounded shard rebuild could retain residency.
[Earlier fixed-core measurements](../relay-scheduling/README.md)

The candidate rebuilds a shard into a fresh table sized to its current
entries and the new node. The old and replacement table allocations count
together at the peak. When a new allocation cannot fit, the index keeps
existing nodes and skips that insert. A skipped content address resolves
through the reader's RocksDB snapshot. Durable writes, branch lookups and
canonical Patricia hashing keep their existing order. Startup still checks
cached bytes against canonical node hashes before publishing the index.

## Reproduction

Use the same four-replica fixture as the relay experiment with the 16 MiB
mempool, 1,440,002 initial Live Cells and 12 physical cores. Keep 768 MiB
for the Cell Node Index, 1,536 MiB for the Live Cell index and 256 MiB
for the RocksDB block cache. Use two VM workers, 131,072 verification
entries, 2,048 transactions and four billion cycles per block, a 100 ms
proposal interval and a 2.4 s timeout. Disable the experimental relay
and client workers. Start each arm from the same stopped funded stores.

Use the external driver with `--rate 8500 --seconds 120 --drain 35
--senders 256 --funded-cells 1440000 --block-transactions 2048
--block-cycles 4000000000 --ready-senders --reuse-submissions
--complete-volume`. Compare the original index allocation path, the
candidate twice, then the original path again. Both release builds
include the same additive RocksDB read counters. Reuse the exact funding
Header and verify each signed archive after measurement. [Fixture setup](../admission-pressure/README.md#method)

Score the fixed 40-second wall-clock windows from 20–60 and 80–120
seconds. Record backlog at each boundary and confirmations at 155
seconds. Use the closest bracketing status, RocksDB and Linux process
samples for stage, logical read and CPU deltas. Samples and exact
boundaries are recorded with the results. Logical RocksDB keys and
bytes describe database operations, including work served from caches.
The process I/O counters are retained separately.

## Results

| Run | Common TPS, 20–60 s | Common TPS, 80–120 s | Late logical RocksDB keys | Late Follower Cell tree | Common at 155 s | Pending at 155 s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 · retire on allocation failure | 5,427 | 3,635 | 8,374,496 | 125.5 ms | 652,137 | 13,235 |
| B1 · bounded rebuild | 5,427 | 4,915 | 69,114 | 38.2 ms | 734,893 | 0 |
| B2 · bounded rebuild | 4,301 | 3,994 | 65,812 | 39.2 ms | 601,108 | 22,554 |
| A2 · retire on allocation failure | 5,427 | 3,277 | 7,480,722 | 133.4 ms | 590,710 | 34,212 |

The two controls retired the index at 64.2–70.2 seconds while the driver
was still offering transactions. All candidate replicas retained it through
155 seconds. Each candidate replica rebuilt 64–111 shards and skipped zero
inserts. The peak accounted allocation including the replacement table was
617,677,840 bytes (589.1 MiB). Both candidates used the original 768 MiB
limit. Peak sampled RSS across replicas was 1,864–1,872 MiB for the
candidate and 2,046–2,057 MiB for the controls, taking the largest replica
in each arm.

The late common rate rose 35.2% and 21.9% in the two pairs. On the same
late windows logical database key reads fell by more than 99% and process
CPU per common confirmation fell 20–21%. Logical bytes fell from 861 to
399 MB and from 778 to 331 MB. The late Follower Cell tree took roughly
70% less time in both pairs. Candidate B2 had 20.8% lower early throughput.
Its Leader prepare rose from 114.2 to 199.8 ms while its Cell tree time
stayed close to the control. The experiment establishes the storage effect
and leaves the cause of that early Leader variance open. Each arm was
under overload. A sustained-capacity claim needs a separate low-backlog run.

All four archives passed independent verification of signed transaction
commitments, Header links, terminal finality and sampled execution receipts.
Library tests cover fixed-cardinality churn, peak budget and retaining old
entries when a new shard allocation cannot fit. Existing tests exercise
authenticated recovery and snapshot reads across publication.
The release run passed 28 library and 38 native integration tests.
[Digests, exact sampling boundaries and raw counters](results.json)

## Decision

Use bounded rebuilds for the opt-in resident Patricia index. When a
runtime allocation exceeds the budget, keep the usable entries and let
individual misses reach RocksDB. The fixed-budget storage comparison
supports retention under load. The remaining Leader variance warrants its
own matched experiment.
