# Relay verification scheduling at a fixed CPU budget

The [previous experiment](../relay-attribution/README.md) found 17
repeated validations in 1,137,296 relay checks. This experiment tests
the placement of first-time relay validation at a fixed 16 MiB mempool
cap. The client admission path is inline in all arms.

An optional per-replica worker uses the existing read-only pinned Store
view to validate decoded, authenticated relay traffic. The Replica retains
control of decoding, identity deduplication and pool insertion. The
pending queue is limited to 128 jobs and 4 MiB of encoded transactions.
Capacity exhaustion falls back to inline validation. The Replica checks
epoch, wire identity and pool space when a result returns. A missing
snapshot or different committed Header triggers a live recheck. The
existing consensus paths handle proposal validation, signing and
journal updates.

## Method

Use the [funded payment fixture](../admission-pressure/README.md#method)
with 1,440,002 initial Live Cells, both resident indexes and four replicas
on the same Ryzen 9 7950X3D. Clone the same stopped stores for every arm.
Use one default-feature release binary, two VM workers per replica,
131,072 recent VM verification entries, a 256 MiB RocksDB cache,
1,536 MiB Live Cell index and 768 MiB Cell Node Index. The transaction
cap is 2,048 and the cycle cap is four billion. The proposal interval is
100 ms and the view timeout is 2.4 s.

Build the archived prototype at `9ab91663ac30f9da5dce05452ad8824f96ccd0f5` to reproduce
these runs. This report's revision carries the result and uses inline
relay verification.

Run inline, shared, reserved, reserved, shared and inline in that order.
Pass `relay_admission_worker: true` only for the shared and reserved arms.
In inline and shared, all node threads can run on CPUs
`2–7,10–15,18–23,26–31`. In reserved, remove CPUs `7,15,23,31`
from the validator processes' startup masks and rebind each persistent
`stem-relay` TID after startup. The relay workers of replicas 0 and 2
share CPU 7 and its SMT sibling 23. The workers of replicas 1 and 3
share CPU 15 and sibling 31. Both layouts use the same 12 physical
cores, with one per CCD reserved for relays in the split layout. The
driver uses CPUs `0–1,16–17` and the sampler runs on `8–9,24–25`.

Use the original external driver commands with 510,000 signed independent
payments, 256 ready senders and connection reuse. Drive 8,500 scheduled
writes/s for 60 seconds and drain for 35 seconds. Score common observations
and client acknowledgements in the fixed wall-clock window from 20 to 60
seconds. Record full confirmation and backlog at 95 seconds. Check every
sampled thread affinity and record cumulative `schedstat` CPU time for
the Replica, relay workers, proposal and candidate dispatch threads, the
shared execution pool and durable writer. Dispatcher CPU and wall timings
have different scopes. The shared VM pool serves both Leader and Follower
work. Run the independent signed-archive verifier for each arm.

## Results

| Run | Actual writes/s | Acks/s | Common TPS | Conditional p99 | Backlog at 20 → 60 s | Common confirmations at 95 s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 · inline | 8,494 | 6,616 | 5,427 | 65.49 s | 57,012 → 104,553 | 435,010 |
| B1 · shared | 8,513 | 6,607 | 5,427 | 65.22 s | 57,185 → 104,374 | 433,564 |
| C1 · reserved | 8,500 | 6,543 | 5,376 | 64.83 s | 57,567 → 104,249 | 431,682 |
| C2 · reserved | 8,512 | 6,404 | 5,274 | 65.60 s | 59,418 → 104,619 | 425,421 |
| B2 · shared | 8,496 | 6,374 | 5,222 | 64.79 s | 59,845 → 105,929 | 425,157 |
| A2 · inline | 8,500 | 6,411 | 5,274 | 64.65 s | 60,287 → 105,789 | 426,558 |

All six runs sent all 510,000 requests and closed with zero acknowledged
payments pending at 95 seconds. Conditional p99 covers offers written in
the scored window that were acknowledged and confirmed by the cutoff.
All archives passed independent verification of Header links, signed
workload commitments, terminal finality certificates and sampled execution
receipts. The benchmark retained 12 physical cores in every layout, with
zero sampled thread affinity mismatches and zero local view timeouts.
There were 38 passing native integration tests and 28 passing library tests
with the optional client admission feature. [Measured data](results.json)

| Run | Mean client queue | Mean control queue | Mean Leader prepare | Mean Follower prepare | Replica CPU | Relay worker CPU | All replica CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 | 14.1 ms | 4.9 ms | 114.0 ms | 221.5 ms | 96.1 s | — | 279.8 s |
| B1 | 10.8 ms | 6.2 ms | 115.0 ms | 215.0 ms | 86.8 s | 20.6 s | 287.3 s |
| C1 | 10.8 ms | 5.9 ms | 114.9 ms | 218.0 ms | 86.1 s | 18.7 s | 282.8 s |
| C2 | 11.7 ms | 6.5 ms | 115.3 ms | 219.2 ms | 82.7 s | 18.1 s | 278.5 s |
| B2 | 12.1 ms | 9.4 ms | 113.1 ms | 220.1 ms | 82.6 s | 19.7 s | 278.1 s |
| A2 | 14.5 ms | 8.3 ms | 111.2 ms | 223.2 ms | 91.4 s | — | 271.6 s |

The shared relay worker changed common throughput by 0.0% and −1.0%.
Client queue time fell. Control queue time rose in both paired
comparisons. Total process CPU rose 2.4–2.7%. Reserving cores changed
throughput by −0.9% and +1.0% relative to the nearby shared arms. The
result supports keeping those cores available to all replica work under
this workload.

### How much relay work moved

| Run | Queued | Inline fallbacks | Live rechecks | Rejected for pool capacity at completion | Mean worker wait |
| --- | ---: | ---: | ---: | ---: | ---: |
| B1 | 124,315 | 198,509 | 11,362 | 16,044 | 1.224 ms |
| C1 | 123,558 | 195,608 | 11,031 | 15,874 | 1.177 ms |
| C2 | 119,829 | 187,046 | 10,773 | 16,194 | 1.179 ms |
| B2 | 119,113 | 185,043 | 11,261 | 15,982 | 1.184 ms |

The 128-job cap returned 61% of dispatch attempts to inline validation.
Pending encoded bytes peaked at 30,976, well below the 4 MiB byte cap.
Each replica averaged 36–38 outstanding jobs in sampled status. At the
observed completion rate and 1.33–1.39 ms worker wait plus service, the
estimated number of jobs waiting for or using the worker was close to one
per replica. The pending count includes completed results awaiting
consumption. Directly timing this interval would test whether staged
result handoff is the main source of backpressure. All four worker
runs repeated about 9% of completed checks against the live tip.

Both resident indexes covered the full 40-second scoring window. Every
replica retired the Cell Node Index afterwards, first observed between
65.3 and 69.5 seconds after the start. The 95-second measurements include
the RocksDB fallback. A matched storage test should hold the index budget
fixed and compare what happens to lookups when a shard needs to grow.

## Decision

Archive the worker prototype and use inline relay validation. Prioritize
bounded index residency and direct observation of completion handoff.
This worker trial moved only a share of relay validation off the Replica
and used one independent-payment overload rate on one host.
