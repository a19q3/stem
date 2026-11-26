# RocksDB compaction under signed payment load

This two-profile comparison ran four native Jolteon replicas, three with a 64-block retention window and one archive. Each payment created a 2 KiB output. The driver and its 32 submission threads used CPUs 0–3. Replica processes used CPUs 4–31 on the same 16-core host. The Btrfs filesystem spans two NVMe devices with zstd:1 mount compression. Host details are in host.json. The candidate patch sets an 8 MiB write buffer and a maximum of three buffers using the pinned wrapper's public options. Both profiles used the original atomic batches, WAL and synchronous writes.

| Planned TPS and profile | Common-confirmed after drain | Driver drops | Common p99 (s) | Process write B / payment / replica | Flush output (MB) | Compaction output (MB) | Compaction jobs with output |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 750 stock | 78,400 | 11,600 | 2.629 | 14,227 | 739.4 | 548.4 | 12 |
| 750 small | 79,406 | 10,594 | 2.176 | 24,620 | 956.8 | 3,741.0 | 88 |
| 200 stock | 17,759 | 241 | 1.872 | 14,422 | 90.3 | 0.0 | 0 |
| 200 small | 17,978 | 22 | 1.059 | 17,506 | 176.3 | 180.7 | 20 |
| 300 fixed stock | 27,000 | 0 | 1.516 | 14,008 | 187.0 | 67.0 | 4 |
| 300 fixed small | 27,000 | 0 | 2.114 | 18,682 | 282.6 | 494.9 | 36 |

The 750 TPS runs lasted 120 seconds. The 200 and 300 TPS runs lasted 90 seconds. At 750 and 200 the driver dropped jobs under its normal queue/deadline policy. The files named equal-small and equal-stock record the attempted 200 TPS comparison and its unequal realized counts. The fixed-volume 300 TPS pair instead applied backpressure at the existing bounded driver queue and allowed submission through the drain cutoff. Both finalized exactly the same 27,000 signed transaction IDs. Each pair used identical signed workload and genesis digests. Block boundaries still varied. Each run verified its common Header and finality proof and created a separate archive history index and proof.

**Decision.** Retain the stock defaults. At fixed volume, the small profile reduced the largest sampled per-process RSS from 189,508 to 134,304 KiB but increased process writes 33.4%. Compaction SST output rose from 67.0 to 494.9 MB over the life of the fresh databases. WAL bytes during the measured window were 975.5 and 976.3 MB. Mean finality batch sync time was 8.84 and 9.51 ms. Common-visible offer-window throughput was 297.23 and 296.73 TPS. The two-minute higher-pressure runs reinforce the compaction cost but carried different transaction counts.

The SST columns sum actual RocksDB LOG table-creation events for all four replicas, including initialization and shutdown. A job can create multiple files. WAL, logical batch and process-write counters use before/after deltas; the latter includes filesystem writes from the entire process. These numbers use different boundaries and cannot be added up as device wear or per-namespace write amplification. The archives include each ordinary/archive log separately, raw process samples, stage latencies and finality. Longer database aging, sustained overwrite churn, read p99, stall attribution, crash recovery and device-level writes remain open.

## Replay

Build the reference replica from `fefb8cc01771d59e228c72755ddd1242291f7f22` and build the candidate from a separate checkout at that commit with memtable.patch applied. Build each with `CXXFLAGS='-include cstdint' cargo build --locked --release --bin stem`. Build native test contracts following OPERATIONS.md. The test harness in this research branch accepts 600-second windows, retains its 100,000-job cap and optional signed-expiration guard, and takes a GNU time path. Use absolute executable paths.

```sh
CXXFLAGS='-include cstdint' cargo test --locked --release --test native_store --no-run
python3 research/storage/run_load.py --harness /path/to/native_store-test --binary /path/to/stock-stem --contracts /path/to/riscv-contracts --time /usr/bin/time --output /tmp/stock-fixed.json.xz --scratch /tmp/stem-load --rate 300 --seconds 90 --drain 24 --data-bytes 2048 --complete-volume
python3 research/storage/summarize.py research/storage/*.json.xz
```

Repeat with the other binary and a fresh output file. Omit `--complete-volume` and use 750 TPS for 120 seconds or 200 TPS for 90 seconds with a 16-second drain to reproduce the other pairs. The runner archives the public reports and checks that the archive can be read before its disposable database directory is released.
