# Mempool capacity under resident-state payment overload

The [resident-state saturation study](../resident-saturation.md) observed
4,403 and 3,584 common TPS near 8,500 offers/s. Both runs accumulated
acknowledged payments. This experiment isolates the effect of lowering the
local encoded mempool byte budget. Default Jolteon validates admission on
the Replica loop, including relayed transactions. Capacity checks already ran
before validation at 16 MiB. The candidate uses the same checks at 4 MiB.

## Method

Run four validators on the same Ryzen 9 7950X3D, without CPU or I/O separation
between replicas. Give the validators CPU IDs `2–7,10–15,18–23,26–31` and the
load driver `0–1,16–17`. Use the default-feature release node at `2b534a6`
with the capacity option and admission counters in this change. Run with
two VM workers, 131,072 verification-cache entries, a 256 MiB RocksDB
block cache, 1,536 MiB Live Cell index and 768 MiB Cell node index per replica.
Use a 100 ms proposal interval, 2.4 s view timeout, four-billion-cycle budget,
and a 2,048-transaction cap. Retain 64 heights on three replicas and run
the fourth as an archive.

Use [the external driver](../external/drive.py) to prepare 1,440,000 payment
inputs and a signed workload of 510,000 independent payments. Submit
at 8,500/s for 60 seconds with 256 ready senders and reused connections.
Compare seconds 20–60 and allow 35 seconds for drain. Run 16, 4, 4 and 16 MiB
in that order, each starting from a fresh copy of the same stopped funded
stores. The driver checks a common recent finalized Header and the archive's
exact funding Header before each offer. Both indexes remained active and
each sampled pool stayed within the configured byte limit.

To reproduce, use `prepare --rate 8500 --seconds 60 --drain 35 --senders 256
--funded-cells 1440000 --block-transactions 2048 --block-cycles 4000000000
--ready-senders --reuse-submissions --complete-volume` with the four-peer
topology above. Run `measure --fund-only` against the initialized fixture
and stop the nodes. Clone the funded directories and start each arm with
its own `mempool_max_bytes` value, then run
`measure --reuse-funding <payment-funding.json>`. Run `verify --report`
against each measured `report.json` in a separate process. See
[results.json](results.json) for hashes, actual measurement windows and
per-arm evidence.

## Measurements

Rates count actual writes, successful acknowledgements and confirmations
visible at all four validators during seconds 20–60. The last two events
can include writes sent during warmup. Backlog counts acknowledged payments
still awaiting common confirmation. Conditional p99 covers only writes in
the comparison window that were acknowledged and confirmed by the cutoff.
The two caps produce different accepted populations.

| Run | Offer TPS | Admitted TPS | Common TPS | Conditional p99 | Backlog start → end | Pending at 95 s |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 · 16 MiB | 8,506 | 6,175 | 5,120 | 65.86 s | 64,639 → 106,824 | 2,396 |
| B1 · 4 MiB | 8,507 | 5,319 | 5,427 | 37.05 s | 30,018 → 25,694 | 0 |
| B2 · 4 MiB | 8,500 | 5,346 | 5,427 | 36.98 s | 29,415 → 26,177 | 0 |
| A2 · 16 MiB | 8,501 | 6,114 | 5,069 | 65.66 s | 66,895 → 108,708 | 4,288 |

The smaller cap improved window throughput by 6.0% and 7.1%. It
reduced the largest sampled local queues from 69,327 to 17,331 encoded
transactions per validator. It also rejected more client submissions.
By the 95-second cutoff, A1 and A2 had jointly confirmed 414,094 and
409,997 payments respectively, while B1 and B2 had confirmed 347,532
and 350,407. Each run wrote all 510,000 requests to the network and the
only recorded submission error in the comparison windows was capacity
rejection. The p99 improvement describes the subset that got in and
finalized.

## Where the work changed

`Store::check` measures admission validation wall time, summed across the
four replicas and split by ingress. The other phase timings are per-event
means over sampled counter deltas. Execution phases can overlap. Process CPU
time is a separate measurement taken from the nearest one-second samples.

| Run | Client / relayed validation | Client queue | Control queue | Leader preparation | Follower preparation | Durable write | Local view timeouts | Process CPU |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 | 37.9 / 34.6 s | 13.3 ms | 4.9 ms | 121.8 ms | 242.1 ms | 43.5 ms | 0 | 274.3 s |
| B1 | 33.3 / 22.6 s | 10.5 ms | 6.4 ms | 103.0 ms | 239.1 ms | 45.0 ms | 0 | 261.1 s |
| B2 | 33.2 / 23.0 s | 10.4 ms | 6.2 ms | 103.0 ms | 241.9 ms | 42.5 ms | 0 | 263.9 s |
| A2 | 37.3 / 32.5 s | 13.2 ms | 6.7 ms | 119.5 ms | 240.0 ms | 45.4 ms | 0 | 269.9 s |

Relayed admission validation spent 29–35% less aggregate wall time. Leader
preparation took 14–15% less time. The share of inspected candidates with
inputs consumed by the speculative parent fell from about 37% to 30%.
CPU per common confirmation fell 9–10%. Follower preparation stayed near
240 ms and the control queue changed direction between pairs. All four
runs recorded zero local view timeouts. Attributing timeouts to WAL
latency requires a workload that produces both. Isolating CPU contention
from the change in accepted traffic requires separate worker measurements.

All four independent archive replays verified the signed workload coverage,
Header links, transaction commitments, terminal finality certificate, and
sampled execution receipts. The replay rebuilds the transaction commitments
from the signed inputs and checks the sampled proofs. Revisions, digests and
verification output are in [results.json](results.json).

The native integration suite passed 38 tests. The library suite with the
optional admission pipeline passed 26 tests. The signed-wire admission
identity regression also passed.

## Decision

Keep 16 MiB as the default. The 4 MiB configuration is useful to study early
rejection under overload and trades drain-inclusive volume for improved
window throughput and a smaller backlog. Both caps were evaluated on one
host with independent payments at one offer rate. Sustainable admission
and operation at 20,000 offers/s require separate trials.

The next useful isolation is the relayed admission lane. Hold the queue
budget fixed and count repeated wire identities separately from first-seen
validations before changing admission scheduling. Compare confirmation,
rejects and total completions over the same offer and drain windows.

The [fixed-budget follow-up](../relay-attribution/README.md) measures
first and repeated relays and compares the optional client worker.
