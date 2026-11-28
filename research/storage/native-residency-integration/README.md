# Native execution with bounded authenticated residency

Revision `965546a` combines the Native V1 execution profile
from `bff2904` with the bounded Cell node index from
`9b3fbf4`. The candidate reuses verified Patricia node
hashes when rebuilding a resident shard. A proposal worker
timer separates execution service from snapshot capture.

## Matched payment runs

Build the control with the default features and the candidate
with `--features native-v1-profile` from `965546a`. Use
four validators on the same 7950X3D with the shared CPU mask
`2–7,10–15,18–23,26–31` and driver mask `0–1,16–17`.
Pin the same binaries and dependencies for each repeat.
Use two VM workers, 16 MiB per mempool, a 256 MiB RocksDB
block cache, a 1,536 MiB Live Cell index and a 768 MiB
Cell node index. Keep the 2,048-transaction and four-billion-cycle
block limits, 100 ms proposal interval and 2.4 s view timeout.

Clone four stores from the same authenticated 1,440,002-Cell
fixture. Offer 5,500 independent signed payments/s for 60 s
with 256 ready senders and submission-connection reuse.
Allow 35 s to drain. Score the 40-second wall-clock window
from seconds 20 through 60. The order is default, Native,
Native, default. Replay the signed archive after each arm.
[External driver](../external/drive.py) · [Measurements and digests](results.json)

| Arm | Actual writes/s | Common TPS | Accepted pending at 20 → 60 s | Conditional p99 | Replica CPU |
| --- | ---: | ---: | ---: | ---: | ---: |
| A1 · default | 5,500.0 | 5,497.8 | 5,854 → 6,096 | 2.179 s | 261.3 s |
| B1 · Native | 5,500.0 | 5,543.9 | 5,273 → 3,445 | 1.798 s | 241.0 s |
| B2 · Native | 5,500.0 | 5,527.7 | 4,172 → 3,075 | 1.601 s | 248.5 s |
| A2 · default, impaired repeat | 5,015.8 | 3,020.8 | 14,477 → 94,274 | 62.220 s | 184.4 s |

Confirmations include work admitted before the scoring window.
Conditional p99 considers writes in the window acknowledged and
confirmed by the cutoff. A1 and both Native arms acknowledged
and finalized all 330,000 payments. A2 acknowledged 328,139
and finalized 291,643 by the end of the drain. Both resident
indexes stayed active and all archives independently replayed.

A2 saw four local view timeouts and 186 ms of mean durable
writer time, compared with 43 ms in A1. It missed the fixed
write rate during the scoring window. The aggregate non-replica
busy time on the validator mask ranged from 21 to 88 seconds
across the four runs. Keep A2 as a separate observation of
the impaired run. Its throughput and conditioned latency
cannot establish a matched reverse-order effect.

## Execution and persistence

| Arm | Leader build | Follower prepare | Durable writer | Median nonempty block |
| --- | ---: | ---: | ---: | ---: |
| A1 | 89.2 ms | 172.4 ms | 42.6 ms | 1,948 payments |
| B1 | 43.7 ms | 78.8 ms | 28.3 ms | 1,205 payments |
| B2 | 44.4 ms | 81.5 ms | 33.6 ms | 1,202 payments |

Blocks were smaller in the first Native run. Summed Leader
service per common confirmation went from 47 to 34–35 µs,
and summed Follower service across the replicas went from
273 to 186–192 µs. These are wall-time stage totals normalized
by confirmations. Some stages overlap. The comparable arms
recorded zero local view timeouts and the Native runs used
5–8% fewer replica CPU seconds. Durable writer service per
common confirmation showed mixed results.

Retain the combined profile as opt-in. The measured gain
appears in execution work, backlog and latency near 5,500
offers/s. Further tests should use the same full block size
for state tree and WAL attribution, and repeat the control
under steady host load before using the measurements to
choose a production default.

The release build with `native-v1-profile` passed 60 library
tests and 44 native store integration tests. The shard churn
regressions use the public canonical Patricia commitment to
generate the fixture hashes.
