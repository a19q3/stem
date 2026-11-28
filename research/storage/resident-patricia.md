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
Allocation checks include a shard's possible replacement table. The
original implementation retired the index when a shard could not grow.
The follow-up covers bounded rebuilds and per-node RocksDB fallback.
[Longer residency comparison](index-residency/README.md)

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
