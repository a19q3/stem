# Lineage Executor engine comparison

Fourteen local 16-replica trials on x86-64 compare the stock sparse-memory interpreter with two uses of CKB's assembly runner for the standalone RISC-V Lineage Executor. The first allocates a fresh core on every call. The second uses the scheduler's existing thread-local core pool and resets it between calls. Script groups continue using their existing VM in every configuration.

| 200 signed hotspot AMM commands/s for 60 s | Stock (fresh-core series) | Fresh assembly | Stock (pool series) | Pooled assembly |
| --- | ---: | ---: | ---: | ---: |
| Trials / confirmed commands per trial | 3 / 12,000 | 3 / 12,000 | 3 / 12,000 | 3 / 12,000 |
| Candidate verification mean, median across runs (ms) | 33.06 | 43.94 | 32.83 | 33.21 |
| Replica CPU seconds, median | 209.64 | 256.12 | 209.29 | 211.19 |
| Common-finality p99, median (ms) | 924 | 1,061 | 923 | 952 |
| Within-window common TPS, median | 197.98 | 196.82 | 197.37 | 197.70 |

In the additional 240 TPS pair, both finalized all 14,400 commands. Stock and fresh assembly measured 234.53 and 236.33 within-window TPS, 41.58 and 54.90 ms mean candidate verification, and 246.62 and 295.64 replica CPU seconds. Both schedules had identical signed workloads, genesis, confirmed ID sets, charged cycles and fees at each rate. Every run replayed its own history serially, checking receipts and both updated Heads per command. The two series shared a workstation with a separate four-replica, 50 TPS storage-aging experiment and short development builds.

A fixed Store workload with 31 blocks of eight swaps measured median serial prepare at 5.33–5.48 ms on stock and 6.29–6.30 ms on fresh assembly. In the pool series the medians were 5.40–5.64 ms on stock and 5.30–5.33 ms on pooled assembly. The real guest differential checked outputs, exit status and cycle cutoffs across changes of data, head count, budget and crypto rules. The local improvement from pooling did not reproduce across all three network pairs. The stock standalone interpreter remains the default. [Per-run measurements](summary.json) · [Runtime checks](differential.log)

## Reproduce

Starting at `0988bb9`, apply [runner.patch](runner.patch). Build three release replicas with the same lockfile, one without additional features, one with `--features stem-vm/lineage-native-runner` and one with `--features stem-vm/lineage-reused-runner`. Save each executable before building the next. Set `STEM_TEST_CONTRACT_DIR` to the compiled contract directory and run `cargo test --release -p stem-vm --features lineage-reused-runner`. For a fixed Store comparison, run the corresponding native_store test executable on CPUs 4–7 with `lineage::measure_lineage_hotspot_amm --exact --ignored --nocapture`.

Run the [replicated load harness](../storage/run_load.py) with `--mode hotspot_amm --replicas 16 --seconds 60 --drain 36`. In each 200 TPS series, cycle stock / candidate / candidate / stock / stock / candidate. In the 240 TPS pair run fresh assembly then stock. The compressed archives include the release binary hash, raw deliveries, per-replica samples and verified finality evidence. Run `python3 compare.py *.json.xz` in this directory to check every cohort.
