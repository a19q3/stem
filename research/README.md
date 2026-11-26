# Stem research experiments, issues 1–14

The experiments use Stem at `fefb8cc01771d59e228c72755ddd1242291f7f22` as the starting point. The studies below include separately selectable candidates, raw outputs and replay instructions. Measurements range from exhaustive finite checks to simulations, loopback TCP, native multi-replica processes and a funded CKB development chain. The evidence column states where each decision applies.

| Issue | Evidence | Decision from this round |
| --- | --- | --- |
| [1](https://github.com/a19q3/stem/issues/1#issuecomment-7196714543) | [Native record sorting and real size trace](evolve/README.md) | Retain the standard sort. Neither small-sort candidate improved the measured Rust workload. |
| [2](https://github.com/a19q3/stem/issues/2#issuecomment-7197400672) | [Fresh replicated RocksDB databases and SST events](storage/README.md) | Keep the stock profile and isolate the cost of compaction over longer retention windows. |
| [3](https://github.com/a19q3/stem/issues/3#issuecomment-7197929163) | [Signed hotspot AMM traffic on 16 validators](admission/README.md) | Profile admission queues and candidate re-verification before expanding transport prechecks. |
| [4](https://github.com/a19q3/stem/issues/4#issuecomment-7196996006) | [Canonical proofs over one million live keys](patricia/README.md) | Test shared-prefix proof reads against a live RocksDB snapshot. |
| [5](https://github.com/a19q3/stem/issues/5#issuecomment-7197227599) | [Scheduled native verification jobs](scheduling/README.md) | Test plain work stealing on an ordered candidate block with the existing dependency barriers. |
| [6](https://github.com/a19q3/stem/issues/6#issuecomment-7196715347) | [Bit-vector proof and actual contract instructions](immediates/README.md) | Keep the reference immediate extractors. The equivalent candidate was slower. |
| [7](https://github.com/a19q3/stem/issues/7#issuecomment-7196995335) | [Mixed Jolteon networks and real CKB handoffs](certificates/README.md) | Keep exact certificate reuse opt-in and measure process-wide cost at matched arrivals. |
| [8](https://github.com/a19q3/stem/issues/8#issuecomment-7196428861) | [Weighted quorum intersections and signed finality replays](finality/README.md) | Reuse the independent verifier and restart corpus for candidate regressions. |
| [9](https://github.com/a19q3/stem/issues/9#issuecomment-7197311116) | [Finite spectra, dissemination model and TCP bursts](dissemination/README.md) | Measure auxiliary payload traffic on multiple hosts with independent egress. |
| [10](https://github.com/a19q3/stem/issues/10#issuecomment-7197311767) | [Finite cuts and paired auxiliary trees](dissemination/README.md) | Evaluate a bounded repair deadline alongside latency and total bytes. |
| [11](https://github.com/a19q3/stem/issues/11#issuecomment-7197142612) | [Two finite local codes and matched Reed–Solomon baselines](availability/README.md) | Keep RS as the worst-case recovery baseline. Include structured placement loss in future tests. |
| [12](https://github.com/a19q3/stem/issues/12#issuecomment-7196716433) | [Five cache policies and a held-out replica](evolve/README.md) | Keep the existing policy. Modeled cache hits did not separate the strategies on the held-out trace. |
| [13](https://github.com/a19q3/stem/issues/13#issuecomment-7197929694) | [Published broadcast selector and Stem recovery comparison](broadcast/README.md) | Retain authenticated retrieval. The advanced branch selects none of the tested 16–256-party cases. |
| [14](https://github.com/a19q3/stem/issues/14#issuecomment-7196717098) | [Pinned evaluator, correctness gates and candidate replays](evolve/README.md) | Use the offline evaluator for reversible candidates and require a separate integrated trial. |

The next experiments on the [evolution path](../STEM-EVOLUTION.md) are the 16-replica hotspot and durable storage workloads, followed by matched all-reference and mixed Jolteon runs for the most promising local changes. For auxiliary routing, independently limit host egress and measure healthy and repaired delivery separately. Check the same signed Finality/CKB boundary when a candidate reaches the native protocol.
