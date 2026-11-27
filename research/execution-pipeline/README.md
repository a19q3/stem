# Non-blocking Jolteon candidate preparation

The candidate feature moves follower preparation off the Replica event loop. A dedicated dispatch thread uses the replica's existing Rayon VM pool. The Replica continues handling peer messages, advancing the pacemaker and writing its journal. It schedules only the earliest unprepared candidate on the requested branch, with one slot for the whole job and its result. The existing branch horizon, frame size and native resource budgets also apply. Leader construction, certified RocksDB commit and admission ran synchronously in the original Follower comparison. [Implementation](../../src/jolteon/candidate_pipeline.rs) · [Admission experiment](../admission/pipeline/README.md)

```mermaid
sequenceDiagram
    participant R as Replica event loop
    participant W as Bounded preparation worker
    participant S as Pinned Store snapshot
    R->>W: Exact epoch, finalized base, parent path and signed transactions
    W->>S: Open read snapshot at the bound committed Header
    W->>W: Prepare on immutable speculative ancestors
    R->>R: Continue processing control and timeout events
    W-->>R: Prepared execution or error
    R->>R: Check epoch, base, branch, view, parent, bytes and complete Header
    R->>R: Cache the valid preparation
    R->>R: Re-run existing vote eligibility and persist before signing
```

The worker has no journal, signer or mutable consensus state. It reads an exact committed snapshot with already-prepared immutable ancestors. The Replica discards a result whose epoch or finalized base changed, whose exact candidate or path changed, or whose branch the replica no longer needs. After a view change a verified result can remain useful for an identical ancestor of the live high QC, recovery target or current preferred proposal. Completion never dispatches a vote. The normal tick checks current membership, view, lock, vote, local timeout and timestamp, persists the same journal record, and only then signs. The current Header encoding, Jolteon proof domains and CKB checkpoint witnesses carry through the experiment.

## Regression controls

| Control | Observed outcome |
| --- | --- |
| Exact branch binding | The unit test changes the epoch, finalized point, committed Header, exact parent branch and ordered wire bytes separately. Each mismatch rejects the completion. |
| Concurrent Store snapshot | Commit a new Header while holding the old snapshot. The snapshot still reads the old Header and a fresh snapshot for that stale base is refused. |
| Timeout recovery | Two four-node runs used a leader clock offset of +120 or −120 seconds. Both recovered from view 1 and certified a new payment at height 6. |
| Committee handoff | A funded CKB devnet completed two handoffs. The CKB verifier accepted checkpoints at heights 10, 20 and 25. |
| Mixed versions and restart | Two reference and two background replicas agreed on 96 signed payments after one scheduled leader was stopped and restarted. All four RocksDB stores reopened with the other implementation. The next payment spent an earlier output. WASM verified finality and the original, spent and successor Cell proofs to height 19. |

The locked release suite passed 512 tests with the default build and 515 with both pipeline features enabled. Both runs marked 25 tests ignored. The regression controls exercise independent failure boundaries. Interleavings of completion, timeout and process failure still need fault injection. The synchronous reference CLI also reproduced the two clock recovery proofs and all three execution or handoff proofs from the funded run. A checkpoint for the mixed run at height 17 also entered two CKB node pools over P2P and committed using a previously built checkpoint ELF. [Clock and committee evidence](faults.json.xz) · [Mixed-version restart proof](mixed.json.xz) · [Build and test output](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/execution-pipeline/checks.json.xz)

## Leader proposal construction

`jolteon-proposal-pipeline` runs one Leader construction job per replica, using the existing VM pool. It captures the ordered mempool entries, proposed timestamp, committed Header and prepared parent branch. The Replica rechecks the epoch, view, parent certificate, exact branch, vote lock, vote and timeout journal, and deadline before publishing. An epoch seal uses the existing publication path. The Replica persists the signed proposal and its voting decision before broadcast, then signs the vote. The worker does neither. [Implementation](../../src/jolteon/proposal_pipeline.rs)

| Control | Observed outcome |
| --- | --- |
| Pinned Store | A speculative child built from a pinned snapshot matched serial execution. A job submitted against an old committed Header returned no proposal. |
| Mixed versions and process restart | Two synchronous and two background replicas finalized 96 signed payments. One replica stopped and then caught up. All four agreed at height 11 and the synchronous verifier authenticated the finality proof. |
| Completion after timeout | A two-second delay on the first job of each Leader worker exceeded the 350 ms view timeout. Each worker recorded one discarded result and five published proposals. All four replicas finalized 96 payments and agreed at height 13. |
| Native workload and restart | Four replicas verified Polytype, sponsored OTX, atomic Lineage and Keeper batches. The same run exercised restart and verified scheduled execution proofs. |
| CKB-backed handoff | A funded CKB devnet accepted the existing Checkpoint witness at both handoffs, heights 10 and 20, and the follow-on execution checkpoint at height 25. |

The release workspace passed 517 tests with the Leader, Follower, Admission, certificate reuse and Work Stealing features enabled. The late-completion control uses the [worker-delay shim](delay-proposal.c), built with `cc -shared -fPIC delay-proposal.c -o delay-proposal.so -ldl -pthread`. Run the four-validator `tools/jolteon_localnet.py` fixture with `--cadence 50 --timeout 350 --offline-blocks 8 --transactions 96 --offered 250`. Set `--alternate-binary` to a wrapper that sets `LD_PRELOAD` to the compiled shim and executes the Leader build. The normal `--binary` is the synchronous reference. The report's `execution.proposal_jobs` counters distinguish started, published and stale work.

Leader construction remains selectable. The fault run demonstrates the deadline and journal boundary. Shared worker contention and throughput belong to the combined ablation after State Commitment, with stage timings, stale work and tail latency measured separately.

## Concurrent state commitments

`native-commitment-pipeline` prepares the Cell tree, System tree and execution/receipt record concurrently after ordered execution. It uses the existing pool for blocks with at least 256 execution items when the pool has at least two workers. It joins the results in the existing Cell, System, Header and execution-write order. The durable certified write remains a single batch.

A direct test compared the ordered batch key and value bytes and the Header bytes against serial finishing. A separate integration test prepared 768 signed payments in blocks of 255, 256 and 257 transactions. Serial replay, the Leader and the Follower produced identical Headers and receipts. After reopening RocksDB, the regression checked signed execution bytes, fee accounting and the encoded Cell and fee proofs.

An all-enabled four-replica smoke run offered 26,400 payments at 2,200/s for 12 seconds. All were jointly confirmed within the drain window. Of 87 measured blocks, 36 contained at least 256 payments. The largest contained 1,195. The independent finality verifier and history index check agreed at height 94. This run exercises the threshold on live validators. It supplies no isolated throughput estimate.

The Cell tree recursion is still serial inside its namespace. Stage timing in the planned ablation can show whether Patricia subtree parallelism helps once receipt and System work overlap.

## Four-validator comparison

Four consecutive runs used the same 90,000 signed payments over 30 seconds at 3,000 scheduled writes per second, with 20 seconds of drain. The order was synchronous, background, background, synchronous. All runs wrote every payment inside the offer and commonly confirmed every payment by the drain deadline. The largest paired difference in cumulative writes at any one-second boundary was 31 and 66 payments. Four RocksDB validators shared twelve physical cores and their SMT siblings on one Ryzen 9 7950X3D. The load generator used two other cores. Both implementations used two VM workers per node and the same CPU affinity. [Raw results](results.json) · [Independent certificate replay](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/execution-pipeline/replays.json.xz)

| Run | Common TPS | Write-to-common p99 | Mean peer control wait | Peer control wait >10 ms | Mean candidate preparation | Validator CPU in offer (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| [Sync A1](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/execution-pipeline/a1/payments.json.xz) | 2,702.60 | 4.572 s | 11.98 ms | 18.1% | 56.63 ms | 101.67–102.71 |
| [Background B1](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/execution-pipeline/b1/payments.json.xz) | 2,943.27 | 1.099 s | 2.41 ms | 6.1% | 50.87 ms | 105.95–107.02 |
| [Background B2](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/execution-pipeline/b2/payments.json.xz) | 2,873.80 | 6.821 s | 9.30 ms | 4.2% | 56.39 ms | 108.69–109.90 |
| [Sync A2](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/execution-pipeline/a2/payments.json.xz) | 2,676.93 | 5.687 s | 15.11 ms | 16.5% | 59.15 ms | 101.36–102.49 |

The candidate gained 240.67 and 196.87 common TPS. The mean control wait fell in both pairs, and the share above 10 ms went from 18.1% to 6.1% and from 16.5% to 4.2%. Aggregate validator CPU increased. The write-to-common p99 improved by 3.473 seconds in the first pair and regressed by 1.134 seconds in the reverse pair. No validator persisted a local timeout in the four measured payment runs. The clock controls exercise timeouts separately. B1 applied 726 background results and B2 applied 648. Both sampled queues had at most one unfinished job per node, and these runs recorded no stale or invalid result. The exact binding test covers the mismatch paths. This evidence supports a longer integrated run while keeping the worker opt-in. These 30-second tests measure one host.

## Reproduction

For the published runs, retrieve the linked inputs and put them at their listed paths before running `analyze.py`. [Archive retrieval](../../benchmarks/README.md)

Build both variants from `e387ca8` with the same release toolchain. Enable only `jolteon-candidate-pipeline` for the candidate. The no-feature reference includes the same control queue and timeout instrumentation. Run the candidate with the same VM worker count and CPU affinity as the reference. [Existing external driver](../storage/external/README.md) · [Process accounting](../storage/submission-sessions/cpu.py)

```sh
cargo build --release --locked --bin stem
cargo build --release --locked --bin stem --features jolteon-candidate-pipeline
external=research/storage/external
python3 "$external/drive.py" --harness "$driver" --contracts "$contracts" prepare \
  --topology research/storage/proposal-cadence/100ms-peers.json --trial "$trial" \
  --rate 3000 --seconds 30 --drain 20 --senders 256 --ready-senders \
  --reuse-submissions --complete-volume --block-transactions 2048 --block-cycles 4000000000
python3 "$external/drive.py" --harness "$stock_verifier" --contracts "$contracts" verify \
  --trial "$trial" --report "$destination/payments.json.xz"
python3 research/execution-pipeline/analyze.py
```

The analysis uses the signed archive, initial and final per-node counters, timeout-view snapshots, and process accounting from each run. Capture `member`, `timeout_views`, `last_untimely_view`, `height` and `epoch` from the driver's initial and final status arrays for `views.json`. The mean control wait runs from a complete peer control frame at the transport to the Replica taking that frame. This includes inbound control-queue backpressure. Local timeout counts record persisted local timeout votes. Stale results remain in preparation service timing, which excludes waiting for the main loop to consume a finished result.
