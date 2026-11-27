# Stem V1 engineering plan

Review date 27 November 2026. The published integration baseline is [`654c9e1`](https://github.com/a19q3/stem/commit/654c9e1e3da6ef2ca64cfe8bdf19b83716ee33d1). The local evidence includes the native integration rounds through `227be92`. Runtime flags and reports in this checkout have their own pinned build commits. Use `fefb8cc` and `12338f2` for the historical comparisons that specify them.

Stem V1 votes authenticate the executed Header and use unified state finality. The CKB settlement adapter consumes that finality with the existing Checkpoint proof format. Standard Spore and Cluster settlement has a frozen regression baseline. Extended Spore compatibility and fixed-delay execution have separate research records. [Protocol](STEM-PROTOCOL.md) · [Evolution path](STEM-EVOLUTION.md) · [Original acceptance index](research/original-21.md)

## Local pipeline decisions

| Area | Local decision entering qualification |
| --- | --- |
| Admission and Follower | Exercise Follower first on physical hosts. Compare the separate Admission toggle under the same resource budget. The combined same-host build improved both window TPS pairs and worsened both p99 pairs. |
| Leader | Keep the worker opt-in. Both short pairs favored synchronous construction for window TPS and p99. Capture time spent scanning inputs already spent on the speculative branch. |
| Work Stealing and commitments | Keep both opt-in. Mixed Work Stealing pairs reversed direction. Concurrent commitments reduced mean Follower prep and control wait in both pairs, with less than 0.4% difference in window TPS. |
| Certified Commit | Sixty-second writer-only pairs confirmed all 156,000 payments per run. Window TPS fell 0.5–1.1%, p99 fell by 19 ms and 7.072 s, and both pairs used more CPU. Continue on independent hosts. The paused-writer and crash probes cover the descendant-reuse and publication boundary separately. |
| Block resources | Use 4B cycles and 2,048 transactions for the first payment reference. In the 3,900/s mixed-script replay, cycle-limited VM visits accounted for 75.5% and 77.0% of proposal wall time in stalled controls. The opt-in scan limit cleared every acknowledged transfer in both paired runs. The 2,600/s trials and overload tails keep the candidate in qualification. |

[Native evidence and reproduction](research/execution-pipeline/README.md) · [Mixed scheduler evidence](research/scheduling/README.md)

## Qualification sequence

The single-host rehearsal completed all three profiles on four loopback validators. Offline replay passed and the performance gates remained open. The 1,800/s baseline drained all payments and exceeded the backlog limits. The 3,000/s and 3,300/s offers filled the mempool and left acknowledged payments pending at cutoff. Independent-host acceptance is deferred. [Results](research/storage/external/README.md#single-host-v1-profile-rehearsal)

1. **First independent-host payment run.** Four physical validator hosts and a fifth driver host. Record the actual placement, binary and contract digests, resources, retention, network conditions and exact signed workload. Establish the 1,800 TPS reference before testing 3,000 TPS. Run the sustainable and overload profiles separately. [Qualification procedure](research/storage/external/qualification.md)
2. **Attribute the native bottleneck.** Run matched feature comparisons on the same host allocation. Capture control wait, proposal search, Follower preparation, commitments, WAL and persistence, CPU, rejection and backlog. The local proportion of proposal visits rejected for a speculative-parent spend was 35.5–36.7%. Measure elapsed cost for those visits and preserve eligibility on branch switches. Tie larger cycle or count limits to a profile that actually consumes them. [Execution report](research/execution-pipeline/README.md)
3. **Verify recovery and validator operations at load.** Run timeouts, leader and process loss, stale completion, retention and restart, mixed versions, and CKB-backed committee replacement. Exercise durable journal/publication boundaries and verify proofs independently. The validator-admission gate requires a new key outside the genesis directory and a published account of guardian authority and failure recovery. [Native finality regression](research/finality/README.md) · [Operator procedures](OPERATIONS.md#validator-admission-and-guardian-authority)
4. **Qualify application resources and custody.** Establish native mixed-script, multi-Head Lineage and due Keeper baselines. Exercise independent and hot AMM markets before an order-book benchmark. Carry the supported Polytype and Metatype obligations through CKB redemption and recovery. Compare per-class outcomes, charged cycles and return-path headroom. [Mixed-script baseline](research/scheduling/README.md#mixed-type-groups-with-the-native-pipeline) · [Metatype specification](METATYPE.md)
5. **Scale by demonstrated capacity.** Add 16 and 32 validators with matched machine and link budgets, then extend toward the larger committees in #30. Qualify 5,000 and 10,000 sustained TPS as separate targets once lower profiles pass. Evaluate network research on payload delivery and repair with measured egress limits and RTT.

The immediate release gates are the independent-host profile, dynamic admission and guardian recovery, and the semantics and settlement checks in #15–#20. Native application scale follows the fixed baseline. Sparse networking, alternate availability codes, new deployment domains and Stem Primary run as separate research tracks. The independent-host profile is pending physical validator and driver placement.

### Payment acceptance

Use 60 seconds warmup, 180 seconds measurement and 45 seconds drain. For the sustainable 3,000 profile, measure at least 3,000 common confirmations/s, at least 99.9% scheduled writes, at most 0.1% unacknowledged, p99 at most five seconds, acknowledged backlog peak at most 15,000 and growth across minute boundaries at most 3,000. Declare changes to service targets before a run. Every acknowledged payment must commonly confirm by drain. For the first overload run offer 3,300/s, retain the 3,000 TPS floor, declare rejection and backlog bounds, and report p99 with its population. Preserve failing runs and distinguish lost writes, missing acknowledgments, drained work and work left pending. [Exact rules and verifier](research/storage/external/qualification.md)

## Issue review

### Experiments 1–14

| Issue | Evidence or local decision | Next gate |
| --- | --- | --- |
| [#1 Verification cache](https://github.com/a19q3/stem/issues/1) | Five policies on held-out replica data. Keep the reference cache. | Revisit when a current trace separates policies under the same capacity and validity gates. |
| [#2 RocksDB](https://github.com/a19q3/stem/issues/2) | Stock durability and compaction profile retained. Long-window SST and compaction accounting exists. | Measure independent devices, aged churn, read tails and recovery. Report WAL, SST and device writes separately. |
| [#3 Admission](https://github.com/a19q3/stem/issues/3) | Opt-in pipeline and paired Follower interaction. The combined build improved window TPS in both pairs and raised p99 in both. | Check long-window contention and isolate parent-spent proposal cost with #26. |
| [#4 Patricia](https://github.com/a19q3/stem/issues/4) | The Store and native RPC share immutable prefix reads. A RocksDB run reduced logical reads by 65.2% and preserved exact proof bytes. | Measure subtree mutation cost and reclamation while checking root, write order and pinned-view validity. |
| [#5 VM scheduling](https://github.com/a19q3/stem/issues/5) | Work Stealing is opt-in. The 13-tier mixed Type Group comparison produced opposite paired throughput results. | Use heavier proof costs and fixed-worker controls in the integrated suite. |
| [#6 VM host paths](https://github.com/a19q3/stem/issues/6) | Immediate extraction, ELF loading and local Lineage controls retained their reference paths. | Profile a current replicated cost before testing another translated candidate. |
| [#7 Certificate reuse](https://github.com/a19q3/stem/issues/7) | Opt-in. The 16-replica 550 ms control reused QC frames and had no repeatable CPU gain. | Measure longer independent-host runs with warm reuse, timeouts and whole-process allocation counts. |
| [#8 Weighted finality](https://github.com/a19q3/stem/issues/8) | Thirteen authenticated restart and handoff observations in Cargo. Later-anchor ancestry replay covers retained Header queries. | Add deterministic schedule exploration for completion, timeout, restart and committee interleavings. |
| [#9 Sparse overlays](https://github.com/a19q3/stem/issues/9) | Finite graph and loopback studies bound possible payload gains. | Healthy and repaired delivery on independent links with explicit egress and bytes. |
| [#10 Redundant trees](https://github.com/a19q3/stem/issues/10) | Paired auxiliary trees have finite cut and reachability evidence. | Measure repair deadlines, correlated endpoint loss and total traffic jointly with #9. |
| [#11 Availability codes](https://github.com/a19q3/stem/issues/11) | Retain Reed–Solomon. Structured losses defeated the two evaluated local codes. | Check reconstruction guarantees and placement under correlated loss before using native output. |
| [#12 Small sorts](https://github.com/a19q3/stem/issues/12) | Keep the standard sort. Measured native candidates lost on the recorded distribution. | Revisit when the input distribution or attributable sort cost changes. |
| [#13 Broadcast](https://github.com/a19q3/stem/issues/13) | Keep authenticated recovery. The published advanced branch was unselected at the evaluated sizes. | Require a concrete recovery bottleneck and an applicable construction before implementation. |
| [#14 Evaluator](https://github.com/a19q3/stem/issues/14) | Offline evaluator has held-out traces, correctness gates and negative controls. | Use its pinned artifacts alongside integrated and fault evidence in #26–#30. |

These experiments have the original evidence and reproduction links in the [research index](research/README.md).

### Semantics, storage and application work

| Issue | Evidence or current scope | Next gate |
| --- | --- | --- |
| [#15 Late Type attachment](https://github.com/a19q3/stem/issues/15) | Four native replicas verified issuance, later deployment, attachment, mixed-group conservation, split, merge and signed discharge. Strip, dust dilution, missing asset identity and missing Lock witnesses were rejected. | Carry the same logical predicates into the cross-runtime and resource gates in #17 and #20. |
| [#16 Composition](https://github.com/a19q3/stem/issues/16) | The native two-creditor fixture distinguishes independent conservation from exclusive collateral rights and checks serial and parallel rollback for strip and dust successors. [Evidence](METATYPE.md#independent-creditor-accounting) | Cover schema and lifecycle conflicts, then run equivalent obligations through CKB Metatype. |
| [#17 Cross-runtime conformance](https://github.com/a19q3/stem/issues/17) | Mixed native Type Groups compare exact ordered Headers and receipts. Existing portable deployments have paired functional evidence. | Check complete native and CKB group and predicate conformance with supported boundary cases and failures. |
| [#18 Adapter migration](https://github.com/a19q3/stem/issues/18) | The physical Metatype Type binds an immutable adapter table. | Pin a migration relation and demonstrate authorised conservation or unique transfer with outstanding messages and rollback. |
| [#19 Anchors and settlement](https://github.com/a19q3/stem/issues/19) | CKB consumes execution finality through the adapter. The cross-chain trust boundary and anchor assumptions are documented. | Verify freshness, reorg and halt, late acknowledgement and refund races, and authority for every terminal release. |
| [#20 Resources and redemption](https://github.com/a19q3/stem/issues/20) | Mixed Type Groups exercised the native cycle boundary. Supported Metatype limits have separate CKB controls. | Sweep groups, proofs, memory and fee budgets under load. Keep measurements of custody return capacity and wall-clock CPU separate. |
| [#21 CellScript](https://github.com/a19q3/stem/issues/21) | Deferred until CellScript 0.40 and explicit maintainer review. | Stabilise raw RISC-V evidence and the portable ABI first. |
| [#22 Retrospective](https://github.com/a19q3/stem/issues/22) | Historical observations and their comparison scope are recorded. | Keep later release and qualification evidence indexed with build identity. |
| [#23 Script interning](https://github.com/a19q3/stem/issues/23) | Canonical Live Cell records retain full Script descriptors. | Measure across live state and block churn. Prove exact canonical reconstruction, atomic reference accounting and migration before a writer change. |
| [#24 LineageRef](https://github.com/a19q3/stem/issues/24) | Latest and Exact encoding, historical proofs and live-write checks are implemented. | Design authenticated dynamic reads with explicit access and freshness and a bounded proof cost. |
| [#25 Logical domains](https://github.com/a19q3/stem/issues/25) | Same-chain deployment membership and issuance provenance form a separate study. | Establish authenticated membership, conservation domain and join/exit rules before experimentation. |
| [#26 Execution pipeline](https://github.com/a19q3/stem/issues/26) | Local native stages have correctness controls and individual short-window results. | Run independent hosts, attribute proposal and commitment cost, and exercise integrated faults under the separate sustainable and overload gates. |
| [#27 Lineage scheduling](https://github.com/a19q3/stem/issues/27) | Independent commands can materialise in parallel within existing dependency and budget barriers. | Compare disjoint, hot and atomic multi-Head workloads with exact ordered effects and rollback. |
| [#28 Keeper](https://github.com/a19q3/stem/issues/28) | Native agenda bounds, retry semantics and scheduled atomic execution have live evidence. | Define larger admissible index sizes, measure due-work fairness and bound skipped and expired work. |
| [#29 Intercell](https://github.com/a19q3/stem/issues/29) | Native authenticated CKB/Stem lanes and bounded message payloads have live evidence. | Measure batched proof and relay cost by source, destination and settlement, with exact replay and acknowledgement. |
| [#30 DEX benchmark](https://github.com/a19q3/stem/issues/30) | Signed native AMM and mixed-script fixtures supply initial baselines. | Pin market and accounting oracles. Measure independent and hot pools, then CLOB operations and fills separately across committee sizes. |
| [#31 Stem Primary](https://github.com/a19q3/stem/issues/31) | Architecture study for sovereign chains and Primary-secured execution domains. | Publish explicit registration, verifier, availability, halt, recovery and CKB role state machines before a prototype. |

## Evidence for each development round

Record the source revision and feature flags, profile, actual offer and observation counts, matching criteria, exact failure and success cohorts, finality verification, restart evidence, and the decision on adoption. Post results and a short reflection on the owning issue. Keep benchmark summaries, corpus fingerprints and replay commands in the tree. The archive inventory records where to obtain historical raw inputs. [Archive guide](benchmarks/README.md)
