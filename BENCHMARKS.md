# Sidechain measurements, 2026-11-25

The archived runs predate the standalone Fern checkout. Their original command lines, executable hashes and source commits are recorded in the raw archives. The commands below use the current Fern package and tool names.

Runs used a single Linux workstation with an Intel Core i9-13900H, 20 logical CPUs, 31 GiB RAM and NVMe storage. All replicas, the load driver and the observers shared this host. Every sidechain replica used native CKB signature and script checks and synchronous RocksDB commits. The evidence includes actual per-transaction RPC admission and executed-block visibility at each replica, sampled CPU, RSS, P2P counters, write bytes, allocated files and application sync totals. The verifier independently checked at least one execution receipt per run.

## Release payments and workload coverage

Rates below use the load window. Completion counts include drain. Latency is measured from client dispatch to when the last replica was observed with a successful execution result.

| Workload | Replicas | Scheduled TPS | Offered TPS | Admitted TPS | Finalized TPS at node 0 | All-replica TPS | All-replica p50 / p95 / p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| CKB payment, 18 s | 4 | 256 | 256.0 | 255.9 | 245.4 | 245.4 | 737 / 1,013 / 1,094 ms |
| CKB payment, 18 s | 4 | 512 | 510.4 | 509.1 | 492.2 | 492.2 | 838 / 1,143 / 1,249 ms |
| CKB payment, 18 s | 8 | 512 | 274.6 | 273.3 | 264.9 | 257.7 | 989 / 1,443 / 1,618 ms |
| CKB payment, 18 s | 16 | 256 | 119.7 | 116.8 | 110.8 | 105.8 | 1,808 / 3,500 / 4,221 ms |
| CKB payment, 18 s | 32 | 128 | 54.2 | 49.9 | 37.8 | 34.2 | 5,559 / 8,525 / 9,498 ms |
| CKB payment, 18 s, batched RPC | 4 | 1,024 | 605.8 | 595.1 | 574.6 | 574.6 | 942 / 1,337 / 1,505 ms |
| CKB payment, 24 s, parallel admission | 4 | 1,024 | 679.7 | 658.3 | 649.5 | 649.5 | 1,280 / 1,938 / 2,203 ms |
| CKB payment, 40 s, batched RPC | 4 | 550 | 523.6 | 518.8 | 513.6 | 513.6 | 905 / 1,329 / 1,560 ms |
| CKB payment, 90 s, sustained | 4 | 256 | 256.0 | 255.9 | 253.2 | 253.2 | 777 / 1,067 / 1,154 ms |
| CKB payment, 120 s, high load | 4 | 450 | 346.4 | 297.9 | 279.1 | 279.1 | 4,515 / 15,751 / 20,465 ms |
| xUDT, 15 s | 4 | 256 | 256.0 | 255.5 | 248.5 | 240.1 | 749 / 1,053 / 1,124 ms |
| Mixed CKB and xUDT, 15 s | 4 | 256 | 256.0 | 254.9 | 245.9 | 245.9 | 740 / 1,023 / 1,138 ms |
| Two xUDT assets in one atomic transaction, 15 s | 4 | 256 | 256.0 | 254.9 | 247.5 | 236.3 | 774 / 1,082 / 1,201 ms |
| Four competing spends per Cell, burst load with 20 ms one-way P2P delay | 4 | 192 | 192.0 | 192.0 | 46.0 | 46.0 | 978 / 1,267 / 1,400 ms |

The 40-second run completed all 21,696 offered payments across all replicas. The higher 18-second batched run completed all 11,664 offered payments and dropped 6,768 scheduled submissions at the bounded client queue. The parallel-admission run completed all 18,360 offered payments and dropped 6,216 scheduled submissions. The larger committees also showed client drops and admission errors. These observations measure the complete same-host pipeline under backpressure. The 90-second run completed all 23,040 offers. At 30, 60 and 90 seconds there were 143, 282, and 240 admitted payments awaiting all-replica visibility, and the backlog drained. In the 120-second high-load test the configured 4,096-entry mempool rejected 5,442 submissions and the bounded driver dropped 10,896 scheduled requests. There were 2,256 admitted transactions waiting for all-replica visibility at the end of the load window, and 37,600 of the 37,662 admitted payments completed during the run and drain. The shorter peaks therefore do not represent sustained throughput. The network-only saturation point remains open.

The typed runs executed the real xUDT RISC-V contract, using the fixture SHA-256 `e9b92e5783f692f6ee99ca20eeda5f3da282e0f4010eb4fbd3db4e3058239349`. Fiber records its source as CKB Production Scripts commit `410b16c499a8888781d9ab03160eeef93182d8e6`. The contended run executed exactly 576 of the 2,304 competing spends, one per funded Cell. [Fiber fixture provenance](https://github.com/nervosnetwork/fiber/tree/4e899912b17ea4859cdb86067e97b2d19ef48f7b/tests/deploy/contracts), [xUDT source](https://github.com/nervosnetwork/ckb-production-scripts/tree/410b16c499a8888781d9ab03160eeef93182d8e6).

The four-validator SIGKILL run finalized all 1,920 payments. The fourth replica was stopped at 3.0 s, restarted at 7.06 s and replayed the missing transactions. Last-replica p99 was 7.69 s including the outage. During a separate 12-second idle phase, each replica produced one heartbeat block, the app hash stayed stable and each process pair used about 0.6 CPU-seconds. Reducing the RocksDB memtable brought observed WAL allocation down from about 70.4 MiB to 17.6 MiB per replica. The parallel-admission run measured 5.28 MB of logical live-cell storage and 11.42 MB of witnessed transaction archive on its first replica.

## Six-minute RocksDB comparison

Both runs used four replicas, two VM workers per replica, a 200 ms commit delay, a 20-second idle phase and 63,360 scheduled CKB payments over six minutes. The baseline used `c83be3b39`. The second release, `8ba40cb51`, used a 64 MiB decoded-block cache per replica and RocksDB Ribbon filters for negative archive lookups.

| RocksDB configuration | Offered TPS | Admitted TPS | All-replica TPS | Completed / offered | All-replica p50 / p95 / p99 |
| --- | ---: | ---: | ---: | ---: | --- |
| Default table settings | 167.4 | 95.0 | 87.0 | 35,479 / 60,256 | 29.08 / 47.26 / 52.89 s |
| Bounded cache and filters | 176.0 | 176.0 | 175.6 | 63,360 / 63,360 | 0.79 / 1.11 / 1.21 s |

The baseline recorded 24,447 mempool-full replies, 1,624 RPC deadlines and 3,104 client queue drops. All replicas observed 1,576 payments for which the client received no successful admission acknowledgment. The run therefore reports the acknowledgment and execution counts independently. The cached run had 129 acknowledged payments awaiting all-replica visibility when the load window ended, and drained them all.

The first replica recorded 77 proposal timeouts and 39 nonempty commits in the baseline. The cached run recorded one proposal timeout and 629 nonempty commits. An eight-second sample of user-space CPU cycles on that replica near the end attributed about 89% of baseline sampled cycles to Snappy decompression. CKB-VM execution and decoding appeared among the largest sampled costs after the change. Total CPU time over load and drain fell from 1,902 to 621 CPU-seconds. Peak sampled RSS stayed near 407 MiB per replica. The four replicas sent 380 MB and 398 MB of CometBFT P2P traffic respectively.

Observed process writes rose from 948 MB to 3,325 MB as the run committed more transactions and many more blocks. The first replica's allocated storage rose from 118.8 MiB to 122.5 MiB, and its p99 application sync-write latency on nonempty blocks fell from 13.2 ms to 9.6 ms. The [raw comparison](benchmarks/2026-11-25-cache-sweep.json.xz) includes per-payment admissions, per-replica visibility, resource samples, CPU profiles, commit counts, public committee inputs and execution proofs.

```sh
python3 tools/localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --replicas 4 --rate 176 --seconds 360 --drain 30 --idle-seconds 20 \
  --output target/fern-runs/cache-comparison
```

## Cached release under higher load and network faults

| Replicas and load | Offered TPS | Admitted TPS | All-replica TPS | Completed / offered | All-replica p99 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Four, 512 scheduled TPS for 120 s | 512.0 | 511.4 | 509.5 | 61,440 / 61,440 | 1.40 s |
| Four, 768 scheduled TPS for 80 s, 24 client workers | 548.8 | 546.4 | 541.5 | 44,664 / 44,664 | 1.66 s |
| Four, 768 scheduled TPS for 80 s, 64 client workers | 557.1 | 550.7 | 548.3 | 46,592 / 46,592 | 2.45 s |
| Eight, 256 scheduled TPS for 35 s, 40 ms one-way delay and one restart | 256.0 | 255.3 | 232.9 | 8,960 / 8,960 | 11.10 s |

The bounded client queue dropped 16,776 and 14,848 requests at the two 768-TPS schedules. Raising client concurrency increased actual offered throughput slightly and raised p99. All offered requests reached all replicas after drain. The measurements bound the complete single-host submission and execution path at these settings. They leave the isolated consensus and execution limit undetermined.

The delayed run forwarded real P2P streams through loopback proxies while preserving TCP ordering. The eighth validator pair was killed at 8.00 s and restarted at 15.14 s with the original signing state. The first replica's p99 stayed at 3.78 s and the all-replica figure includes the recovering process. These runs share one workstation. The [higher-load and fault observations](benchmarks/2026-11-25-cache-load.json.xz) include each run's arguments, transaction records, resource readings and verifiable receipt.

## Cost of maintaining the wallet Cell index

Both runs used the same release from `1d1510195109d30abe18340e833cc76a7ddc7a34` with SHA-256 `12c72c180e6d175220da370b7ae121222c189164f51681d02bf7cdf4edd5b359`. They ran four replicas on one workstation with 512 scheduled payments per second for 60 seconds and a 10-second idle phase. The index was enabled only on replica 0 in the second run. The gateway served no wallet traffic during either run.

| Wallet index | Offered TPS | Admitted TPS | All-replica TPS | Completed / offered | All-replica p50 / p95 / p99 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Off | 497.1 | 493.9 | 490.2 | 30,576 / 30,576 | 0.95 / 1.43 / 1.66 s |
| One replica | 496.5 | 493.3 | 487.0 | 30,552 / 30,552 | 0.97 / 1.43 / 1.62 s |

The indexed replica held 30,840 index records and 3.26 MB of logical index data at the end of the run. Replica 0's allocated application WAL measured 18.46 MB without the index and 57.19 MB with it. Aggregate process writes measured 820.35 MB and 825.54 MB, and aggregate CPU time was 399.22 and 418.99 seconds. The client dropped 144 and 168 scheduled submissions before sending them. This single pair records the observed write and storage costs and does not establish a repeatable latency difference. The [raw index comparison](benchmarks/2026-11-25-rpc-index.json.xz) includes parameters, per-payment records, resources and execution proofs under `plain` and `indexed`.

```sh
python3 tools/localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --replicas 4 --rate 512 --seconds 60 --drain 20 --idle-seconds 10 \
  --rpc-index-replica 0 --output target/fern-runs/rpc-index-cost
```

Run again with a new output directory and omit `--rpc-index-replica 0` for the baseline.

## Live Cell checkpoint proofs

Two runs used release `36d8a6af63a261b91b5688a5f2207a9c4164d46d` with SHA-256 `b6d862ba105b5ef92d3d079f6b10abb16d5e3c6c76fb2ccee4693963169d777e`. Both ran four replicas on one workstation, with 512 scheduled payments per second for 60 seconds, a 10-second idle phase and a checkpoint every 24 blocks. The second run enabled checkpoint format 2 with Live Cell proofs.

| Checkpoint format | Offered TPS | Admitted TPS | Finalized TPS at node 0 | All-replica TPS | Completed / offered | All-replica p50 / p95 / p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Format 1 | 492.7 | 489.5 | 486.7 | 482.8 | 30,304 / 30,304 | 0.97 / 1.55 / 1.94 s |
| Format 2 | 511.3 | 508.1 | 499.6 | 499.6 | 30,720 / 30,720 | 0.97 / 1.49 / 1.77 s |

The client dropped 416 scheduled submissions in the first run and zero in the second. Total process writes were 1,095.75 MB and 1,140.89 MB. Format 2's two retained proof indexes on replica 0 each occupied 34,824 logical bytes and 36,864 allocated bytes. The final format-2 archive on replica 0 was 22.13 MB. Across 16 captures per format, median and maximum capture times were 82.2 and 98.2 ms for format 1 and 79.2 and 133.0 ms for format 2. The capture clock covers the archive scan and root computation. This pair records two realized load levels. The [raw checkpoint comparison](benchmarks/2026-11-25-cell-checkpoints.json.xz) has timings, resource samples, retained file hashes, per-payment observations and execution proofs.

```sh
python3 tools/localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --replicas 4 --rate 512 --seconds 60 --drain 25 --idle-seconds 10 \
  --checkpoint-interval 24 --checkpoint-cell-proofs \
  --output target/fern-runs/checkpoint-proof-cost
```

Run with a new output directory and omit `--checkpoint-cell-proofs` to measure format 1.

## Compact transaction checkpoints

The matched runs used release `1de49cef61a16baa3f1eb003ee849b8fc067e657` with SHA-256 `9ff0f28b56eec50e03a939fae3c25c5252605d7a7e54ac58596be70845929fdf`. Each run offered 450 signed payments per second for 90 seconds to four same-host validators. The validators retained their local transaction archives in both runs. A fresh fifth replica performed P2P state sync after the load window using checkpoint height 168.

| Checkpoint format | Image size | Transfer and installation | Startup through catch-up | All-replica TPS | Payment p99 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Format 2 | 30.47 MB | 3.394 s | 12.608 s | 446.0 | 1.48 s |
| Format 3 | 12.31 MB | 1.427 s | 11.608 s | 445.0 | 1.49 s |

All 40,500 offered payments completed in each run. The checkpoint covered 39,130 payments in the format-2 run and 39,024 in the format-3 run, following on-chain funding. The format-3 snapshot was 59.6% smaller. The observer received 31.46 MB and 13.37 MB of P2P traffic, including block catch-up, and its processes wrote 101.97 MB and 46.21 MB. On replica 0, preparing the height-168 checkpoint took 129.72 ms and 85.63 ms. Each archive validator retained 22.37 MB of logical witnessed transaction data, and format 3 added 2.63 MB of logical replay records. Both restored observers could find the original CKB transaction's committed header. The format-2 observer also returned the witnessed transaction bytes.

In the initial runs, the format-3 observer was offered height 168 before height 170 was available. It restored height 144 and replayed 24 additional blocks to reach height 168. The matched runs use the fix that waits for both successor blocks before advertising a checkpoint. The [raw observations](benchmarks/2026-11-25-compact-snapshots.json.xz) contain both pairs under `initial` and `matched`, with per-payment receipts, observer state-sync logs, storage statistics and independently verified execution proofs.

```sh
python3 tools/localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --replicas 4 --rate 450 --seconds 90 --drain 25 --idle-interval 2s \
  --idle-seconds 8 --admission-workers 3 --checkpoint-interval 24 \
  --checkpoint-cell-proofs --checkpoint-compact-transactions --prefund-payments \
  --sync-observer --sync-checkpoint-height 168 --output target/fern-runs/compact-v3
```

Run with a new output directory and omit `--checkpoint-compact-transactions` to measure format 2.

### Removing the permanent ordinary replay index

Both runs used source commit `344f78e49c2f34fe204e1df4a2c9ba3dd54d77a4` and the same release binary with SHA-256 `068c8016b2881591f98bc305c01a511960d792667fb6b69c815f979d44e95962`. Four validators used `--compact-transaction-history` and on-chain funding. Each run offered 450 signed payments per second for 90 seconds, and all 40,500 completed on all replicas. A fifth replica restored checkpoint height 168 over P2P.

| Format | Height-168 image | Transfer and installation | Startup to CometBFT readiness | Admitted TPS | Finalized and all-replica TPS | All-replica p50 / p95 / p99 |
| --- | ---: | ---: | ---: | ---: | ---: | --- |
| 3 | 12.31 MB | 1.435 s | 11.610 s | 449.87 | 445.18 | 0.79 / 1.17 / 1.50 s |
| 4 | 9.46 MB | 1.198 s | 7.591 s | 449.78 | 444.67 | 0.82 / 1.19 / 1.54 s |

Format 4 cut the checkpoint image by 23.2%. From height 24 to 168, the images grew by 2,660,060 bytes and 35,856 bytes respectively. The format-4 Live Cell payload size stayed constant after funding, and its growth over this interval came from 144 CKB headers. The observers received 13.34 MB and 9.83 MB over P2P and wrote 46.06 MB and 33.59 MB. Startup includes peer discovery and readiness checking. The observers reported application heights 174 and 168 immediately after readiness, so the startup figures measure different amounts of block catch-up. The transfer and installation figures measure the same checkpoint height.

Replica 0 held 40,520 permanent ordinary replay records occupying 2.63 MB in format 3. Format 4 held 32,693 ordinary statuses and 121 expiry buckets totaling 3.17 MB of logical local records inside its 128-block window. Both replicas held 40,503 Live Cells. The four validators wrote 1.50 GB and 1.43 GB over the respective load and drain windows. The [raw comparison](benchmarks/2026-11-25-live-replay.json.xz) contains `v3` and `v4` run parameters, per-payment records, storage, resource samples, checkpoint logs, and independently verified execution proofs.

```sh
python3 tools/localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --replicas 4 --rate 450 --seconds 90 --drain 25 --idle-interval 2s \
  --idle-seconds 8 --admission-workers 3 --checkpoint-interval 24 \
  --checkpoint-cell-proofs --checkpoint-compact-transactions --compact-transaction-history \
  --prefund-payments --sync-observer --sync-checkpoint-height 168 \
  --checkpoint-live-cell-replay --output target/fern-runs/live-replay-v4
```

To measure format 3, omit `--checkpoint-live-cell-replay` and use a new output directory.

### Compact validators without the CometBFT transaction index

Source commit `eb7f524870a732fe01656fb391dc27a095e25b06` ran the same format-4 release in both profiles. Four compact validators shared one host, retained 96 consensus blocks, and funded the payment Cells on chain. Each run offered 450 signed payments per second for 90 seconds and completed all 40,500 on all four validators.

| CometBFT index | Admitted TPS | All-replica TPS | All-replica p99 | Allocated index per replica | Four-replica process writes |
| --- | ---: | ---: | ---: | ---: | ---: |
| `kv` | 449.69 | 444.78 | 1.52 s | 11.34 MB | 1.48 GB |
| `null` | 449.69 | 442.90 | 1.48 s | 0 | 1.24 GB |

Both validator sets had pruned to base height 76 at the end of load and drain. During load, `/tx` on the `kv` replica returned the witnessed transaction from block 2 after that block body became unavailable. The `null` replica disabled the lookup and created no index database. Total process writes were 16.5% lower in the `null` run. Allocated consensus WAL on replica 0 was 49.95 MB and 39.91 MB respectively, and both WALs rotated under load. Replica 0's p99 synchronous application write time for nonempty blocks was 12.38 ms and 12.89 ms.

Each fresh observer installed a 9,456,197-byte image at height 168 and applied through height 170. Startup through application replay took 8.09 s with `kv` and 8.31 s with `null`, including transfer and installation times of 1.22 s and 1.29 s. Each run verified a weighted execution proof. With `null`, the exporter reconstructed the proof from the retained block and execution results. The [raw observations](benchmarks/2026-11-25-comet-index.json.xz) include the arguments, receipts, resource samples, WAL and application sync measurements, archive lookup checks, and proofs.

A separate `null` run offered 8,000 payments in 20 seconds and killed replica 3 at 5.00 s. It restarted against the same signing state at 11.07 s. All 8,000 payments reached all four replicas, which agreed on the application hash at height 30. Last-replica p99 was 10.80 s including the outage. This run appears under `recovery` in the same archive.

```sh
python3 tools/localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --replicas 4 --rate 450 --seconds 90 --drain 25 --idle-interval 2s \
  --idle-seconds 8 --admission-workers 3 --checkpoint-interval 24 \
  --checkpoint-cell-proofs --checkpoint-compact-transactions --checkpoint-live-cell-replay \
  --compact-transaction-history --retain-consensus-blocks 96 --prefund-payments \
  --sync-observer --sync-checkpoint-height 168 --no-comet-tx-index \
  --output target/fern-runs/comet-index-null
```

For the `kv` comparison, omit `--no-comet-tx-index` and use a new output directory.

## Four KVM guests on a shared host

Source `595c2e92eb988c20dbe51f8b814482f5553ede70` ran four Linux guests with three vCPUs and 2 GiB of configured RAM each. The guests used separate kernels, validator keys and ext4 images on the same physical workstation. Each guest established authenticated P2P sessions with the other three over a private virtual Ethernet network. The load driver ran on the host and used four forwarded RPC endpoints. The delayed run applied Linux `netem` on each guest's P2P egress and added 20 ms in each direction of a peer round trip.

Both runs used format-4 checkpoints and funded the workload on chain. Each offered 300 signed payments per second for 90 seconds and completed all 27,000 on all four replicas.

| Added one-way P2P delay | Admitted TPS | Finalized TPS | All-replica TPS | All-replica p50 / p95 / p99 |
| --- | ---: | ---: | ---: | --- |
| 0 ms | 299.91 | 297.37 | 297.37 | 0.84 / 1.20 / 1.54 s |
| 20 ms | 299.91 | 295.64 | 295.64 | 1.01 / 1.42 / 1.78 s |

The four QEMU processes used 440.38 and 448.64 CPU-seconds respectively, and their combined peak sampled RSS was 2.31 GB and 2.28 GB. Their process writes were 2.21 GB and 1.87 GB, and the allocated guest images grew by 267 MB and 273 MB. CometBFT reported 164 MB and 168 MB of transmitted P2P traffic. The first replica's p99 synchronous application write latency for nonempty blocks was 18.38 ms and 21.53 ms. Samples include the guest kernels' resource use, and the raw file records the sample boundaries.

The [raw network observations](benchmarks/2026-11-25-kvm-network.json.xz) include the topology, authenticated peer IDs, measured queue depths, guest settings, application storage and write timings, and signed execution proofs. The `runs` entries each contain an `execution_proof` and public `committee` for offline verification. A separate HTTPS check observed all 160 payments through a private path prefix, including eight whose batch response was lost after submission. The archive also records preflight rejections and the preservation of all transaction observations when a post-run telemetry request failed.

```sh
python3 tools/vm_localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --rate 300 --seconds 90 --vm-cpus 3 --vm-memory-mib 2048 \
  --p2p-delay-ms 20 --output target/fern-runs/kvm-network-delay
```

For the zero-delay profile, omit `--p2p-delay-ms` and use a new output directory.

## Six-minute payment run with on-chain funding

Release `9c95b7f6e70b17f802a5544b14545cec1105f0e1` used CometBFT 0.38.26, four same-host validators, two proposal VM workers and three admission workers per validator. The release executable SHA-256 was `b8cec1324b2936c77cb5d08c428be2e647fdfb66d4a20921d5c02986fd951955`. The app produced format-2 checkpoints every 120 blocks. Before the load window, all replicas executed 90 signed split transactions to create 184,320 payment inputs in 12.23 seconds. The fixture used 2.2 MB of application genesis data.

| Scheduled TPS | Offered TPS | Admitted TPS | All-replica TPS | Completed / offered | All-replica p50 / p95 / p99 |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 512 | 512.0 | 511.9 | 510.1 | 184,320 / 184,320 | 0.82 / 1.16 / 1.46 s |

The client queue dropped zero transactions. Admitted payments waiting for all-replica visibility numbered 297, 484, 454, 427, 494 and 648 at minute boundaries. The maximum across one-second samples was 853. All payments were visible 1.09 seconds after the load window. The four replicas agreed on the final state and used 2,392.5 CPU-seconds, sent 1.10 GB of P2P data and recorded 8.71 GB of aggregate process writes. Peak replica RSS ranged from 319 to 332 MB.

Replica 0 held 32.44 MB of logical Live Cell data and a 97.91 MB witnessed transaction archive. Its allocated application WAL was 18.46 MB and its allocated consensus WAL was 175.86 MB. Its largest retained application snapshot was 126.61 MB with a 207,624-byte proof index. The capture took 512.54 ms and application sync-write p99 across nonempty commits was 12.67 ms.

Two 52-second diagnostic runs at 1,024 scheduled TPS used the earlier `36c37b832` release. The default configuration with two workers per pool finalized 515.6 TPS across all replicas and used 371.35 CPU-seconds. Four workers per pool reached 522.4 TPS and used 475.05 CPU-seconds. Both dropped roughly 25,000 client requests before submission. A 10-second CPU capture in the two-worker run attributed 96.3% of the sampled performance-core cycles across replica 0's application and CometBFT processes to the admission threads. The [raw observations](benchmarks/2026-11-25-prefunded-512.json.xz) include the profiles, per-transaction records, funding confirmations, snapshots and proofs.

```sh
go build -o target/fern-tools/fern-loadgen tools/loadgen/main.go
python3 tools/localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --replicas 4 --rate 512 --seconds 360 --drain 35 --idle-seconds 15 \
  --vm-workers 2 --admission-workers 3 --checkpoint-interval 120 \
  --checkpoint-cell-proofs --prefund-payments --output target/fern-runs/prefund-512
```

## Assembly VM allocation reuse

Four validators with voting powers 2, 2, 1 and 1 ran on the workstation with three admission workers and two proposal VM workers each. The 75-second saturation runs scheduled 63,750 signed CKB payments at 850 TPS. The bounded RPC driver records requests dropped before dispatch. Each run prefunded its inputs with 32 signed split transactions, took format-4 checkpoints every 24 blocks, compacted ordinary transaction history and retained 96 consensus blocks. Each replica ran the actual CKB secp256k1 lock in RISC-V and persisted execution synchronously.

| VM allocation | Offered TPS | Admitted TPS | Finalized TPS at node 0 | All-replica TPS | Completed / offered | All-replica p50 / p95 / p99 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| Fresh core, first run | 577.8 | 575.3 | 565.9 | 565.9 | 44,078 / 44,078 | 987 / 1,493 / 1,771 ms |
| Fresh core, repeat | 548.7 | 546.1 | 540.1 | 540.1 | 41,910 / 41,910 | 1,013 / 1,497 / 1,740 ms |
| Reused core with reference wrapper | 557.0 | 554.5 | 548.8 | 548.8 | 42,534 / 42,534 | 1,001 / 1,516 / 1,841 ms |
| Reused core with inlined references | 588.3 | 585.7 | 582.0 | 578.2 | 44,878 / 44,878 | 994 / 1,513 / 1,844 ms |
| Reuse at VM startup and teardown | 622.4 | 619.8 | 611.9 | 611.9 | 47,448 / 47,448 | 986 / 1,491 / 1,810 ms |
| Sidechain feature, saturation repeat | 625.6 | 623.0 | 618.1 | 618.1 | 47,678 / 47,678 | 963 / 1,501 / 1,863 ms |
| Sidechain feature, 575 scheduled TPS for 180 s | 575.0 | 574.8 | 571.5 | 571.5 | 103,500 / 103,500 | 853 / 1,251 / 1,596 ms |

The baseline profile attributed 94.2% of sampled performance-core user cycles on replica 0 to the admission threads. For the sidechain feature, the share in `__memset_avx2_unaligned_erms` fell from 14.35% to 5.75%. The new allocation and reset routine accounted for 3.03%. Each completed payment used 12.88 ms of aggregate validator and application CPU time in the saturation repeat, compared with 14.04 and 14.70 ms in the fresh-core runs. The feature increased all-replica throughput by 9.2% over the faster fresh-core run. The RPC driver dropped 16,072 scheduled submissions in the feature's saturation run and 19,672 and 21,840 in the baseline runs. These measurements describe same-host saturation with bounded client concurrency.

The fixed-rate run confirmed all 51 funding splits on every replica before offering payments. The client dropped zero payments. At the 30-second marks, 268, 510, 396, 548, 355 and 600 acknowledged payments awaited visibility across all replicas. The largest one-second sample was 860 and the backlog drained within two seconds after the load window. The node processes used 1,318.66 CPU-seconds, sent 609.72 MB of CometBFT P2P traffic and recorded 3.45 GB of process writes. Peak sampled replica RSS ranged from 232 to 265 MB. Replica 0's p99 synchronous application write on nonempty blocks was 10.62 ms. Its allocated application WAL, snapshots and consensus WAL were 18.46 MB, 41.68 MB and 103.99 MB. The first fixed-rate launch collided with local RPC ports before load. The successful run used base port 29420.

Release `d016e6fb2eaa4dc1ec803cca528e99347dd5ec31` enables a feature that keeps one spare CKB-VM allocation per sidechain worker. A new VM resets guest registers, cycles, memory flags and decoded trace validity before running. The default CKB assembly core, scheduler, syscalls and cycle model execute every script. Standalone L1 builds use the default `ckb-script` feature set. Tests compare fresh and reused core execution in VM versions 0, 1 and 2, including cycle exhaustion, reuse at the same virtual address and `spawn` with `exec` under the V2 scheduler.

On the same release, four validators executed all 1,536 signed transactions in a mixed CKB and xUDT run. A separate Spore V2 SDK run verified execution receipts for mint, transfer and melt and rejected a transfer that changed content. The CellScript `data2` Lock rejected the incorrect witness and executed the corrected spend in 6,952 cycles on all four replicas.

```sh
python3 tools/localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --replicas 4 --base-port 29420 --rate 850 --seconds 75 --drain 25 --idle-seconds 6 --idle-interval 2s \
  --vm-workers 2 --admission-workers 3 --checkpoint-interval 24 \
  --checkpoint-cell-proofs --checkpoint-compact-transactions --checkpoint-live-cell-replay \
  --compact-transaction-history --no-comet-tx-index --retain-consensus-blocks 96 \
  --prefund-payments --output target/fern-runs/vm-reuse-saturation
```

Replace the rate, duration and drain with `--rate 575 --seconds 180 --drain 30` for the fixed-rate test. Use a new output directory for each run. Rebuild from `142ae0d701429ab26b2f12cb346c85bc98f0b290` in a separate checkout for the fresh-core control. To sample replica 0, run `perf record -e cycles:u -F 99 -p "$APP_PID,$COMET_PID" -- sleep 18` during the load window. The evidence includes original admissions, resource samples, profiles, binary hashes, commit IDs, verified receipts and their public committee configurations. Each payment's inclusion record stores the common height, CKB transaction hash and cycles, with one observation time per replica in replica order. [Raw observations](benchmarks/2026-11-25-vm-reuse.json.xz)

## Admission workers and HTTP batch size

The VM reuse release `d016e6fb2` ran four same-host validators for 45 seconds with 1,000 scheduled TPS and 64 client workers. The workload used the same on-chain prefunding, format-4 checkpoints, compact history and proof verification as the VM allocation comparison. CometBFT 0.38.26 processes the synchronous admission calls in an HTTP batch in order and returns a JSON object for a one-response batch. The original driver expected an array and reported a decode error for every dispatched request in the initial single-request run. The run was excluded from throughput comparisons. Driver commit `3b9ce4b7f` accepts both response shapes.

| HTTP requests per batch | Admission workers per replica | Offered TPS | Admitted TPS | Finalized and all-replica TPS | Completed / offered | All-replica p50 / p95 / p99 | Node CPU seconds |
| ---: | ---: | ---: | ---: | ---: | ---: | --- | ---: |
| 8, initial control | 3 | 661.7 | 650.3 | 643.8 | 31,824 / 31,824 | 1,300 / 2,016 / 2,361 ms | 420.95 |
| 1 | 3 | 638.6 | 637.2 | 624.2 | 28,992 / 28,992 | 1,024 / 1,433 / 1,823 ms | 391.81 |
| 1 | 4 | 596.1 | 594.7 | 586.5 | 27,079 / 27,079 | 1,061 / 1,460 / 1,753 ms | 440.53 |
| 8, updated driver | 3 | 612.4 | 601.1 | 592.4 | 29,600 / 29,600 | 1,368 / 2,086 / 2,424 ms | 424.31 |

The driver dropped 13,176 to 17,921 of the 45,000 scheduled requests before dispatch. A fourth admission worker used more CPU and finalized fewer transactions in the single-request runs. The two eight-request controls differed by 51.5 TPS. These runs leave the sustained limit for this configuration open. The archive includes both controls, all per-replica observations, and the failed driver's original reply classifications. [Raw ingress measurements](benchmarks/2026-11-25-ingress-sweep.json.xz)

Build the driver from `3b9ce4b7f` or a later revision. Reuse the VM saturation command with `--rate 1000 --seconds 45 --drain 22 --idle-seconds 5 --client-workers 64` and vary `--rpc-batch` and `--admission-workers`. Use a fresh development network for each run.

## Reading across ABCI Flush requests

The control (`3346708da`) waited for the ordered writer to finish prior CheckTx replies before reading beyond each Flush. Release `d7c506190` enqueues the Flush reply on the same writer and lets the reader queue later checks. Responses still leave the socket in request order. A later stateful request still waits for prior checks before dispatch. CometBFT 0.38.26 drains the mempool ABCI connection before issuing Commit. Both binaries report the time from decoding each Flush to servicing its response on the writer and the time to acquire the application Commit write lock.

The four saturation runs alternated control and release on the same host. Each had four weighted validators, two proposal workers and three admission workers per validator. The driver used 64 workers, singleton RPC calls and a 950 TPS schedule for 60 seconds. Each workload used real on-chain funding, format-4 checkpoints every 24 blocks, compact transaction history, disabled CometBFT transaction indexing and 96 retained consensus blocks.

| ABCI reader | Offered TPS | Admitted TPS | All-replica TPS | Completed / offered | All-replica p50 / p95 / p99 | Node CPU seconds |
| --- | ---: | ---: | ---: | ---: | --- | --- | ---: |
| Wait at Flush | 612.1 | 611.0 | 602.8 | 36,980 / 36,980 | 1,028 / 1,434 / 1,781 ms | 524.63 |
| Queue across Flush | 707.9 | 706.9 | 696.9 | 42,732 / 42,732 | 1,062 / 1,596 / 1,969 ms | 805.41 |
| Wait at Flush, repeat | 596.4 | 595.4 | 587.0 | 36,037 / 36,037 | 1,037 / 1,432 / 1,804 ms | 524.18 |
| Queue across Flush, repeat | 702.1 | 701.0 | 691.5 | 42,380 / 42,380 | 1,079 / 1,554 / 1,869 ms | 814.66 |

Release throughput exceeded the faster control by at least 14.7% in these runs. CPU cost per completed payment rose from 14.19–14.55 ms to 18.85–19.22 ms across the four app and CometBFT pairs. Mean writer wait per decoded Flush rose from 3.37–3.48 ms to 56.29–61.52 ms and the largest measured wait was 274.58 ms. The application Commit write-lock wait remained at or below 12 µs in all four saturation runs. The client dropped 20,020 and 20,963 scheduled requests in the controls and 14,268 and 14,620 in the release runs before dispatch. Finality p99 rose with the increase in work admitted by the replicas.

At 650 scheduled TPS for two minutes the driver dropped 402 scheduled requests. All 77,598 offered payments reached all four replicas. The load window measured 646.7 offered TPS, 646.3 admitted TPS and 640.8 all-replica TPS, with 1.72 s p99. The largest sampled backlog of acknowledged payments was 1,012 and drained within two seconds of the end of the window. In a 30-second process sample during this run, admission workers accounted for 334.18 of the 413.20 CPU-seconds across the four app and CometBFT pairs.

At 625 scheduled TPS for two minutes the client offered all 75,000 payments and every payment reached all four replicas. During load the system admitted 624.7 TPS and finalized 619.2 TPS across all replicas. Confirmation p50, p95 and p99 were 0.85, 1.32 and 1.66 seconds. The backlog at 30-second intervals was 486, 480, 484 and 651 acknowledged payments, and drained within two seconds after load. The four process pairs used 1,239.67 CPU-seconds, sent 443.13 MB of CometBFT P2P traffic and recorded 2.09 GB of process writes. Peak sampled replica RSS ranged from 233 to 240 MB. Replica 0's p99 synchronous write on nonempty blocks was 20.46 ms. It allocated 18.46 MB for the application WAL, 31.40 MB for snapshots and 76.85 MB for the consensus WAL.

The release confirmed all 9,600 payments in the 320 TPS run with 20 ms one-way loopback P2P delay and a four-second crash of the weight-1 replica. Of the 1,311 payments offered while that replica was down, 928 became visible at replica 0 before the restart. The restarted replica showed the last of those 1,311 payments 5.48 seconds after restart. All-replica p99 including the outage was 8.04 seconds.

```sh
python3 tools/localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --replicas 4 --base-port 29320 --rate 950 --seconds 60 --drain 24 \
  --idle-seconds 5 --idle-interval 2s --client-workers 64 --rpc-batch 1 --admission-workers 3 \
  --checkpoint-interval 24 --checkpoint-cell-proofs --checkpoint-compact-transactions \
  --checkpoint-live-cell-replay --compact-transaction-history --no-comet-tx-index \
  --retain-consensus-blocks 96 --prefund-payments --output target/fern-runs/abci-flush
```

Run the commands sequentially and use a fresh output directory for each network. Use `--rate 650 --seconds 120` or `--rate 625 --seconds 120` for the fixed-rate runs. For the recovery run use `--rate 320 --seconds 30 --p2p-delay-ms 20 --outage-at 9 --outage-seconds 4`. The archive records source and binary hashes, admission replies, per-replica visibility times, commit timing, resource samples and the independently verified execution receipts. [Raw observations](benchmarks/2026-11-25-abci-flush.json.xz)

## ABCI response queue depth

Application `427de2f4f` exposes the size of each connection's ordered response queue. The larger saturation runs held all other local benchmark settings constant, including the binary, three admission workers per replica, 64 RPC client workers and one payment per HTTP request. Four same-host validators ran each depth for 50 seconds at 950 scheduled TPS. The tests used on-chain funding, format-4 checkpoints, compact transaction history and synchronous commits. Each offered payment reached all four replicas in all four runs.

| Queue depth | Offered TPS | Admitted TPS | All-replica TPS | Client drops before dispatch | RPC admission p99 | Finality p99 across all replicas | Mean Flush wait | CPU ms per completed payment |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 128 | 696.2 | 694.9 | 684.7 | 12,436 | 356 ms | 2.19 s | 52.87 ms | 18.87 |
| 64 | 675.5 | 674.2 | 662.6 | 13,470 | 325 ms | 1.93 s | 45.77 ms | 20.16 |
| 32 | 657.3 | 656.0 | 647.1 | 14,387 | 355 ms | 1.86 s | 26.25 ms | 20.53 |
| 16 | 680.2 | 678.9 | 664.4 | 13,239 | 345 ms | 1.87 s | 13.89 ms | 19.81 |

A smaller queue shortened the measured Flush wait during saturation. The admission p99 varied by at most 31 ms in these runs. The 128-slot control had the largest measured throughput and lowest CPU cost per completed payment in the saturation sweep, and remains the default.

A separate saturation run used two admission workers per replica and 128 slots. It offered 690.8 TPS, admitted 689.5 TPS and finalized 676.7 TPS across all replicas. All 34,794 offered payments completed and the client dropped 12,706 scheduled requests before dispatch. Finality p99 was 1.94 seconds. CPU use across the four app and CometBFT pairs was 477.82 seconds, or 13.73 ms per completed payment. The matching three-worker control used 661.56 CPU-seconds, or 18.87 ms per payment, and finalized 684.7 TPS. Mean Flush wait in the two-worker run was 65.94 ms.

Each fixed-rate run targeted 625 scheduled TPS for 90 seconds. The measurements separate client drops from chain confirmation, and keep the full admitted workload visible through drain.

| Queue depth | Admission workers | Offered TPS | Admitted TPS | All-replica TPS | Completed / scheduled | RPC admission p99 | All-replica p50 / p95 / p99 | Node CPU seconds |
| ---: | ---: | ---: | ---: | ---: | --- | ---: | --- | ---: |
| 128 | 3 | 625.0 | 624.9 | 621.1 | 56,250 / 56,250 | 219 ms | 861 / 1,307 / 1,539 ms | 954.30 |
| 32 | 3 | 625.0 | 624.9 | 620.4 | 56,250 / 56,250 | 112 ms | 827 / 1,215 / 1,575 ms | 892.53 |
| 128 | 2 | 625.0 | 624.8 | 618.7 | 56,250 / 56,250 | 161 ms | 853 / 1,261 / 1,612 ms | 749.76 |

The 32-slot run used 6.5% less node CPU than the 128-slot run with the same three workers. Two admission workers with 128 slots reduced CPU use by 21.4% at the same scheduled rate. Each fixed-rate run delivered the full workload with zero client drops and drained after load.

The one-slot setting delivered all 4,608 signed payments in a 192 TPS run with 20 ms one-way loopback P2P delay. The weight-1 replica was down for 4.06 seconds. Of the 781 payments offered during that interval, 203 reached replica 0 before the restart. The restarted replica displayed the last of the 781 payments 4.36 seconds after restart. All-replica p99 including the outage was 7.86 seconds. The CLI rejected depths zero and 129 before opening an application home.

Use the ABCI Flush benchmark command above with `--abci-response-queue 128`, `64`, `32` or `16`, `--seconds 50` and a fresh output directory for each network. Use `--rate 625 --seconds 90` for the fixed-rate comparison and `--admission-workers 2` for the worker-count run. The archive includes workload metadata, CPU and storage samples, admission acknowledgments, visibility times and independently verified execution proofs. [Raw queue and KVM observations](benchmarks/2026-11-25-abci-response-queue.json.xz)

Four KVM guests with separate kernels and ext4 images ran the same application on one physical workstation. Each guest had three vCPUs and 2 GiB RAM and added 20 ms of one-way P2P delay with `netem`. Both runs offered 450 TPS for 55 seconds with the external driver's 24 workers and eight payments per HTTP batch. Both delivered all 24,750 signed payments to all four replicas with zero client drops.

| Queue depth | Admitted TPS | All-replica TPS | All-replica p50 / p95 / p99 | Four QEMU CPU seconds |
| ---: | ---: | ---: | --- | ---: |
| 128 | 449.5 | 441.4 | 1,018 / 1,432 / 1,877 ms | 400.51 |
| 32 | 449.5 | 445.0 | 999 / 1,422 / 1,932 ms | 379.43 |

Use the KVM benchmark command above with `--rate 450 --seconds 55 --p2p-delay-ms 20` and `--abci-response-queue 128` or `32`. The guest CPU samples include the Linux kernels and virtual I/O. The archive records the sample boundaries, authenticated peer addresses and queue settings from the guest logs. [Raw queue and KVM observations](benchmarks/2026-11-25-abci-response-queue.json.xz)

## Admission worker CPU affinity

Four local validators ran application `427de2f4f` for 50 seconds at 950 scheduled TPS. The runs used native signed payments from on-chain prefunding, 64 client workers, one payment per HTTP request, 128 ABCI response slots, format-4 checkpoints and compact transaction history. With affinity enabled, each validator's first admission worker ran on a distinct performance core and each additional admission worker ran on a distinct efficiency core. Only admission threads received a CPU mask. The app, consensus and load driver otherwise shared the workstation's 20 logical CPUs.

| Admission workers | CPU affinity | Offered TPS | Admitted TPS | Finalized and all-replica TPS | Completed | Client drops before dispatch | All-replica p50 / p95 / p99 | CPU ms per completed payment |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | --- | ---: |
| 2 | Inherited | 712.5 | 711.2 | 695.3 | 35,880 | 11,620 | 1,036 / 1,524 / 1,865 ms | 13.15 |
| 2 | Fixed | 638.7 | 637.4 | 626.5 | 32,192 | 15,308 | 1,041 / 1,449 / 1,848 ms | 14.77 |
| 3 | Inherited | 692.7 | 691.5 | 678.3 | 34,893 | 12,607 | 1,058 / 1,570 / 2,103 ms | 19.30 |
| 3 | Fixed | 677.0 | 675.7 | 664.5 | 34,102 | 13,398 | 1,002 / 1,437 / 1,838 ms | 19.94 |

The fixed masks reduced all-replica throughput by 9.9% with two workers and 2.0% with three. Every offered payment completed at all four replicas. The runner read all masks back from Linux before load. The archive includes the host topology, actual masks, the affinity wrapper as `local_affinity_source` and the matched run flags as `local_reproduction`. [Raw affinity and packet impairment observations](benchmarks/2026-11-25-affinity-netem.json.xz)

## Three-minute load with two admission workers

Four validators shared the workstation and ran the same application executable from `427de2f4f`. Each had two admission workers, two proposal VM workers, inherited CPU affinity, 128 ABCI response slots, synchronous RocksDB writes and format-4 checkpoints. The driver used 64 workers and one payment per HTTP request. Before each measurement, the replicas executed signed funding splits to create the payment inputs. The 650 and 635 TPS runs used the benchmark from the executable's source commit. The 600 TPS run used benchmark commit `9303869b9` to collect CometBFT mempool observations with the same executable.

| Scheduled TPS | Offered TPS | Admitted TPS | Finalized and all-replica TPS | Completed / scheduled | Client drops before dispatch | All-replica p50 / p95 / p99 |
| ---: | ---: | ---: | ---: | --- | ---: | --- |
| 600 | 600.0 | 600.0 | 597.0 | 108,000 / 108,000 | 0 | 854 / 1,304 / 1,659 ms |
| 635 | 621.3 | 621.0 | 618.2 | 112,060 / 114,300 | 2,240 | 1,077 / 1,617 / 1,986 ms |
| 650 | 624.2 | 623.9 | 620.7 | 112,615 / 117,000 | 4,385 | 1,080 / 1,602 / 1,973 ms |

Every offered payment reached all four replicas. At 600 scheduled TPS, the 30-second samples of acknowledged payments awaiting all replicas were 538, 474, 689, 674, 492 and 538. The largest one-second sample was 1,029 and the last payment reached every replica 0.67 seconds after the load window. The last payments at 635 and 650 scheduled TPS reached every replica 1.20 and 1.55 seconds after load. The 600 TPS run recorded peaks of 874–1,061 in the per-replica mempools and the last samples showed 82,286–85,666 rechecks since process startup.

At 600 TPS the four replica pairs used 1,461.39 CPU-seconds and sent 639.3 MB of P2P traffic over the sampled interval. They performed 3.43 GB of process writes and their peak sampled RSS ranged from 232 to 239 MB per pair. Replica 0 used 18.46 MB for the application WAL, 43.27 MB for snapshots and 108.45 MB for the consensus WAL. Its p99 synchronous write latency for nonempty blocks was 10.87 ms. At 635 and 650 scheduled TPS, the node pairs used 1,693.81 and 1,676.66 CPU-seconds and sent 664.5 and 672.6 MB of P2P traffic. Each profile's archive records the underlying resource and storage samples.

The 650 TPS run sampled replica 0 for 18 seconds at 99 Hz. The admission threads accounted for 92.3% of sampled performance-core user cycles across that application and CometBFT pair. The decoder and assembly instruction handlers were among the largest symbols. Separate CPU samples during 155 seconds of the 635 TPS run measured 84–88 °C at the CPU package and no increase in the kernel's package thermal throttle counter.

Use the ABCI Flush benchmark command above with `--rate 650`, `635` or `600`, `--seconds 180`, `--admission-workers 2` and `--abci-response-queue 128`. Run each with a new output directory. The archive includes exact run manifests, per-payment acknowledgments and execution times, resource samples, profiles, the public committee configurations and independently verified proofs. [Raw three-minute observations](benchmarks/2026-11-25-steady-load.json.xz)

## Cargo build profile comparison

Both binaries used application source `427de2f4f`, the pinned Rust 1.95.0 toolchain and the same Cargo lockfile. Cargo's `prod` profile inherits the release overflow checks and uses full link-time optimization and one codegen unit. The two builds produced byte-identical RocksDB, Snappy and LZ4 static libraries. The app executable was 21,143,784 bytes with `release` and 17,397,680 bytes with `prod`, a 17.7% reduction.

Four weighted validators shared the workstation. Every run scheduled 52,250 signed native payments over 55 seconds after on-chain funding. Each node used two admission workers, two proposal VM workers, synchronous commits, format-4 checkpoints, compact history and a 128-slot ABCI response queue. The driver used 64 workers and one payment per request. Rows follow actual run order.

| Cargo profile | Offered TPS | Admitted TPS | Finalized and all-replica TPS | Completed | Client drops before dispatch | All-replica p50 / p95 / p99 | CPU ms per completed payment |
| --- | ---: | ---: | ---: | ---: | ---: | --- | ---: |
| `release` | 711.8 | 710.6 | 701.6 | 39,400 | 12,850 | 1,046 / 1,545 / 1,886 ms | 13.22 |
| `prod` | 706.3 | 705.1 | 692.3 | 39,101 | 13,149 | 1,041 / 1,555 / 1,907 ms | 13.34 |
| `prod` | 696.9 | 695.7 | 681.7 | 38,586 | 13,664 | 1,044 / 1,522 / 1,878 ms | 13.65 |
| `release` | 666.6 | 665.5 | 651.9 | 36,916 | 15,334 | 1,070 / 1,615 / 2,048 ms | 14.20 |

Every offered transaction reached all replicas and the verifier independently checked an execution proof from each run. The two `prod` results fell within the measured `release` throughput and CPU cost ranges. The largest sampled combined app and CometBFT RSS for any pair in a run ranged from 198 to 209 MB. The four pairs sent 220–237 MB of sampled CometBFT P2P traffic and performed 841–868 MB of process writes per run. The samples recorded increases of 3.9–5.5 seconds in the CPU package thermal throttle time during the four measurement sessions. Perf samples across both profiles continued to attribute most performance-core user cycles to the admission threads, with VM instruction dispatch and decoding among the leading symbols.

The `prod` binary also finalized all 1,536 transactions in the mixed native CKB and xUDT workload. The native bridge scenario used the frozen CKB L1 executable and deployed the RISC-V vault and deposit contracts on a local dev chain. It completed deposit, attested mint, transfer, burn and L1 payout, then moved the vault to a disjoint signing group and completed another deposit and withdrawal. The archive includes the two offline-verified withdrawal proofs and the bridge's observed L1 transactions and rejection results.

Build with `cargo build --locked --release -p fern` and `cargo build --locked --profile prod -p fern`. For each binary, use the ABCI Flush benchmark command above with `--rate 950 --seconds 55 --admission-workers 2 --abci-response-queue 128` and a fresh output directory. The archive includes the build and run commands, binary and library hashes, per-payment observations, hardware counter samples, resource and storage records, and independently verified execution proofs. [Raw build profile observations](benchmarks/2026-11-25-build-profiles.json.xz)

## Bounded proposal verification

Application `132776326` gives `PrepareProposal` a work allowance of 60 billion charged VM cycles. It takes candidate batches in wire order, reserves the transaction maximum of 200 million cycles per candidate, and verifies each batch in parallel. Successful candidates charge their measured cycles, including cached results. A failed verification charges the full per-transaction allowance. Candidate selection and validation of the finished block still enforce the 30-billion-cycle block limit. When the work allowance defers candidates, the proposer rebuilds the Cell changes from the transactions it actually selects.

An isolated ABCI measurement offered the same 1,400 signed transactions to fresh databases in original, bounded, bounded and original order. Each transaction used the native secp256k1 lock and a RISC-V test type script that ran a fixed number of VM instructions. The proposer used four VM workers and a separate cold serial app accepted and committed the proposed block in every run. The original release checked the scripts of all 1,400 candidates in 5.06 and 5.30 seconds. The bounded release checked 628 candidates in 2.34 and 2.39 seconds, a 54.4% reduction in median preparation time. All four runs chose the same 314 transactions in the same order and recorded 29,905,398,978 cycles, matching execution results and the same application hash.

The live control and treatment each used four same-host validators, two VM and two admission workers, real on-chain funding, synchronous commits and format-4 checkpoints. Both schedules offered all 33,000 native payments over 55 seconds and confirmed all payments after drain with zero client drops. The first network launch exceeded the Unix socket path limit. The measured runs used shorter output names.

| Proposal verification | Offered TPS | Admitted TPS | Finalized and all-replica TPS | All-replica p50 / p95 / p99 | Node CPU seconds |
| --- | ---: | ---: | ---: | --- | ---: |
| Original | 600.0 | 599.9 | 590.8 | 789 / 1,165 / 1,578 ms | 390.42 |
| Bounded | 600.0 | 599.7 | 590.9 | 828 / 1,205 / 1,522 ms | 410.62 |

The bounded run used 5.2% more node CPU in this pair. The bounded binary also finalized all 2,048 two-asset transfers in a live run, with matching serial replay results and checkpoint files. The archive includes the exact signed diagnostic fixture, its reproduction scripts, all live-run observations, unit test output and independently verified execution proofs. [Raw proposal budget observations](benchmarks/2026-11-25-proposal-budget.json.xz)

## Serial replay and checkpoint equivalence

Four weighted validators each used four proposal VM workers and two admission workers. After the load, a newly initialized nonvoting replica replayed the same chain from genesis with one VM worker. The harness compared committed block hashes and full ABCI execution results, including VM cycles and application hashes, at every height through the comparison target. The serial app verified every committed transaction with zero script-cache hits. Checkpoint archives and metadata matched byte for byte at all participating replicas. [Reproduction and recorded execution results](benchmarks/2026-11-25-serial-replay.json.xz)

| Signed workload | Offered TPS | Admitted TPS | Finalized and all-replica TPS | Committed / offered | All-replica p50 / p95 / p99 | Serial replay height / duration | Verified checkpoint |
| --- | ---: | ---: | ---: | --- | --- | --- | ---: |
| Two-asset atomic transfers, 20 ms one-way delay and validator restart | 128.0 | 127.9 | 119.4 | 2,560 / 2,560 | 1,347 / 6,899 / 7,641 ms | 31 / 8.44 s | 24 |
| Bursts of conflicting native spends | 256.0 | 256.0 | 58.3 | 768 / 3,072 | 753 / 1,292 / 1,480 ms | 27 / 5.19 s | 24 |
| Mixed CKB and xUDT with state sync | 256.0 | 256.0 | 244.5 | 5,120 / 5,120 | 799 / 1,352 / 1,528 ms | 44 / 11.13 s | 40 |
| Mixed CKB and xUDT with a post-load checkpoint | 256.0 | 255.9 | 247.7 | 5,120 / 5,120 | 781 / 1,340 / 1,530 ms | 52 / 12.14 s | 48 |

The atomic run killed the weight-one validator at 5.00 seconds and restarted it at 9.08 seconds with its original signing state. The competing-spend run admitted all candidates and committed exactly one per group of four against the same input. All four load runs had zero client drops. In the first atomic attempt, block sync exited while the application was one block short of the comparison height. The harness now waits for that height before comparing results.

In the post-load run, the last transaction executed at height 41. A separate observer installed the certified format-4 checkpoint at height 48 and replayed through height 50 in 7.21 seconds. All four validators, the serial replica and the snapshot observer produced or installed the same 4,269,509-byte checkpoint image. An independent verifier recomputed its archive digest, Live Cell Merkle root and checkpoint application hash from the retained image. Each workload also exported an execution proof that passed offline committee verification.

Use the local committee harness with `--vm-workers 4 --admission-workers 2 --serial-replay --checkpoint-interval 8 --checkpoint-cell-proofs --checkpoint-compact-transactions --checkpoint-live-cell-replay --compact-transaction-history --no-comet-tx-index`. Add `--sync-observer --sync-checkpoint-height 48` for the post-load profile. The archive includes exact commands, build identities, per-transaction observations, per-block result digests, checkpoint images, public committee configurations and execution proofs. [Raw replay observations](benchmarks/2026-11-25-serial-replay.json.xz)

## Seeded KVM peer packet impairments

Four weighted validators with voting powers 2, 2, 1 and 1 ran the same application build in separate Linux guests on one physical host. Each guest had three vCPUs, 2 GiB RAM and a separate ext4 image. All profiles used 32 ABCI response slots, 20 ms of one-way P2P delay and per-guest seeds 4242–4245. The mixed profile added 1% loss, 1% duplication and 10% reordering at each P2P interface's egress queue. The load driver used a separate RPC interface with 24 workers and eight payments per HTTP batch. [Raw affinity and packet impairment observations](benchmarks/2026-11-25-affinity-netem.json.xz)

| Profile | Initial proposal timeout | Offered TPS | Admitted TPS | Finalized TPS | All-replica TPS | Completed / scheduled | All-replica p50 / p95 / p99 |
| --- | ---: | ---: | ---: | ---: | ---: | --- | --- |
| Delay, 65 s | 1 s | 420.0 | 419.7 | 413.2 | 413.2 | 27,300 / 27,300 | 1,023 / 1,447 / 1,875 ms |
| Mixed, 45 s | 1 s | 160.0 | 159.8 | 156.8 | 153.6 | 7,200 / 7,200 | 1,370 / 1,900 / 2,234 ms |
| Mixed, 65 s | 12 s | 420.0 | 419.8 | 408.6 | 408.6 | 27,300 / 27,300 | 2,001 / 2,823 / 3,151 ms |

The first 420 TPS mixed run used the one-second timeout. All four replicas committed the first funding split at height 2. The remaining 13 funding splits missed the 45-second setup deadline while consensus reached round 6 at height 3. Payment dispatch starts after prefunding, so that run recorded a setup failure. The 12-second profile completed all 14 funding splits in 7.44 seconds. Each completed run had zero client drops, matching CKB transaction IDs, heights and VM cycles at all four replicas, and an independently verified execution proof. The 160 TPS run captured the manifest while only benchmark and operations documentation had local edits.

The last queue samples recorded 0, 644 and 1,757 dropped packets across the four guests, in table order. Linux applies these impairments below CometBFT's TCP stream. Aggregate QEMU CPU use was 431.61, 115.62 and 408.12 seconds, with combined peak sampled RSS of 2.25, 1.85 and 2.23 GB. QEMU wrote 1.41, 0.63 and 0.90 GB. During the payment observation window CometBFT reported sending 168.5, 47.0 and 172.7 MB of P2P traffic. The replica-0 p99 synchronous write latencies for nonempty blocks were 18.99, 14.09 and 14.44 ms. The archive includes the failed run's console logs, actual queue settings and samples, and the exact timeout read from each guest's config.

```sh
python3 tools/vm_localnet.py \
  --binary target/release/fern --comet target/fern-tools/cometbft \
  --loadgen target/fern-tools/fern-loadgen \
  --proof-verifier target/fern-tools/fern-proof \
  --rate 420 --seconds 65 --vm-cpus 3 --vm-memory-mib 2048 \
  --abci-response-queue 32 --p2p-delay-ms 20 --p2p-netem-seed 4242 \
  --p2p-loss-percent 1 --p2p-duplicate-percent 1 --p2p-reorder-percent 10 \
  --proposal-timeout-ms 12000 --output target/fern-runs/netem-mixed-wide-timeout
```

Use a fresh output directory for each run. To run the one-second profile, omit `--proposal-timeout-ms`. For the lower-load run set `--rate 160 --seconds 45`. For the delay control also omit the loss, duplicate and reorder options.

## Frozen L1 comparison

The original CKB release executable came from a clean checkout of `119598b0a1146a569068ac80bdba7eb5f6292fd3`. Its SHA-256 was `b258d0d9e7ef1925a6e30564666ddee6d310b6b071461a5c748ecc540429d8f7`. One dev node used Dummy PoW with a 500 ms offered block cadence. It accepted and included all 4,608 signed payments. At 256 offered TPS it included 228.6 TPS during the load window and reached six additional blocks at 184.9 TPS in that window. Inclusion p50/p95/p99 were 1.62/1.96/2.02 s. At six additional blocks those latencies were 4.62/4.96/5.02 s. L1 block depth and weighted sidechain finality are distinct measurements.

```sh
python3 tools/l1_baseline.py \
  --binary target/release/fern --l1 /path/to/frozen/ckb \
  --output target/fern-runs/l1-baseline --rate 256 --seconds 18
```

## Raw evidence

The [compressed observations](benchmarks/2026-11-25.json.xz) include transaction admission, replica visibility, receipt-verifier output, binary hashes and benchmark settings. Use `xz -dc` to inspect the JSON. The bridge, validator handoff and state-sync reports use local test keys. The archive includes executable proof bundles and their public committee configurations for the final bridge and handoff scenarios. The release performance runs precede the checkpoint and admission changes unless their run manifests identify a later commit.

## Native ledger VM fixture cycles

The native host ran these witnessed transactions with CKB-VM assembly, VM reuse, and Clang 23.1.5 RISC-V example ELFs. The cycle counts include the real secp256k1 lock. They measure transaction verification on one process.

| Transaction | Type Script Groups | Charged cycles |
| --- | ---: | ---: |
| Fungible asset transfer | 1 | 1,938,965 |
| Identity and lifecycle update | 2 | 1,931,200 |
| Two-asset atomic settlement with collateral and transfer policy | 5 | 2,020,121 |

Run `cargo test --locked -p ckb-script --features vm-reuse --test fern_native -- --nocapture` to reproduce the fixtures. Serial and four-worker execution charged the same amount for the atomic transaction. The host also checked the budget on cached verification and ran separate Spawn and Exec fixtures. These cases use different transaction structures and signatures, so the difference between rows does not isolate the per-group cost.

## Four-node original-ledger control

A debug build with the bridge recipient fix ran four local CometBFT 0.38.26 replicas and two VM workers per replica. The load offered 64 signed CKB payments per second for 12 seconds, including a restart of one replica. The runner scheduled, offered, admitted, and observed all 768 transactions on every replica. Its sampled admission rate was 63.33 TPS, confirmed rate 56.00 TPS, and client drops zero. Full-replica visibility was p50 2,861 ms, p95 6,473 ms, p99 6,973 ms. The sampled CPU total was 17.67 seconds, process writes were 117,301,248 bytes, and peak combined Fern/CometBFT RSS per replica ranged from 140.5 MB to 146.1 MB. Aggregate sampled P2P counters increased by about 4.67 MB received and 4.73 MB sent, counting sampled increments across the process restart.

The snapshot observer restored checkpoint 16 and caught up through application height 18 in 10.02 seconds. The single-worker observer replayed from genesis through height 19 in 7.49 seconds. The runner verified the exported execution proof. The original ledger control and local CKB/xUDT custody runs exercise the current network binary. The native host has no four-node, staking-handoff, or 1,000–5,000 TPS result.
