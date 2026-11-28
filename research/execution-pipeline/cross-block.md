# Native cross-block proposal construction

In the old pipeline, the Replica handled a new quorum certificate, wrote the preceding certified block, and waited for the proposal interval before it started constructing the next proposal. The overlapping Follower path also needed another Leader's proposal to arrive while the local write was still active. A 100 ms proposal interval and shorter writes made that opportunity rare under normal load.

The `jolteon-cross-block-pipeline` feature enables the Follower, Leader and certified writer stages. On an ordinary single-block commit, the next Leader begins construction against the certified parent branch while the writer persists the earlier block. When a quorum for block N finalizes N−1, the two active tasks are the write for N−1 and construction for N+1. N is the already prepared parent. Proposal publication remains paced and requires the acknowledged durable prefix, the current epoch and view, the same parent and surviving branch, and a timely execution Header. Commit, vote and publication retain their existing journal ordering. Boundary seals, recovery and multiple-block commits use the established paths.

When the worker opens its snapshot before the write, it uses the original base plus the prepared branch. When the same write wins the race, it can open the exact first committed Header and use the remaining suffix. The gate compares the new committed Header bytes and the first prepared block's old parent. The completion then compares the consensus point and surviving branch again. The same snapshot-acquisition rule covers a Follower scheduled by the writer. A completed Leader result occupies the one bounded proposal slot until it is published or discarded.

The status counters record the time in both a writer job and an accepted Leader or Follower computation by intersecting their monotonic intervals. In the final build the compute interval starts inside the Rayon task, after pool admission. Queue time belongs to the existing proposal and Follower latency metrics. Leader overlap is counted at publication. Follower overlap is counted when the verified result is accepted by the Replica. The report treats those counts separately. Stale counters track discarded completed computations and their associated worker-job wall time, including any wait for the pool. Worker off-CPU time comes from the named Rayon threads' user and system CPU over the same window. It includes blocking, waiting for work and OS scheduling.

The changes preserve Stem's execution-bound votes, Header format and finality proof rules. These results measure one host.

## Matched pipeline ablation

Four static validators and an independent driver shared a Ryzen 9 7950X3D. Validators used logical CPUs `2–7,10–15,18–23,26–31`. The driver used `0–1,16–17`. Each replica used two execution workers, a 256 MiB block cache, 131,072 verification entries, 100 ms cadence, a 2,400 ms timeout, 4B cycles and 2,048 items per block. Every build used the current VM trace implementation, optional Cell prefetch and default parallel Cell commitments.

The reference used synchronous consensus and execution. The old integration additionally used Admission, Follower, Leader, Commit, concurrent commitments, Work Stealing and certificate reuse, with Commit pulling in the Follower feature. The early integration adds the cross-block change to that exact combination. The focused build uses the cross-block feature with its three required stages. Eight runs used a fresh store each and the same 160,000 signed payments at a 4,000/s offer. The order was reference, old, early, focused, focused, early, old, reference. Each ran for 10 s of warmup, 30 s of measurement and a 30 s drain.

| Run | Common TPS | p99 to common | Leader mean | Follower mean | Control queue mean | Validator CPU | Reused overlapping Leader jobs |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Reference 1 | 3,995.87 | 2.217 s | 57.46 ms | 131.25 ms | 10.82 ms | 128.30 s | 0 |
| Old integration 1 | 3,966.13 | 1.292 s | 67.77 ms | 124.06 ms | 3.63 ms | 138.65 s | 0 |
| Early integration 1 | 3,994.90 | 0.766 s | 50.88 ms | 84.24 ms | 2.10 ms | 141.26 s | 135 |
| Focused 1 | 4,007.20 | 0.903 s | 54.72 ms | 100.77 ms | 1.82 ms | 139.66 s | 123 |
| Focused 2 | 4,008.03 | 1.015 s | 56.81 ms | 104.54 ms | 1.38 ms | 139.74 s | 119 |
| Early integration 2 | 4,007.23 | 0.805 s | 51.93 ms | 86.85 ms | 1.34 ms | 141.92 s | 132 |
| Old integration 2 | 4,016.03 | 1.087 s | 64.79 ms | 119.31 ms | 1.99 ms | 137.47 s | 0 |
| Reference 2 | 3,971.17 | 2.094 s | 55.70 ms | 125.94 ms | 10.84 ms | 128.53 s | 0 |

Every run drained all 160,000 accepted payments and independently replayed its archive. There were 3,038 replayed blocks across these eight runs. The common signed-workload identifier was `1790b0a228937979a32fcbc03fbbba0a0155ddd6197e7c311392cc0801b0802a`. Each p99 covered 120,000 acknowledged and confirmed writes. All candidate and proposal stale counts and local timeouts were zero in the measurement windows. Worker off-CPU time was 71.4–73.4%. The construction measurements include the pool wait in this initial run. The 509 overlapping Leader jobs all yielded accepted proposals. There was one nonempty block at the 2,048-item limit across the eight measurement windows. Near the offered-rate ceiling, lower p99 in both run orders provided the useful signal. The mature-state comparison below tests the loaded regime and instruments the execution interval separately.

## Matched mature payment pressure

Every arm started from reflinks of the same stopped four-replica database snapshot after funding 1,440,000 Cells. The fixture checks the complete set of signed funding transaction IDs, the funded Header on the archive and the expected committed count on every replica. Recent replicas establish agreement at a shared retained height during preflight. The control and focused configurations used the same placement and runtime settings as above. They offered 240,000 payments at 4,000/s with 20 s warmup, 40 s measurement and 35 s drain. The order was reference, focused, focused, reference.

| Run | Common TPS | p99 to common | Leader mean | Follower mean | Control queue mean | Validator CPU | Full blocks | ACKs pending at cutoff |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Reference 1 | 3,072.0 | 63.01 s | 200.08 ms | 333.30 ms | 52.15 ms | 224.12 s | 60 | 0 |
| Focused 1 | 3,328.0 | 59.62 s | 226.88 ms | 321.05 ms | 3.61 ms | 253.47 s | 65 | 0 |
| Focused 2 | 3,225.6 | 62.05 s | 230.33 ms | 326.29 ms | 5.25 ms | 250.01 s | 63 | 0 |
| Reference 2 | 3,020.8 | 66.29 s | 203.62 ms | 340.20 ms | 55.00 ms | 221.39 s | 59 | 6,933 |

Common TPS improved by 8.3% and 6.8%, with validator process CPU up 13.1% and 12.9%. The p99 is for measurement-window writes with an ACK and common confirmation by cutoff. The four populations were 159,950, 159,612, 157,293 and 155,295. Backlog grew during measurement in every arm. All observed nonempty measurement blocks were full. The focused builds moved work off the control loop while using more time per Leader build. Worker off-CPU time was 63.8%, 61.6%, 61.9% and 64.2%, respectively. None of the arms recorded a local timeout during measurement. These are overload outcomes.

The signed workload ID was `7e67e746f550645301fd5ec515045480e6ef67446375dcbf24359f35350e34c1`. Independent replay checked the ordered transaction roots, receipt samples and terminal certificates for 613 observed blocks. The first collector retained the old Status schema and omitted the new overlap fields. The throughput, latency and existing stage counters were available in that schema. The instrumented pair below uses the current schema and isolates the early scheduling change inside the same three-stage configuration.

## Instrumented scheduling-only pair

The final replica binaries held the same Follower, Leader and certified writer stages enabled in both arms. Only `jolteon-cross-block-pipeline` changed. The driver rebuilt its Status reader with the new fields and reused the same funded snapshot, signed workload, windows and core placement. Order was paced construction followed by early construction.

| Measurement | Paced construction | Early construction |
| --- | ---: | ---: |
| Common TPS | 2,713.6 | 2,969.6 |
| Write-to-common p99 | 66.86 s | 64.24 s |
| Writer service, mean | 92.19 ms | 93.68 ms |
| Leader job, mean | 219.79 ms | 222.75 ms |
| Follower job, mean | 334.55 ms | 335.92 ms |
| Control queue, mean | 9.06 ms | 18.37 ms |
| Validator CPU | 215.51 s | 233.59 s |
| Execution worker off-CPU time | 67.7% | 64.8% |
| Early jobs started | 0 | 58 |
| Early results published after exact rebase | 0 | 58 |
| Useful compute/write overlap | 0 | 4.114 s total, 70.93 ms per published result |
| Stale Leader and Follower jobs | 0 | 0 |
| ACKs pending at cutoff | 4,990 | 0 |

The additional pair saw longer writer service in both arms than the primary pressure comparison. Within this pair it stayed within 1.5 ms, and average Follower time was nearly flat. The 9.4% throughput difference coincided with actual successor work during the writer interval. Aggregate CPU rose 8.4% and control queue mean increased. The p99 populations were 141,198 and 154,193. Both arms independently replayed their archives, totaling 246 certified blocks. The early arm reused all 58 measured jobs. Neither arm recorded Follower overlap during ordinary writes because it received the next proposal after acknowledgment. The paused-writer control exercises Follower overlap separately.

## Mixed Type Groups

Both configurations acknowledged and finalized all 55,000 signed transfers in the established zero-to-twelve-Type-Group fixture. The rate was 2,200/s for 25 s followed by a 20 s drain. The reference and focused common rates during the offer were 2,133.28 and 2,141.32 TPS. The write-to-common p99 went from 1.491 s to 1.134 s. Mean control queue time went from 10.37 to 2.92 ms. Validator process CPU increased from 85.99 to 93.93 s. Independent replay checked 271 and 258 certified blocks. The signed workload ID was `85f80e6e8a2448944da69b7faaa3e662d6e8506c12749a0dc2ed4aa1ba7f652e`.

## Crash and recovery checks

The locked release workspace passed 545 tests across 26 suites with all features enabled. There were 30 explicitly ignored fixtures. The focused regressions verify the same Header, receipts and write batch when the snapshot opens before or after the exact scheduled commit. A sibling fork, an unscheduled prefix and a two-block advance are rejected. The Leader binding test also exercises epoch, view and exact Header changes.

The Linux commit probe ran before writing, before WAL sync, after WAL sync and with a paused sync released. The target used the focused build and its peers used the synchronous reference. Each run submitted 12 payments during the pause and finished with 24 payments confirmed across all four replicas. The pre-acknowledgment Header and certificate remained gated. The before-write crash reopened at height 3. Both sync-boundary process-crash cases reopened at height 4. The before-sync result reflects the operating system page cache. Releasing the paused writer reused one verified Follower preparation with 2.254 ms of measured compute/write overlap. The synchronous verifier independently authenticated the resulting common Header in every run. [Writer fault fixture](README.md#integrated-commit-faults-with-live-payments)

A mixed-version late-completion run delayed the first construction on each focused replica by two seconds against a 350 ms timeout. Each discarded one stale result. The replicas confirmed all 96 signed payments and agreed at height 14 after one replica went offline and restarted.

## Build and replay

The feature stays opt-in. The measured focused configuration is:

```sh
cargo build --release --locked \
  --features native-candidate-cell-prefetch,jolteon-cross-block-pipeline --bin stem
```

For a scheduling-only control, substitute `jolteon-commit-pipeline,jolteon-proposal-pipeline` for the cross-block feature. Omit the pipeline flags for the synchronous Cell-prefetch reference. For the integrated control, add `jolteon-admission-pipeline,native-commitment-pipeline,native-work-stealing,jolteon-certificate-experiment` to the paced configuration. Use the current checkout's release `native_store` test executable for the [driver and independent verifier](../storage/external/README.md#measure-and-replay), including the current Status fields. For the mature snapshot, use the [fixed funding fixture](../saturation/README.md#实验条件与复现) and stop all four validators after funding and before dispatch. Reopen independent copies for each arm and verify the funded Header and paid count before starting the timed offer.

Final release binary SHA-256 values were `33d490d9ac76a671fa040e125b3db7f00743cd7ccdf9ac843a3b8317aa800368` for the synchronous reference, `f7cd758ed57f944fc70043775dd9fb0285de0e6ffaa7d4e1bbf263e5b6e57efc` for the paced three-stage control and `c39a0af5749881d21caac429300c6a3da92bbb594690d5034c35ef8ffac47c95` for the focused candidate. Reported native performance remains bounded by execution before voting, plus the consensus cadence and persistence budget.
