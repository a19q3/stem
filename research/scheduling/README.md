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

## Reproduce the resolved-job experiment

```sh
CXXFLAGS='-include cstdint' cargo test --locked --release -p stem-vm --test native --no-run
# Use the executable path printed by Cargo. The native fixture needs clang with RV64 support.
taskset -c 0-31 /usr/bin/time -v /path/to/native-test-binary measure_resolved_job_schedules --ignored --nocapture
```
