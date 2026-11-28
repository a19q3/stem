# Full-block Follower preparation on one Ryzen 9 7950X3D

Four synchronized Followers with two dedicated physical cores each took **6–7% longer per block** than the same processes run individually on those cores. With the broad shared CPU affinity used in the saturation test, the median per-process time rose from **210–215 ms to 247–250 ms**. This experiment measures preparation on separate database copies on one host.

## Controlled placement

All results are median milliseconds per 2,048-transaction candidate. Numbers in a cell follow the listed CPU slot order. Each process prepared 16 timed candidates with exactly 1,365 verification-cache hits and 683 misses per candidate.

| Concurrent Followers | Physical CPU sets per process | Elapsed ms per process |
| --- | --- | --- |
| 1, four separate runs | `2,3` · `4,5` · `10,11` · `12,13` | 196.97 · 196.55 · 200.43 · 206.28 |
| 2, same 96 MiB CCD | `2,3` · `4,5` | 203.12 · 202.65 |
| 2, same 32 MiB CCD | `10,11` · `12,13` | 212.28 · 212.67 |
| 2, split CCDs | `2,3` · `10,11` | 195.72 · 202.34 |
| 4, disjoint slots | `2,3` · `4,5` · `10,11` · `12,13` | 209.53 · 210.89 · 215.20 · 215.35 |
| 4, same broad mask | All `2-7,10-15,18-23,26-31` | 248.25 · 249.65 · 246.47 · 246.61 |

The paired two-node placement across CCDs was close to the isolated controls. The broad four-process run also consumed **349–356 ms of process CPU per candidate**, versus **280–293 ms** with disjoint masks. In the timing-probe repeat, each process accumulated 1,908–2,191 Linux scheduler migrations with the broad mask and 234–562 with fixed masks. These counts include priming and waits and do not identify inter-CCD migrations. Shared CPU placement, cache capacity and background RocksDB activity coexist in this comparison.

## Full-block attribution

Single Follower, two physical cores on the 96 MiB CCD. Cold, partial and hot refer to **VM verification entries**. RocksDB had been opened and warmed on two disjoint candidates in every case. CPU time is whole-process CPU measured during decoding and preparation. The spans shown below are elapsed time on the coordinator unless identified otherwise.

| Per-candidate median | Cold | Partial | Hot |
| --- | ---: | ---: | ---: |
| Preparation including wire decoding | 397.88 ms | 199.93 ms | 112.03 ms |
| Process CPU | 460.82 ms | 265.96 ms | 172.77 ms |
| Wire decoding | 1.89 ms | 1.94 ms | 1.87 ms |
| Cell reads inside resolution and effects | 163.63 ms | 53.09 ms | 8.79 ms |
| Physical Cell-record requests | 4,096 | 2,731 | 4,095 |
| VM verification wave wall time | 133.51 ms | 47.55 ms | 4.19 ms |
| VM verification, summed worker elapsed | 257.57 ms | 88.09 ms | 3.15 ms |
| Cell Patricia traversal and update wall time | 70.41 ms | 72.23 ms | 73.08 ms |
| Patricia node requests | 38,493 | 38,493 | 38,493 |
| Canonical effects application | 2.80 ms | 2.58 ms | 2.45 ms |

Input and dependency resolution consumed 91.06, 34.04 and 14.73 ms in cold, partial and hot modes. Effects generation consumed 89.68, 32.96 and 7.24 ms, including existing output OutPoint checks. Those phases already include the Cell-read row. The covered lookups included 2,048 output absence checks per block. The hot run primed all 2,048 entries through admission before each timed block, warming RocksDB and displacing older in-process Cell-cache records. Its larger request count can coexist with lower latency.

Patricia node-read spans amounted to 97–102 ms summed across workers, with about 1 ms in node decoding. Worker elapsed time overlaps. No timing in this table isolates node hashing from the remaining tree work. The single speculative-parent control took 192.78 ms. Its 6,144 branch probes summed to 3.99 ms and hit no entries because the parent and child spent disjoint inputs. That comparison measures branch walk overhead and tree visibility. It undercovers positive speculative reads, rejected proposals and branch replacement.

Cold reads were the largest directly observed preparation stage in this fixture. The hot case instead left the Cell Patricia tree as the dominant measured stage. For a cold full candidate, only ~2 ms was spent decoding transaction bytes. The 366–372 ms network averages in the [saturation study](../saturation/README.md) include a different distribution of cache hits, branch state and concurrently active node tasks. The offline test isolates CPU placement and candidate preparation. It does not measure Leader selection, WAL sync, network or consensus wait, or an independent-host throughput ceiling.

## Reproduction and correctness

- Use the locked release build of `4075a62dda27451f8358a2768db4c8a92b61f03c` with default features. Recreate the 4,000 TPS [saturation configuration](../saturation/README.md#实验条件与复现), including the 1,440,000-cell prefill, and retain the stopped archival replica. The database used here had committed height 1,148.
- Walk the saturation driver's length-prefixed signed payments, considering every twelfth entry in order. Look up output zero in the stopped database. For each still-live output, sign a new payment of the same lock, with a single input, one output minus the fixture fee and the fixture's direct code dependency. The first 51,200 retained transactions came from 735,708 entries. The local corpus of u32 little-endian lengths followed by signed bytes had SHA-256 `63f3cd67ce0ebf9ab4a2f75919e3cfac4a3894770f58f3c0d0c7dd51bc6ea3fb`. Rerunning the earlier network can produce different live OutPoints.
- Reflink the database once per process and open each copy with a 256 MiB RocksDB block cache and 131,072 verification entries. Partition the corpus into 25 consecutive groups of 2,048. Group zero supplies the optional uncommitted parent, groups one and two warm each process, and groups three through 18 are timed. Prime the first 0, 1,365 or 2,048 transactions of each timed group with `Store::check`. Wait at a barrier after priming, then decode signed transaction bytes, invoke `Store::prepare` on the chosen view and record process CPU and elapsed time. Run the exact same candidates with a one-worker cold oracle. Use a separate one-worker oracle for the speculative-parent case.
- Set CPU affinity with `taskset -c` using the table's sets and record two Rayon workers per process. The kernel listed `0-7,16-23` in the 96 MiB L3 domain and `8-15,24-31` in the 32 MiB domain. The fixed slots exclude SMT siblings. Keep database setup, verification priming and barrier waits outside the timed span. Preparation builds the atomic write batch but never publishes or writes it.
- Independently compare every timed block with the applicable one-worker oracle by encoded Header, BLAKE2b digest of ordered encoded receipts and total charged cycles. The tested cache profiles, worker counts and placement profiles matched exactly. The scoped timing executable added counters to branch and Cell lookups, VM waves, transaction staging and Patricia node loads. Timed result rows exclude output serialization and measurement summaries.
