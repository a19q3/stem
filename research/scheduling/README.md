# Dispatching independent native verification jobs

The ignored Rust experiment signs and resolves 160 distinct input Cells. It uses the first 32 to estimate time by script-group count. The other 128 mix zero to three type scripts, including four insufficient cycle budgets. A serial reference provides the exact cycle count or complete error string for each original transaction position. All 180 dispatch trials matched every position, with four rejections and 239,286,381 total accepted cycles per trial. Every schedule uses the same bounded Rayon pool. The candidate removes the barrier after each worker-sized wave. A third mode dispatches the higher estimated costs first.

Median wall time in milliseconds over five trials after one warmup, on a Ryzen 9 7950X3D with all 32 logical CPUs available. Each trial alternates the evaluation order. The archived file also includes a 4-logical-CPU affinity sweep, individual p99 start/service times, elapsed service sums and GNU time logs.

| Workers | Fixed waves | Work stealing | Estimated longest first |
| ---: | ---: | ---: | ---: |
| 2 | 8.225 | 6.883 | 7.700 |
| 4 | 4.518 | 3.602 | 4.221 |
| 8 | 3.776 | 2.172 | 2.203 |
| 16 | 2.804 | 1.641 | 1.865 |
| 32 | 3.967 | 1.877 | 1.859 |

At 16 workers the median p99 start time was 2.454 / 1.351 / 1.412 ms. The corresponding sum of elapsed job service divided by workers × elapsed batch time was 59% / 90% / 88%. This ratio measures occupied wall time and includes preemption. Peak process RSS for the entire 4-CPU and full-socket fixtures was 153,132 and 151,932 KiB, including fixture construction and every scheduler configuration.

**Initial decision.** Try cross-wave stealing for already-resolved immutable jobs in block preparation. Estimated sorting added little in this workload.

## Native block preparation

Build with `--features native-work-stealing` to test the scheduler in `Store::prepare`. It uses the existing Rayon pool and can combine up to four consecutive waves of ordinary VM jobs when the remaining block budget covers each transaction's full cycle limit. The original wave boundaries apply to Lineage execution, Keeper actions, L1 references and protocol operations. State staging, VM cycle charging and receipt updates follow proposal order. The flag is independent of the Jolteon Admission and Candidate Pipeline features.

The block regression checks identical signed transactions, Headers, receipts and serialized Live Cell proofs against serial execution. Its cases include a later transaction consuming an earlier output, a double spend, a missing witness and an exhausted block cycle budget.

### Four-replica payment comparison

Four same-host runs used the same 18,000 signed payments, with both builds enabling `jolteon-candidate-pipeline`. The builds differed only by `native-work-stealing`. The offer was 1,800 payments per second for 10 seconds with 8 seconds of drain. Each replica had four VM workers, a 100 ms proposal cadence, and block limits of four billion cycles and 2,048 transactions. All runs admitted and commonly confirmed all 18,000 payments. The driver authenticated the common finality proof and ran the standalone history index check.

| Run order | Schedule | Mean follower preparation | Common confirmations during offer, TPS | Write-to-common p99 |
| --- | --- | ---: | ---: | ---: |
| A1 | Fixed waves | 15.00 ms | 1,751.2 | 0.486 s |
| B1 | Work stealing | 12.25 ms | 1,735.9 | 1.150 s |
| B2 | Work stealing | 11.45 ms | 1,729.8 | 1.260 s |
| A2 | Fixed waves | 15.51 ms | 1,570.8 | 1.844 s |

Mean preparation time, pooled across follower samples within each run, fell by 18% and 26% in the two pairs. Block sizes varied across runs. Window throughput and p99 moved in opposite directions across the pairs.

### Mixed Type Groups with the native pipeline

The native payment driver now accepts `STEM_STORAGE_PAYMENT_TYPE_GROUPS=12`. Each independent genesis Cell spends to a successor with the same amount, classification and TypeSet. The corpus cycles through 0–12 Type Groups. They use distinct Script identities built from the existing RISC-V amount, collateral and classification rules. All transactions use the same Lock and four code dependencies. This isolates variation in Type Groups alongside an ordinary secp256k1 check. Both the Cell read limit (32,768) and the genesis funding approach are fixed for all mixed runs. [Fixture](../../tests/native_store/storage_load.rs)

A four-replica smoke test confirmed 390 transfers. Offline receipt replay recovered 30 transfers at each tier and charged between 1,920,507 cycles at zero groups and 2,137,685 cycles at twelve groups. The serial and four-worker test uses one ordered 260-transfer block with the same fixture and compares the complete Header and receipts.

For the load comparison, both `17d41160ed5e2d89ecec40b92f62711dee3e52d8` builds enabled Admission, Follower via Commit, Leader, concurrent commitments and Commit. The candidate additionally enabled `native-work-stealing`. Four same-host validators and the driver shared logical CPUs `2-7,10-15,18-23,26-31`. Each node used four VM workers, a 100 ms cadence, a four-billion-cycle limit, and a 2,048-transaction limit. The driver scheduled 52,000 signed transfers at 2,600/s for 20 seconds with 30 seconds of drain and 64 persistent senders. The run order was fixed waves, Work Stealing, Work Stealing, fixed waves. [Earlier resource-envelope checks](../execution-pipeline/README.md#block-resource-limits-under-a-payment-burst)

| Run | VM schedule | Common TPS during offer | Write-to-common p99 | Peak acknowledged backlog | Mean proposal | Mean Follower preparation | Validator CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 | Fixed waves | 2,471.10 | 1.279 s | 3,438 | 12.012 ms | 32.372 ms | 105.65 s |
| B1 | Work Stealing | 1,731.75 | 24.338 s | 17,365 | 94.832 ms | 42.786 ms | 148.06 s |
| B2 | Work Stealing | 2,507.85 | 0.898 s | 2,509 | 12.269 ms | 24.651 ms | 102.77 s |
| A2 | Fixed waves | 1,548.40 | 27.149 s | 21,030 | 143.974 ms | 71.782 ms | 162.73 s |

All four runs wrote and acknowledged all 52,000 transfers inside the offer and confirmed them by drain. The common proof and history index passed. A separate read-only replay authenticated the transactions and receipts for 851 observed blocks. The cycle charge for each Type Group tier matched across runs. B1 and A2 each persisted four local timeout votes. Their highest charged blocks used 3,999,981,320 and 3,999,825,353 cycles, respectively. A1 and B2 stayed below 2.61 billion and had zero local timeouts. Severe queuing and near-full VM budgets occurred once with each scheduler. Throughput and p99 reversed direction between pairs. Keep Work Stealing opt-in, and use this fixture to compare resource budgets under a genuinely cycle-limited load. The mix uses one Lock and a maximum per-transfer cycle difference of about 11%. It does not represent heavier proof validation or mutable Head conflicts.

The binaries hashed to `b8953afa511f2b75a5b90e667a449ee9d152e7a661d051f8ab0575575566f4ea` (fixed waves) and `b3153856d62e08baffc6304ae22bdb8c2d46b91d0a7b7a54e55628776587426b` (Work Stealing). The signed workload and genesis SHA-256 values were `5a1ed47acd8565640e86dba8965c54d5949617977b413a6429dc145e2b4d37ca` and `8f43fa48be3b92af25a6c8460ace8706da03453591f65a38f53521c0c2b65116`. The fixture compiled the Type Scripts with Clang 23.1.5 and LLD.

Run `storage_load::sustained_payment_storage` with `STEM_TEST_REPLICA_BIN` and `STEM_STORAGE_PAYMENT_TYPE_GROUPS=12`. Under the `STEM_STORAGE_` prefix set `REPLICAS=4`, `RATE=2600`, `SECONDS=20`, `DRAIN=30`, `SENDERS=64`, `VM_WORKERS=4`, `RETENTION=256`, `CADENCE=100`, `BLOCK_CYCLES=4000000000`, `BLOCK_TRANSACTIONS=2048` and enable `READY_SENDERS`, `COMPLETE_VOLUME` and `REUSE_SUBMISSIONS`. The mixed fixture allocates independent input Cells at genesis and requires `PREFUND_PAYMENTS=0`, `PAYMENT_CHAINS=0` and `PAYMENT_DATA_BYTES=0`.

## Resolving and verifying a window of Cell transactions

`native-dependency-execution` is an opt-in candidate for `Store::prepare`. The earlier Work Stealing candidate starts VM verification after ordered Cell resolution and staging. This candidate passes resolution, effect construction and VM verification to the workers. A transaction reads the latest declaration for an OutPoint strictly before its own position, including deletions, then the accepted block prefix and the pinned parent. The same lookup expands Dep Groups. A consumer can verify against its producer's immutable output while the producer's authorization is still running.

The coordinator accepts results in proposal order and applies each transaction's actual resource charges before publishing its effects. If a trial fails, its charge will not fit, or it depends on an edit from a prior unadopted trial, the coordinator runs the transaction through ordered staging. This leaves independent trials eligible to be reused after a fallback. Windows are bounded by four jobs per worker, 32 total jobs and the remaining block cycles at each transaction's full cycle limit. Jobs exceeding the speculative read limit of 4 MiB fall back to ordered staging. Lineage registry changes, Keeper, bridge operations, certified L1 references and group messages use ordered staging.

The branch checks cover a same-block spend chain, conflicting spends, a replaced speculative parent, and a Dep Group created, read and consumed within one block. The branch was checked with `cargo check --locked --offline --release --all-features --tests`, `cargo build --locked --offline --release --features native-dependency-execution --bin stem` and `cargo check --locked --offline --release --no-default-features --lib --bin stem`. Regression execution, independent replay and load comparisons are pending. The preceding Work Stealing results apply to the earlier candidate.

When workload testing resumes, compare builds differing only in `native-dependency-execution` with the same workers, parent snapshots and input corpus. Independent mixed Type Groups and same-block spend chains exercise different parts of the mechanism. Record end-to-end confirmation and Follower preparation alongside worker CPU, fallbacks and discarded work. Projection adds an OutPoint index and a hash of each transaction up front. Read contention and repeat work after an invalid prefix may offset the savings on VM-cache hits or under tight resource limits.

### Greedy Leader selection

`native-leader-dependency-execution` enables the same preparation workers for the Leader's ordered mempool scan. It is independent of `native-dependency-execution`. The Leader reserves space for a window against the current block and proposal wire byte limits. Workers record which earlier projected edits they observed, including dependency-group expansion and checks for a duplicate output. The scan still visits entries in input order and applies its current block limits. It can adopt a worker result only if it adopted every source of the projected edits that result read. Failed and invalidated trials return to the original staging path, which classifies rejections and decides whether to end the scan. Independent trials can be adopted after a rejected proposal candidate. Header construction and replay use the existing origin and transaction-root rules.

Leader scans expose `parallel_cell_jobs`, `parallel_cell_reused`, `parallel_cell_fallback` and `parallel_cell_us`. The gap between jobs and the sum of reused and fallback trials counts work the scan skipped or never reached. Parallel time covers projection and worker execution. It contributes to overall selection wall time and should not be added to it. Regressions compare exact selected transaction bytes, Headers and receipts after a rejected producer, a dependent spend, a replacement spend, and a double spend, including a tight wire budget. The Dep Group fixture also checks that the Leader skips a reader following the group's consumption.

The implementation is compile-checked pending runtime and replay qualification. To attribute improvements when testing resumes, compare the original builder, Leader only, Follower only, and both stages with fixed workers and index settings. With valid independent transfers, the coordinator should reuse most trials. A churn workload should also contain stale inputs and competing spends to quantify work repeated or launched past a scan stop. Each window still waits for its workers before adopting results, and saturated blocks fall back to the existing builder once they can no longer reserve multiple full transaction cycle limits.

### Rolling Follower preparation

`native-rolling-cell-execution` extends ordinary Follower preparation. For a run longer than a fixed window, one immutable projection covers the run. Workers verify against that projection and the earlier accepted block prefix. The coordinator keeps a separate ordered delta and refills the queue after publishing a ready prefix. A slow later trial no longer holds back publication of earlier results. Trials carry the positions of the edits they read across previous window boundaries. Ordered fallback reads the actual prefix and delta, and only successful trials certify their projected writes.

The active job queue is limited to two jobs per worker. Running and completed work together stay within four slots per worker, 32 slots overall, and the remaining block cycles reserved at each transaction's full limit. Reads retain the 4 MiB per-trial limit. The projection is bounded by block transaction limits, and the short-window and ordered paths still handle small runs and protocol barriers. The Leader continues to use its separate fixed-window configuration.

`candidate_cell_stream` reports attempted jobs, adopted results, fallbacks, refills overlapping earlier unadopted jobs after the initial window, peak outstanding work, and elapsed run time including projection. Compiled fixtures compare full Headers, receipts and writes to serial preparation. They cover a refill beyond the initial window, spending across rolling boundaries and starting from a nonempty ordered prefix. Regression execution, independent replay and performance qualification are pending.

For matched tests, hold the Leader feature and resident index configuration constant and vary only the rolling option over the fixed-window Follower. Include fully independent payments and long same-block chains with unequal script costs. Inspect refill occupancy and full preparation time against coordinator and worker CPU. A large immutable projection front-loads indexing, and a slow earliest position can still fill the bounded reorder buffer. The queue hands completions through a channel, which can cost more than the barrier it removes on short VM-cache-hit workloads.

## Reproduce the resolved-job experiment

```sh
CXXFLAGS='-include cstdint' cargo test --locked --release -p stem-vm --test native --no-run
# Use the executable path printed by Cargo. The native fixture needs clang with RV64 support.
taskset -c 0-31 /usr/bin/time -v /path/to/native-test-binary measure_resolved_job_schedules --ignored --nocapture
```
