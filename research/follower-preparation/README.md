# Full-block Follower preparation on one Ryzen 9 7950X3D

Four synchronized Followers with two dedicated physical cores each took **4.4–7.4% longer per block** than the same processes run individually on those cores. With the broad shared CPU affinity used in the saturation test, the median per-process time rose from **210–215 ms to 247–250 ms**. This experiment measures preparation on separate database copies on one host.

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
| RocksDB Cell-record requests | 4,096 | 2,731 | 4,095 |
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

## Candidate-local Cell reads

The optional `native-candidate-cell-prefetch` feature resolves up to 64 unique OutPoints in parallel for a window of at most 32 ordinary one-input, one-output transactions. The window records exact-parent input lookups and output existence checks. The existing serial staging path still orders all effects and charges. It reads current-block writes and tombstones before consulting the window. The newest speculative ancestor is authoritative over the committed cache and database. Tentative values stay out of the shared Cell cache. The prefetch path accepts records up to 16 KiB and falls back to the ordered lookup for oversized records and read or decode errors. Dynamic dependencies remain in canonical resolution.

The two release binaries differed in the feature flag. In run order A–B–B–A, each single-Follower arm prepared 16 distinct full blocks per cache case with the same signed bytes, parent and pinning as the timing study. The table pools the two orders for those cases. The concurrent and worker-count comparisons each ran one pair.

| Case | Control wall | Parallel reads wall | Change | Control → candidate process CPU |
| --- | ---: | ---: | ---: | ---: |
| Cold verification cache | 405.22 ms | 311.12 ms | −23.2% | 452.21 → 447.29 ms |
| Partial verification cache | 199.87 ms | 171.92 ms | −14.0% | 261.49 → 259.33 ms |
| Hot verification cache | 109.53 ms | 108.33 ms | −1.1% | 169.44 → 172.40 ms |
| Speculative parent, partial cache | 187.44 ms | 169.00 ms | −9.8% | 255.81 → 256.64 ms |
| Four pinned Followers, partial cache | 207.30 ms | 184.78 ms | −10.9% | 287.90 → 285.46 ms |
| Same four cores, two workers, partial cache | 197.01 ms | 181.78 ms | −7.7% | 262.63 → 266.67 ms |
| Same four cores, four workers, partial cache | 146.09 ms | 111.04 ms | −24.0% | 280.66 → 284.63 ms |

All 448 timed executions in the matched feature comparison agreed with the separate one-worker oracle on the full Header, ordered receipts and charged cycles. A focused regression spends a speculative parent output and then spends nine preceding outputs within the same prefetched window. It compares serial and parallel Headers and receipts and the rejection of a double spend and a replacement parent. The host retains the same two-worker setting for live comparisons. Increasing the isolated worker count recovered Cell-trie time, from 71.00 ms to 40.62 ms in the two controls on the same four-core mask, at a process CPU cost.

## Four-replica payment check

Control then candidate, four disk-backed loopback validators with the same broad CPU mask and two VM workers per replica. The [saturation setup](../saturation/README.md) supplied the identical 1,440,000-Cell prefill, 256 MiB RocksDB block cache, 131,072 verification entries, 100 ms proposal cadence and 4B cycles / 2,048-item limits. Both arms used the same genesis and 360,000 signed payments. The driver offered 4,000 TPS for 90 seconds, with a 30-second warmup, 60-second measurement and 45-second drain. Each arm started with fresh stores.

| Measurement | Control | Parallel Cell reads |
| --- | ---: | ---: |
| Socket writes/s | 4,000.00 | 4,000.95 |
| Common confirmations/s | 2,935.47 | 3,037.87 |
| Follower candidate preparation, mean | 365.86 ms | 338.80 ms |
| Leader build, mean | 208.47 ms | 194.85 ms |
| Durable commit, mean | 42.37 ms | 48.40 ms |
| Consensus control queue, mean | 49.03 ms | 52.44 ms |
| Validator process CPU | 319.19 s | 333.28 s |
| Nonempty blocks at the 2,048-item cap | 86/86 | 89/89 |
| Acknowledged and confirmed measurement cohort p99 | 83.96 s | 79.89 s |
| Peak acknowledged backlog during measurement | 92,764 | 86,959 |
| Acknowledged backlog at cutoff | 0 | 0 |

Follower time declined by 7.4%, common throughput gained 3.5% and process CPU increased 4.4%. The different volume of admitted and confirmed transactions affects aggregate CPU and commit time. The p99 cohorts cover 230,561 and 239,494 acknowledged writes sent in the measurement interval. Pending work grew by 54,486 and 57,222 during that interval. This is an overload experiment with a single live run order, and independent-host acceptance remains deferred.

Independent `drive.py verify` reconstructed both signed payment archives, checked the ordered transaction roots and receipt witnesses and authenticated the terminal weighted certificates. All 350,561 and 359,437 acknowledgments were accounted for at drain. The identical signed-workload identifier was `c1af7f1d7d5e3398b8eafad8b78f7347efb1788bbb7cbffe9fbbb120d9f2cc13`. The node binary SHA-256 values were `d950c34a6ed417f5794c09abd76e5b854dba6e76b42145fbffd5d2f184120967` and `ad9e113aa01f015c83640c2cafefcdcfdf6e116af5b21cd833e0f42fb5b26ed8`. Reproduce by building the control with `cargo build --release --locked --bin stem` and the candidate with the additional `--features native-candidate-cell-prefetch` flag. Prepare the 4,000/s plan with the saturation fixture's fixed prefill and change the offer to 90 seconds. Run and independently verify each arm with the same plan. [Driver and replay](../storage/external/README.md#measure-and-replay)

## Four-replica mixed Type Groups

The two arms also ran the established mixed fixture with zero to twelve Type Groups, 55,000 genesis-funded transfers, 2,200 TPS offered for 25 seconds and 20 seconds of drain. It used 64 ready senders with persistent connections and the same validator configuration as the payment pair. Both arms independently replayed the entire load and the terminal certificate. The signed-workload identifier was `85f80e6e8a2448944da69b7faaa3e662d6e8506c12749a0dc2ed4aa1ba7f652e`.

| Measurement | Control | Parallel Cell reads |
| --- | ---: | ---: |
| Offer-window common confirmations/s | 2,123.00 | 2,147.48 |
| Follower preparation, mean over offer and drain | 31.21 ms | 30.11 ms |
| Write-to-common p99, 55,000 confirmed payments per arm | 1.031 s | 0.948 s |
| Certified blocks independently replayed | 275 | 278 |

The smaller Follower change is consistent with the short, type-heavy blocks and less time available to hide behind Cell reads. A single arm per build leaves the throughput and p99 changes subject to normal live-run variance. Keep the feature opt-in. The full-block controls establish a preparation benefit for cold and partially cached state. A wider launch default needs repeat live results under varied contract workloads and shared-resource pressure. [Mixed fixture command](../critical-path/README.md#reproduction)
