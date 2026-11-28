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

## Script and TypeSet reuse across native archives

This read-only survey at `69a4fed` used the stopped archive replica from four earlier runs. The [2,048-item payment control](admission-order/README.md#transaction-count-limit-under-a-fixed-cycle-budget) and the [serial mixed Type Group control](../patricia/README.md#mixed-type-group-ablation-and-default) supplied ordinary and repeated-script cases. The [fixed-scheduler Lineage and hotspot AMM controls](../execution-pipeline/README.md#lineage-materialization-across-conflicts) supplied object-specific arguments. The heights below are the exact persisted archive tips after drain and shutdown.

| Workload | Tip | Live Cells | Canonical record bytes | Distinct Locks | Distinct Types / occurrences | Distinct TypeSets / occurrences | Distinct Type code identities |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Ordinary payment | 1,182 | 720,002 | 110,906,876 | 1 | 0 / 0 | 0 / 0 | 0 |
| Mixed Type Groups | 245 | 55,005 | 22,731,967 | 1 | 12 / 329,985 | 12 / 50,769 | 3 |
| Mixed Lineage | 297 | 9,671 | 1,596,767 | 3 | 65 / 65 | 65 / 65 | 2 |
| Hotspot AMM | 290 | 4,873 | 883,778 | 3 | 68 / 196 | 66 / 66 | 3 |

To reproduce, walk keys starting with `b's'` followed by the Cell namespace tag in each stopped archive's default RocksDB column family. Decode and re-encode each value with `record::decode_cell` and `record::encode_cell_into`, check its key against `record::cell_key`, and count exact Lock and Type descriptors separately from exact nonempty TypeSet bytes. Count code identities as the complete code hash and hash-type byte. Rebuild the Cell Patricia root from all key and canonical value-hash pairs starting at the empty root. Every value round-tripped byte for byte, all rebuilt roots matched, and the archive's two-chain proof at each tip independently authenticated the exact Header against the development roster and the genesis ID recomputed from its spec.

For an optimistic logical estimate, let `n` be the frequency of one descriptor and `b` its canonical size in bytes. Count `max(0, n × (b − 4) − b)` for repeated descriptors, allowing four bytes per reference and one dictionary copy. Ordinary payments could save 38,160,049 bytes from the one recurring Lock, or 34.4% of canonical record bytes. In the mixed fixture, whole TypeSets plus full Locks could save 16,441,347 bytes (72.3%). Individual full Types plus full Locks could save 15,124,161 bytes (66.5%). These alternatives overlap. The mixed generator deliberately repeats its twelve sets. Every TypeSet in both object workloads was unique. Interning only Type code identities has a small logical upside there, 1,823 bytes for mixed Lineage and 5,589 bytes for hotspot AMM. Two recurring complete Type descriptors accounted for all 4,344 bytes of the alternative full-Type estimate in hotspot AMM.

The estimates cover canonical Live Cell values only. RocksDB block compression, dictionary keys, indexes, reference updates, extra lookups and write amplification can substantially change the result. Compare compressed inline records, code identity interning, full Script interning and whole TypeSet interning on fresh stores with the same Cell records and a matched churn trace. Measure SST and WAL bytes separately and verify exact reconstruction after reopen before considering a writer change.

### Compressed layout and churn screen

Use the four authenticated snapshots above as the key and value corpus, in archived key order. Test five fresh default-column-family databases per snapshot, using inline Snappy, inline LZ4HC, or Snappy with code identities, whole Scripts, or whole TypeSets with a Script fallback. The pinned build supports Snappy and LZ4HC. Its Zstandard option is unavailable without changing the build. Run the layouts in forward and reverse order on the same host. Use the same RocksDB defaults and enable statistics for all layouts.

Select dictionary entries that occur at least eight times in the starting population. Assign IDs in sorted order by item kind and canonical bytes, encode references with a one-byte discriminator and four-byte little-endian ID, and keep unique entries inline. Code identities leave arguments inline. The TypeSet variant refers to complete canonical sets where eligible and otherwise uses the whole-Script representation. Store immutable dictionary values and eight-byte counts as separate records. Update net count changes in the same synchronous WAL batch as the Cell writes. This screen pins the starting dictionary through the run.

Seed in batches of 512, then flush and compact. For each of 256 rounds, pick `m = min(512, live_cells / 8)` keys. In round `w`, select indices `(i × live_cells / m + w) % live_cells` for `0 ≤ i < m`, with integer division. Restore the previous round's keys and delete the current round's keys in one batch. Restore the final cohort in its own batch, flush and compact, then close. This repeats actual Cell records and keys and preserves each workload's distribution. It models delete and insert I/O without simulating new asset issuance or the rest of a block commit.

Collect WAL bytes and syncs from RocksDB tickers and flush and compaction output from `table_file_creation` events by job. Measure live SST size after the final compaction. After reopen, walk all keys and resolve every reference, check persisted counts against the full walk, verify each canonical value against the source, and recompute the Cell root. Measure 20,000 point reads after 1,000 warm-up reads in each mode, using the same deterministic pseudorandom key sequence. Compare a resident dictionary loaded from the reopened database with dictionary lookups through warmed RocksDB. Reverse the read-mode order in the second run.

The fifth population isolates unique arguments at the mixed fixture's size. In archive key and canonical Script order, replace each Type Script's first four argument bytes with a globally unique increasing `u32` in little-endian order, starting at zero. Rebuild each Cell with `Cell::new` so its Type ordering is canonical. This preserves 329,985 Type occurrences and their lengths and code identities, and removes all full Type and TypeSet reuse. Its expected root is recomputed from the transformed source. It has no chain finality proof.

| Population | Inline Snappy SST / churn WAL (MB) | Inline LZ4HC change | Code identities change | Full Scripts change | Whole TypeSets change |
| --- | ---: | ---: | ---: | ---: | ---: |
| Ordinary payment | 88.29 / 29.91 | −1.1% / 0% | −3.9% / −12.7% | −5.4% / −23.2% | −5.4% / −23.2% |
| Mixed Type Groups | 9.57 / 63.85 | −10.7% / 0% | −14.1% / −40.2% | −24.4% / −55.0% | −31.5% / −61.1% |
| Mixed Lineage | 1.24 / 31.36 | −3.4% / 0% | −3.9% / −12.2% | −5.9% / −22.5% | −5.9% / −22.5% |
| Hotspot AMM | 0.69 / 33.44 | −3.7% / 0% | −4.0% / −11.8% | −5.5% / −21.4% | −5.5% / −21.4% |
| Unique-argument control | 10.07 / 63.85 | −4.7% / 0% | −10.2% / −40.2% | −4.1% / −9.5% | −4.1% / −9.3% |

All 50 stores passed reopen, byte comparison, reference accounting and root reconstruction. Size and WAL counts agreed across run orders, with 131,072 deletes and 131,072 restores in 257 WAL syncs per store. In ordinary payments, full Scripts lowered flush output from 16.49 to 15.63 MB and compaction output from 88.29 to 83.56 MB. In mixed Type Groups, whole TypeSets lowered flush output from 10.80 to 6.72 MB and compaction output from 9.57 to 6.56 MB. The compaction in each run materialized the final Live Cell keyspace. These figures describe just this column family's test records and its dictionaries.

In mixed Type Groups, direct dictionary reads made 140,345 additional gets per 20,000 Cells for individual Code and Script layouts and 38,433 for TypeSets. Direct p99 was 2.23 / 2.21 µs for inline, 7.91 / 6.22 µs for Code, 7.51 / 6.73 µs for Scripts and 2.67 / 3.24 µs for TypeSets. Resident TypeSet p99 was 1.84 / 1.54 µs against inline 3.34 / 2.00 µs. The starting population kept the dictionary small enough to preload. Synchronous batch p99 increased from 4.76 / 4.81 ms to 6.00 / 5.34 ms with TypeSets, despite the reduction in bytes. The ordinary-payment full-Script sync p99 moved in opposite directions across the two orders. This screen omits runtime encoding, cache growth, births of new descriptors, reclamation and finality scheduling.

The prior native archive's measured Live Cell operations supplied 164.16 MB of its 2,204.55 MB WAL for ordinary payments and 26.77 MB of 182.35 MB for mixed Type Groups. Applying the isolated churn ratios to those shares would reduce total WAL by approximately 1.7% for ordinary full Scripts and 9.0% for mixed whole TypeSets. These are workload-specific projections from the archive replica's `after.storage` minus `before.storage` counters. They exclude changes to other commit stages.

**Decision.** Keep inline Snappy as the native layout and decline all three dictionary layouts for the current production writer. Ordinary payment and both object controls showed small compressed SST gains. The repeated TypeSet fixture showed a substantial space benefit alongside higher sync p99 in both orders. Unique Type arguments removed that TypeSet advantage, and resolving code references through RocksDB raised point-read p99. Revisit #23 if a representative workload shows substantial repeated TypeSet traffic or sustained Live Cell I/O pressure. Any candidate then needs an integrated test of bounded cache and reference lifetimes, full-store churn, migration and crash recovery.

The [resident Lock and TypeSet candidate](live-cell-index.md#resident-lock-and-typeset-sharing) uses that reuse distribution to explore index memory and read cost while retaining inline durable storage.
