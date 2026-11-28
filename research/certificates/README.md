# Jolteon certificate reuse experiment

The default replica already decodes every QC, hashes its full bytes and caches successful verification in a committee-local bounded set. Committee replacement clears the set. The opt-in feature `jolteon-certificate-experiment` adds two bounded caches. A one-entry memo holds a successful timeout-bearing parent-view verification with the exact proposal view, parent point and full TC bytes. Every call decodes the certificate. A different tuple calls the original verifier. The other cache retains the last canonical framed QC and TC for the 350 ms announcement, comparing the journal body bytes before every reuse. Each slot has a fixed sender and message kind. The epoch transition clears all slots and a restart reconstructs from the authenticated journal. Destinations, timers, vote persistence, quorum checks and proposal execution use the existing paths.

## Outcomes

The native Rust CPU experiment used real Ed25519 signatures and one signed QC embedded in each TC. Six reversed-order pairs each decoded and checked the same certificate 16 times with authenticating once or on every call. At 128 members, median elapsed time was **123.111 ms** for 16 reference checks and **7.318 ms** for one check plus 15 exact hits. The corresponding 4-member medians were **4.362 and 0.275 ms**. This high-reuse fixture isolates the ceiling for that trace.

The live fixture used byte-identical signed payments and genesis within each committee size, deterministic unequal weights, 20 planned payments/s, a stopped leader at size four and two consecutive stopped leaders at larger sizes. All-reference, all-candidate and alternating mixed binaries ran sequentially on one Ryzen 9 7950X3D host with a CPU affinity of 4–31. Every successful run verified its live finality proof; the archive sweep invoked the reference verifier again and compared the reported outcome.

| Replicas | Payments per run | All-candidate exact TC memo hits / checks | All-candidate QC announcement frame hits / encodes |
| ---: | ---: | ---: | ---: |
| 4 | 16 | 0 / 4 | 6 / 25 |
| 16 | 40 | 0 / 16 | 114 / 203 |
| 64 | 64 | 0 / 64 | 7,784 / 4,110 |
| 128 | 64 | 127 / 128 | 25,093 / 16,528 |

The counts sum the final reported per-process counters, which reset on restart. The 128-candidate run also recorded 4,368 TC announcement frame hits and 254 encodes. All three 128-member runs agreed internally at height 129 after restart. Live schedules vary between runs. CPU, process writes, RSS, sent/received bytes, transport drops and admission histograms are archived per node. The fixed arrival target and immutable signed corpus do not make different consensus schedules identical. Host CPU affinity was shared across nodes. The data establish actual cache hits and successful interoperability; they do not establish throughput or overall latency improvement.

The candidate also completed **two CKB-backed committee handoffs** with full replacement and checkpoint publication on CKB 0.210.0. All replicas agreed at height 25. The Rust regressions compare exact frame bytes and allocation reuse, verify rejection/resumption across changed input and committee reset, and recheck the existing predicate on cache misses.

**Decision.** Retain the opt-in research feature for comparison. Announcement reuse recurred across every committee size. Parent-view memoization benefited only some live schedules. Whole-process allocations and independent WAN control latency remain unmeasured.

## Matched sixteen-replica payment bursts

Both release builds used `d227375aaeed47d15390196831de8b59a5da3ec0`, Admission and Follower. The candidate enabled only the additional `jolteon-certificate-experiment` feature. A separate driver build retained the optional cache counters when reading native RPC status. The first 100 ms comparison used a driver that dropped those counters and returned mixed CPU and latency results. The comparison below repeats the four-run order with the repaired driver and the same two validator binaries.

Each cadence used the order reference, reuse, reuse, reference. Sixteen validators and the driver shared logical CPUs `2-7,10-15,18-23,26-31` on one host. Each node used two VM workers, a 256-block retention window, and limits of four billion cycles and 2,048 transactions per block. Each trial wrote the same 9,000 signed payments at 600/s for 15 seconds through 128 persistent senders, followed by 12 seconds of drain. All payments reached common finality in all trials. The native driver authenticated the common finality proof and verified indexed transaction history. CPU and wire counters are before-to-after deltas through drain. RSS is the highest sampled resident memory of a single validator.

| Cadence | Run | Common TPS during offer | Write-to-common p99 | Validator CPU | Mean control wait | Peak sampled RSS | QC hits / encodes |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 100 ms | Reference A1 | 584.87 | 0.511 s | 62.66 s | 1.320 ms | 92.18 MiB | — |
| 100 ms | Reuse B1 | 589.60 | 0.427 s | 61.35 s | 0.885 ms | 90.66 MiB | 0 / 1,205 |
| 100 ms | Reuse B2 | 588.80 | 0.404 s | 62.56 s | 1.179 ms | 91.82 MiB | 0 / 1,216 |
| 100 ms | Reference A2 | 586.07 | 0.457 s | 62.09 s | 0.813 ms | 92.35 MiB | — |
| 550 ms | Reference A1 | 561.27 | 2.586 s | 51.35 s | 1.605 ms | 87.22 MiB | — |
| 550 ms | Reuse B1 | 493.93 | 2.578 s | 51.68 s | 2.583 ms | 86.07 MiB | 542 / 625 |
| 550 ms | Reuse B2 | 525.27 | 2.608 s | 51.42 s | 4.866 ms | 87.68 MiB | 567 / 608 |
| 550 ms | Reference A2 | 549.53 | 1.597 s | 52.28 s | 0.790 ms | 85.22 MiB | — |

At 100 ms the QC advanced frequently enough that the announcement cache never reused a frame. The p99 differences from those runs cannot establish a benefit from reuse. At 550 ms the cache reused 46–48% of observed QC announcements. The reuse builds had lower window throughput and higher control-message wait in both order-balanced pairs, with CPU changing by +0.6% and −1.6%. Aggregate validator transmit totals were 65.97, 56.15, 78.06 and 61.83 MB in run order at 550 ms. No trial at either cadence recorded a local timeout or a parent TC check. This exercises QC frame reuse. The timeout-related part of the feature is covered by the separate fault fixtures. The sixteen-replica mixed restart replay also recorded 64 QC hits and 64 encodes across the eight opted-in processes at the resumed snapshot. [Signed restart regression](../finality/README.md)

Keep the feature opt-in. A repeatable process-level CPU benefit would justify broader measurement. These short trials share hardware, sample RSS and do not measure whole-process allocation counts or WAN tail latency.

The reference binary SHA-256 was `1cedbf66faddde2f6bfdab9b5732c2213f5494d7cec33ec745418cc42f7fef42`. The candidate was `25c6910754ba33733e621d031b4c73dea97e539267ee91074fbd0e9085b2fa46`. The genesis SHA-256 in all eight trials was `a039e672ff2a1f5b28bab34b4eca688fe134db797fc7e9a3b957e7ecf2bad80b`. The signed workload SHA-256 was `78d3058b74fd50377715ba256b5c771a02e479414b1b720e2868529d4496813c`.

For the paired burst, point `STEM_TEST_REPLICA_BIN` at each build and invoke `storage_load::sustained_payment_storage` from the native integration suite. Set `STEM_STORAGE_REPLICAS=16`, `RATE=600`, `SECONDS=15`, `DRAIN=12`, `SENDERS=128`, `VM_WORKERS=2`, `RETENTION=256`, `BLOCK_CYCLES=4000000000` and `BLOCK_TRANSACTIONS=2048`, all under the `STEM_STORAGE_` prefix. Use the indicated `CADENCE` and set `PREFUND_PAYMENTS`, `COMPLETE_VOLUME`, `READY_SENDERS` and `REUSE_SUBMISSIONS` to `1`. Use the same driver with the optional status field in both conditions.

## Matched reuse with two stopped leaders

At `f4f0f94`, sixteen same-host weighted replicas ran a fixed, reuse, reuse, fixed order. Both builds used default parallel Cell commitments. The reuse build added `jolteon-certificate-experiment`. Each trial used identical genesis and 600 signed payments, offered at 40/s. The fixture stopped two consecutively scheduled leaders, waited for all serving replicas to observe all payments plus consecutive timeout certificates, then restarted the stopped nodes. CPUs `2-7,10-15,18-23,26-31` were shared across all processes. The cadence was 550 ms and the timeout 2,400 ms.

| Run | Fault visibility, s | Restart visibility, s | Serving validator CPU, s | Mean fault control wait, ms | QC hits / encodes | TC hits / encodes |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Fixed A1 | 26.457 | 1.162 | 5.63 | 0.459 | — | — |
| Reuse B1 | 26.399 | 1.109 | 5.60 | 0.387 | 756 / 280 | 364 / 56 |
| Reuse B2 | 26.363 | 1.294 | 5.46 | 0.383 | 756 / 280 | 377 / 56 |
| Fixed A2 | 26.444 | 1.173 | 5.64 | 0.385 | — | — |

Counters subtract the pre-fault snapshot for the 14 surviving processes. CPU includes the recovery observation interval for those same PIDs. All runs observed 56 local timeout events and 600 common confirmations. Each opted-in run had 28 parent-view TC checks and zero memo hits. The independent reference verifier checked four saved proofs per run, including the three requested checkpoint heights 23 through 25. Every replica converged at height 25 after restart. Both canonical frame and memo reset regressions passed. Fault, restart and mean control-wait results leave the feature opt-in. CPU moved by −0.5% and −3.2% in the two pairs. Frame reuse avoided encoding work while fault visibility stayed within 0.4%. Whole-process allocations and independent links were outside this comparison.

Use `tools/jolteon_localnet.py` with `--validators 16 --transactions 600 --offered 40 --offline 2 --offline-blocks 16 --cadence 550 --timeout 2400 --recovery-seconds 60 --covered-checkpoint-proofs 3` and the release lock binary. Independently run the fixed binary's `jolteon-proof` on every saved proof and compare the full Header result against the live report. The node SHA-256 fingerprints were `d09f92203af1b5c6c494120ac2989d1da7a362868cac0c120ff312794f9518ef` and `04e0e3c7d08a59e6295583e9d6c84f58844958f751e53986e96ae8448e1baaea` in fixed/reuse order. Genesis SHA-256 was `b5a4ce90ca00b0fee9f6d71de96474278227d5469894d2b3ac4ae53ef8f02e3b` and signed-workload SHA-256 was `76dafd210cdc81a1770fb42ef934d9103e7a5d86e9279aee164c4795d8fd5557`.

## Sharing certificates and direct vote frames

`jolteon-shared-certificate-path` extends frame reuse to the local QC and TC certification paths. The first broadcast uses the bytes that are installed in the journal, and later announcements can reuse that framed allocation. The cache binds its sender's epoch, message kind and complete body. On receipt, byte-for-byte equality with the authenticated current high QC returns its decoded value. A repeated current TC announcement also skips decoding. A different byte sequence uses the existing checks and TC proposal justification still follows the parent-view predicate. The two vote destinations can share one signed transport frame and both receive their own queued send.

The feature retains direct routing, the 350 ms announcement schedule, independent TCP connections, and the existing durability and message formats. The [overlay and repair results](../dissemination/README.md#matched-egress-and-bounded-direct-repair) continue to support direct routing for the native transport. The candidate saves local encoding and verification work. It still transmits the original copies to each peer.

The `certificate_experiment` status adds `current_qc_hits`, `current_tc_hits` and `vote_frame_encodes_saved`. The existing frame counters include initial certification and periodic announcements. Compiled regressions cover strict byte equality, boundary resets, exact signed envelopes, and missing vote routes. Runtime and replay qualification are pending.

Use the same 16-replica corpus at 100 ms and 550 ms, with no feature, with the original experiment only, and with the shared certificate path only. A fourth build can enable both to measure the independent parent-view memo. Replay the stalled-leader and mixed-version restart fixtures and authenticate their common proof and covered heights. Measure validation CPU, allocation counts, control wait and finality as well as the new hit counters. An exact QC hit still clones its typed signatures. Misses and differing arrivals can limit speedup, and retaining an authenticated frame can increase live allocations.

## Unsuccessful runs

The first large run used listener ports inside the host's ephemeral range and some 128-member listeners failed to bind. Subsequent runs used lower ports. Some requests for two exact-height proofs received a later anchor. One request at 64 returned a proof that verified at 65. The matrix uses the fixture's normal common-header and two-chain checks. The historical coverage mode now checks ancestry to a later signed anchor. The exact-height mode remains available for CKB checkpoint fixtures. [Initial failures](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/certificates/initial-runs.json.xz) · [Proof coverage and replay](../finality/README.md)

With the original fixed 75-second observer timeout, 126 serving nodes in the 128-member candidates confirmed all 64 payments before the two stopped leaders appeared again in the schedule. The reference and mixed run hit the same observation deadline. The harness now accounts for a full weighted rotation when computing its observation deadline. Certificate criteria and protocol timing are unchanged. The three retried configurations passed. [Paired and rotation archives](https://github.com/a19q3/stem/tree/12338f2fea73c8df437694da473d08e950a0938b/research/certificates)

## Replay

Build the reference at fefb8cc in a separate checkout. Build the candidate here with the feature. Supply a disposable output directory, two executable paths and the RISC-V stem-lock built per OPERATIONS.md; the fixture requires Python cryptography. The same handoff tool and CKB version appear in OPERATIONS.md. Release database directories after archiving the public output.

```sh
CXXFLAGS='-include cstdint' cargo build --locked --release --features jolteon-certificate-experiment
CXXFLAGS='-include cstdint' cargo test --locked --release --lib --features jolteon-certificate-experiment certificate_experiment
CXXFLAGS='-include cstdint' taskset -c 2 cargo test --locked --release --lib --features jolteon-certificate-experiment repeated_timeout_cost -- --ignored --nocapture
python3 research/certificates/matrix.py --reference /path/to/reference --candidate /path/to/candidate --lock /path/to/stem-lock --output /tmp/stem-cert-run
python3 research/certificates/archive.py /tmp/stem-cert-run /path/to/reference /tmp/stem-cert-run.json.xz
```
