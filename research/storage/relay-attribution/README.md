# Relayed admission and client verification isolation

The [capacity experiment](../admission-pressure/README.md) scored
confirmations observed during a fixed 40-second wall-clock interval, from
20 to 60 seconds after the start of the offer. Both 4 MiB arms confirmed
217,088 payments during that interval, or 5,427.2 TPS. Their
347,532 and 350,407 totals use the full observation through the 95-second
drain cutoff. The 40-second count can include payments submitted during
warmup. This study keeps the default 16 MiB cap and uses both denominators.

## Method

Run the same four-replica [resident-state fixture](../admission-pressure/README.md#method)
on the Ryzen 9 7950X3D with both authenticated indexes active and 1,440,002
initial Live Cells. Offer 8,500 independent payments per second for 60 seconds,
measure seconds 20–60 and allow 35 seconds for drain. Clone the same stopped
funded disk images for each arm. Use the same signed inputs, worker count,
cycle and transaction limits, caches, CPU masks and client routing.

Set `measure_relay_admission: true` for each measured replica. Compare
`cargo build --release --locked --bin stem` with the same command plus
`--features jolteon-admission-pipeline`. Run inline, worker, worker and
inline in that order. The feature moves client submissions to an existing
bounded background validation thread. Relayed submissions run inline in
both arms. All threads share the same allowed CPUs in both arms.

For each relay, classify the exact signed-wire hash at the receiving replica
as first-seen or repeated. Within each group count pool hits, capacity
rejections, decode failures and actual `Store::check` calls. Separate a
hash's first such relay check from repeated checks against the same or a
newer committed height. Tag hashes observed during normal local
certified commit in this process. A first relay at one replica can be a
first relay at a different replica as well. The experiment counts repeated
checks at the same receiving replica. Each replica can retain 524,288
identities for the measurement and reports overflow explicitly.

For reproduction, follow the prior experiment's funding and replay commands,
with 16 MiB and relay measurement enabled in every measured configuration.
Use one common funded copy with relay measurement disabled while funding.
Record both binaries, the harness and the contract hashes. Align Linux
process and thread CPU counters to the driver's monotonic measurement window.
Counter deltas are based on the nearest completed status sample at each
boundary. Report conditional confirmation p99, acknowledged work still
pending at cutoff, and the sum of confirmations over the full 95 seconds.

## Results

| Run | Writes/s | Acks/s | Common TPS | Conditional p99 | Backlog at 20 → 60 s | Confirmed at 95 s | Pending at 95 s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 · inline | 8,497 | 6,170 | 5,018 | 65.63 s | 60,180 → 106,268 | 398,440 | 18,437 |
| B1 · worker | 8,500 | 5,572 | 4,659 | 66.63 s | 69,801 → 106,293 | 380,650 | 12,086 |
| B2 · worker | 8,500 | 6,454 | 5,376 | 64.31 s | 56,876 → 99,976 | 427,914 | 0 |
| A2 · inline | 8,508 | 6,344 | 5,222 | 64.78 s | 59,680 → 104,551 | 423,226 | 0 |

All four drivers wrote the entire 510,000-payment workload. All four archives
passed independent replay of Header links, signed transaction commitments,
terminal finality and sampled execution receipts. The two windows and the
per-arm proof and binary fingerprints are recorded in [results.json](results.json).
The native suite passed 38 tests. The library suite with the admission
worker passed 27 tests, and its concurrent signed-wire regression passed.

### Which relays reached validation

| Run | First relay frames | Repeat relay frames | Repeat pool hits | Repeat capacity refusals | First check on a repeat frame | Total relay checks | Repeat checks | Checks after local finality |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 | 468,122 | 83,359 | 80,557 | 2,008 | 791 | 279,582 | 3 | 379 |
| B1 | 448,086 | 86,578 | 81,215 | 3,011 | 2,351 | 244,579 | 1 | 2 |
| B2 | 493,878 | 87,009 | 84,016 | 1,592 | 1,394 | 322,851 | 7 | 11 |
| A2 | 474,977 | 81,342 | 79,772 | 851 | 713 | 290,284 | 6 | 222 |

Of 338,288 repeated frames, 325,560 hit the existing pool entry. Some of
the remaining frames received their first validation after an earlier
capacity refusal. Only 17 of 1,137,296 relay validation calls repeated a
previous check at that receiving replica. All 17 occurred at a newer
committed height, followed local finality, and failed. They took 47 μs
in aggregate, out of 149.8 s of relayed validation wall time summed across
the four windows and replicas. The tracking tables did not overflow
and the classified frame counts and elapsed times reconcile with the
existing aggregate counters. These are relays handled by the Replica.
The aggregate transport counters separately recorded 828–1,788 drops
of frames across all message classes per window.

### Thread isolation and the drain

| Run | Client queue | Control queue | Leader prepare | Follower prepare | Durable write | Process CPU | Admission worker CPU | View timeouts |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 | 13.3 ms | 5.4 ms | 143.0 ms | 225.5 ms | 44.3 ms | 263.4 s | 0.0 s | 0 |
| B1 | 6.6 ms | 3.2 ms | 183.3 ms | 216.3 ms | 43.5 ms | 244.3 s | 36.5 s | 0 |
| B2 | 8.6 ms | 6.6 ms | 117.2 ms | 212.0 ms | 43.4 ms | 281.9 s | 42.6 s | 0 |
| A2 | 14.5 ms | 10.2 ms | 114.4 ms | 222.0 ms | 46.9 ms | 266.5 s | 0.0 s | 0 |

The client worker cut client and control queue means in both comparisons.
Common TPS changed by −7.1% and +2.9%. The worker recorded no full
queue refusals or missing snapshots in the scored windows. Committed
tip rechecks covered 0.3–0.5% of dispatches. These measurements isolate
work from the Replica thread with a shared CPU set. Testing a reserved
CPU budget or moving relay validation requires a separate comparison.

Both state indexes stayed active through the 40-second scoring window.
Every replica then retired its 768 MiB Cell Node Index during drain,
first observed between 66.4 and 82.7 seconds after the offer began,
at heights 744–747. The last active samples still held 2,880,003 nodes,
while the allocated table size had grown. Drain results include the
fallback to RocksDB. The status counters establish the transition.
A separate storage control can test which table growth operation
triggered retirement.

## Decision

Keep the background client worker optional. The observed relay load
supports investigating the cost and scheduling of first-time local
validation, with each validator still checking independently. Additional
reuse for repeated relay checks has negligible measured headroom here.
Separately test a drain with steady Cell Node Index residency before
using 95-second totals to qualify an overload policy. The observations
cover single-host payments near 8,500 writes/s.

The [fixed-core follow-up](../relay-scheduling/README.md) measures first
relay verification off the Replica and tests reserved CPU placement.
