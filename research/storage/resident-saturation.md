# Payment saturation with both resident state indexes

Use the deployed native execution configuration at `7b85682` and
enable both `--cell-node-index-mib 768` and `--live-cell-index-mib 1536`.
Each run starts from a distinct copy of the same authenticated state with
1,440,002 Live Cells. The four validators share a Ryzen 9 7950X3D.
The placement, 256 MiB RocksDB cache, 131,072 verification entries, two VM
workers, 100 ms proposal interval, 2.4 s timeout, four-billion-cycle
envelope and 2,048-item cap match the
[resident index comparison](resident-patricia.md#native-four-replica-comparison).

Offer each rate for 60 seconds, use the last 40 seconds for comparison
and allow 35 seconds to observe drain. Count actual writes, successful
admissions and confirmations visible at all four replicas. The
admission count for a window uses response time and can include an
offer at the edge of the previous window. Count outstanding
acknowledgements at each measurement boundary. Conditional p99 covers
offers written in the measurement window which were acknowledged and
confirmed by the cutoff. The independent archive replay checks each
terminal certificate and verifies the confirmed transaction history.

The two 4,000/s reference runs both finalized all 240,000 payments.
They provided the starting point. Increase the actual offer in larger
steps until higher admitted load yields a similar common confirmation
rate and higher backlog. Recheck the observed plateau in reverse order
and probe the lower shoulder separately. The aim is to distinguish
where a queue starts growing from the throughput available under already
saturated load. Configuration and hardware bound both observations.

## Completed measurement windows

| Run | Actual offer TPS | Admitted TPS | Common confirmation TPS | Conditional p99 | Backlog at start → end | Pending after drain |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 4,000 reference 1 | 4,000 | ≈4,000 | 3,994 | 1.31 s | 2,859 → 3,091 | 0 |
| 4,000 reference 2 | 4,000 | ≈4,000 | 4,013 | 2.41 s | 2,591 → 2,161 | 0 |
| 4,500 run 1 | 4,500 | 4,499 | 4,496 | 1.97 s | 3,403 → 3,521 | 0 |
| 5,000 run 1 | 5,000 | 5,000 | 4,425 | 28.72 s | 3,529 → 26,519 | 0 |
| 5,000 run 2 | 5,000 | 4,998 | 5,019 | 11.29 s | 4,077 → 3,251 | 0 |
| 6,500 run 1 | 6,500 | 6,278 | 4,864 | 57.51 s | 32,313 → 88,876 | 0 |
| 6,500 run 2 | 6,511 | 6,081 | 4,147 | 58.05 s | 25,076 → 102,437 | 0 |
| 8,500 run 1 | 8,510 | 5,455 | 4,403 | 67.08 s | 67,954 → 110,030 | 0 |
| 8,500 run 2 | 8,522 | 4,580 | 3,584 | 68.82 s | 82,727 → 122,566 | 24,292 |
| 10,000 run 1 | 10,026 | 4,571 | 3,942 | 67.21 s | 107,547 → 132,704 | 14,585 |
| 10,000 run 2 | 10,075 | 4,873 | 4,198 | 62.79 s | 97,205 → 124,203 | 30,022 |

The 4,500/s run finalized all 270,000 offers, with a 118-payment increase
in backlog over the measurement window and 1.97 s p99. This is the
highest completed low-backlog point. The two 5,000/s runs divide the
transition region. One accumulated 22,990 payments in the measured
window. The repeat drained its starting backlog and still had an 11.29 s
p99. The two 6,500/s runs both had backlog growth and a p99 above 57 seconds.

The highest observed window was 5,019 common TPS at 5,000/s offer.
Its confirmations included warmup payments. At the larger rates
admission and useful confirmation both fell. At approximately 10,000/s
the two runs admitted 46–48% of the offered rate in the measurement
window and still had 14,585 and 30,022 pending at the drain cutoff.
This supports an observed transition around 4,500–5,000/s on this
configuration, with a variable overload throughput and collapse at
higher offers. The 4,500/s point has one completed observation.

The reference arms at 4,000/s averaged 913 and 1,023 payments per
nonempty block. Above 5,000 offered TPS the median reached the
2,048-item cap. In the 6,500/s runs, the measured full blocks produced
common confirmations at about one such block every 421–494 ms. The
configured proposal cadence remained 100 ms.

The p99 populations vary with admission and finality. In particular,
the larger rates exclude writes that received no admission and accepted
payments still pending at drain. Each completed archive passed
independent replay. A subsequent 4,750/s trial was stopped before
completion and is excluded. The report retains the node's current
admission policy and describes single-host behavior.
