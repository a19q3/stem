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

## Unsuccessful runs

The first large run used listener ports inside the host's ephemeral range; some 128-member listeners failed to bind. Subsequent runs used lower ports. The optional request for two *consecutive exact-height* proofs also failed in some runs: for example, a request at 64 returned a proof that verified at 65. These failures are retained in initial-runs.json.xz. The matrix uses the fixture's normal common-header and two-chain checks. This leaves a distinct limitation for consumers requiring an exact proof at every height.

With the original fixed 75-second observer timeout, 126 serving nodes in the 128-member candidates confirmed all 64 payments before the two stopped leaders appeared again in the schedule. The reference and mixed run hit the same observation deadline. The harness now accounts for a full weighted rotation when computing its observation deadline. Certificate criteria and protocol timing are unchanged. The three retried configurations passed. paired.json.xz and rotation.json.xz preserve both series.

## Replay

Build the reference at fefb8cc in a separate checkout. Build the candidate here with the feature. Supply a disposable output directory, two executable paths and the RISC-V stem-lock built per OPERATIONS.md; the fixture requires Python cryptography. The same handoff tool and CKB version appear in OPERATIONS.md. Release database directories after archiving the public output.

```sh
CXXFLAGS='-include cstdint' cargo build --locked --release --features jolteon-certificate-experiment
CXXFLAGS='-include cstdint' cargo test --locked --release --lib --features jolteon-certificate-experiment certificate_experiment
CXXFLAGS='-include cstdint' taskset -c 2 cargo test --locked --release --lib --features jolteon-certificate-experiment repeated_timeout_cost -- --ignored --nocapture
python3 research/certificates/matrix.py --reference /path/to/reference --candidate /path/to/candidate --lock /path/to/stem-lock --output /tmp/stem-cert-run
python3 research/certificates/archive.py /tmp/stem-cert-run /path/to/reference /tmp/stem-cert-run.json.xz
```
