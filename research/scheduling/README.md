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

**Decision.** Plain cross-wave stealing warrants a prepare-path experiment for already-resolved immutable jobs. Estimated sorting added little in this workload. Ordered state staging, dynamic Head dependencies, Keeper boundaries, candidate resource cuts, receipt/Header/proof equivalence and sustained network throughput still need a block-level comparison before changing deployment defaults.

```sh
CXXFLAGS='-include cstdint' cargo test --locked --release -p stem-vm --test native --no-run
# Use the executable path printed by Cargo. The native fixture needs clang with RV64 support.
taskset -c 0-31 /usr/bin/time -v /path/to/native-test-binary measure_resolved_job_schedules --ignored --nocapture
```
