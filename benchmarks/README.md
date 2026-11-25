# Stem experiment archive

Each experiment round has a durable report and its available run evidence.
The report names the baseline, tested revision, workload, hardware and
toolchains, commands, comparison conditions, results and run-specific limits.
Keep negative results and superseded approaches alongside successful runs.
Add new dated records for reruns so that original measurements remain
available. Published network evidence contains public genesis and proof data.

- [Current Stem measurements](../BENCHMARKS.md)
- [Earlier Fern measurements](../FERN-BENCHMARKS.md)
- [Research scope and current verification status](../STEM-EVOLUTION.md)

| Recent round | Report and evidence |
| --- | --- |
| Registered Lineage Heads and paired authorization | [Report](../BENCHMARKS.md#paired-authorization-and-lineage-lifecycle), [evidence](2026-11-25-stem-lineage.json) |
| Dynamic Lineage, conflicting execution and AMM | [Report](../BENCHMARKS.md#dynamic-lineage-commands), [evidence](2026-11-25-stem-dynamic-lineage.json) |
| Portable and reused assembly Executor comparison | [Report](../BENCHMARKS.md#executor-backend-selection), [evidence](2026-11-25-stem-executor-backends.json) |
| Repeated native signature recovery | [Report](../BENCHMARKS.md#repeated-native-signature-recovery), [evidence](2026-11-25-stem-secp-reuse.json) |
| Native Keeper | [Report](../BENCHMARKS.md#native-keeper), [evidence](2026-11-25-stem-keeper.json.xz) |

Read compressed run records with `xz -dc benchmarks/<file>.json.xz`.
