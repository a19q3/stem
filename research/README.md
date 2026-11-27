# Stem research experiments, issues 1–14

Research reports and smaller result files are in the current tree. Replay commands that use relocated raw inputs can obtain them from the pinned [archive inventory and retrieval guide](../benchmarks/README.md).

The fixed-delay execution experiment is retained as a [results report](delayed-execution/README.md).

The experiments use Stem at `fefb8cc01771d59e228c72755ddd1242291f7f22` as the starting point. The studies below include separately selectable candidates, raw outputs and replay instructions. Measurements range from exhaustive finite checks to simulations, loopback TCP, native multi-replica processes and a funded CKB development chain. The evidence column states where each decision applies.

| Issue | Evidence | Decision from this round |
| --- | --- | --- |
| [1](https://github.com/a19q3/stem/issues/1#issuecomment-7196714543) | [Five cache policies and a held-out replica](evolve/README.md#success-cache-gate) | Keep the existing policy. Modeled cache hits did not separate the strategies on the held-out trace. |
| [2](https://github.com/a19q3/stem/issues/2#issuecomment-7197400672) | [Fresh replicated RocksDB databases and SST events](storage/README.md) · [Sustained load and compaction](storage/sustained/README.md) | Keep the stock profile and isolate the cost of compaction over longer retention windows. |
| [3](https://github.com/a19q3/stem/issues/3#issuecomment-7197929163) | [Signed hotspot AMM traffic on 16 validators](admission/README.md) · [Proposal order on four validators](admission/order/README.md) | Profile admission queues and candidate re-verification before expanding transport prechecks. |
| [4](https://github.com/a19q3/stem/issues/4#issuecomment-7196996006) | [Canonical proofs and the RocksDB batch query](patricia/README.md) | Shared-prefix reads are available through the Store and native RPC. Parallel subtree updates and reclamation remain research. |
| [5](https://github.com/a19q3/stem/issues/5) | [Native Work Stealing and mixed Type Group trials](scheduling/README.md) | Optional in native verification. The mixed workload hit the cycle limit with both schedulers, and the paired throughput results reversed. |
| [6](https://github.com/a19q3/stem/issues/6#issuecomment-7196715347) | [Bit-vector proof and actual contract instructions](immediates/README.md) | Keep the reference immediate extractors. The equivalent candidate was slower. |
| [7](https://github.com/a19q3/stem/issues/7) | [Mixed networks, CKB handoffs and matched sixteen-replica bursts](certificates/README.md) | Keep certificate reuse opt-in. At 550 ms the QC cache hit in both trials, with no repeatable CPU gain. |
| [8](https://github.com/a19q3/stem/issues/8#issuecomment-7196428861) | [Weighted quorum intersections and signed finality replays](finality/README.md) | Thirteen native signed observations are in the fixed Cargo regression. Systematic schedule exploration remains open. |
| [9](https://github.com/a19q3/stem/issues/9#issuecomment-7197311116) | [Finite spectra, dissemination model and TCP bursts](dissemination/README.md) | Measure auxiliary payload traffic on multiple hosts with independent egress. |
| [10](https://github.com/a19q3/stem/issues/10#issuecomment-7197311767) | [Finite cuts and paired auxiliary trees](dissemination/README.md) | Evaluate a bounded repair deadline alongside latency and total bytes. |
| [11](https://github.com/a19q3/stem/issues/11#issuecomment-7197142612) | [Two finite local codes and matched Reed–Solomon baselines](availability/README.md) | Keep RS as the worst-case recovery baseline. Include structured placement loss in future tests. |
| [12](https://github.com/a19q3/stem/issues/12#issuecomment-7196716433) | [Native record sorting and real size trace](evolve/README.md#pure-ordering-gate) | Retain the standard sort. Neither small-sort candidate improved the measured Rust workload. |
| [13](https://github.com/a19q3/stem/issues/13#issuecomment-7197929694) | [Published broadcast selector and Stem recovery comparison](broadcast/README.md) | Retain authenticated retrieval. The advanced branch selects none of the tested 16–256-party cases. |
| [14](https://github.com/a19q3/stem/issues/14#issuecomment-7196717098) | [Pinned evaluator, correctness gates and candidate replays](evolve/README.md) | Use the offline evaluator for reversible candidates and require a separate integrated trial. |

## Remaining native integration work

- Experiments **1–5** feed the native configuration comparison. Keep the stock RocksDB durability settings and use the recorded compaction counters. Shared-prefix Patricia proofs run through native RPC, and parallel Cell subtree mutation is the default after the payment and mixed-script comparisons. Compare cache capacity, age priority and the admission and execution workers on matched workloads. The slower two-pass Patricia prefetch and the mixed Work Stealing throughput results remain controls. [Current decisions](../STEM-V1-PLAN.md#local-pipeline-decisions)
- Experiments **7, 9 and 10** share the later network comparison. Measure certificate reuse and auxiliary routing together with bounded egress, repair latency, total traffic and recovery from controlled losses. The loopback evidence and the mixed-version certificate checks supply starting controls.
- Experiments **8 and 14** supply qualification checks. Keep signed weighted-finality replay and negative controls in the benchmark path. Use held-out evaluation to select reversible experiments and require separate replicated evidence for production changes.
- Experiments **6, 11, 12 and 13** retain their existing native implementations. The replacement immediate extractors and small sorts were slower, the local availability codes failed structured-loss checks, and the advanced broadcast selector found no applicable case at the tested sizes.

The [native consolidation sequence](../STEM-V1-PLAN.md#native-consolidation-sequence) prioritizes integrated ablations and storage, application and network improvements. The existing sustainable payment profile remains a reference for later qualification.
