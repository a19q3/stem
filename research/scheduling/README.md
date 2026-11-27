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

Mean preparation time, pooled across follower samples within each run, fell by 18% and 26% in the two pairs. Block sizes varied across runs. Window throughput and p99 moved in opposite directions across the pairs. Keep the feature opt-in pending sustained runs on independent hosts and an end-to-end workload with varied script costs.

## Reproduce the resolved-job experiment

```sh
CXXFLAGS='-include cstdint' cargo test --locked --release -p stem-vm --test native --no-run
# Use the executable path printed by Cargo. The native fixture needs clang with RV64 support.
taskset -c 0-31 /usr/bin/time -v /path/to/native-test-binary measure_resolved_job_schedules --ignored --nocapture
```
