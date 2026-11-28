# Proposal-to-quorum timing on Wi-Fi and an emulated LAN

The [first two-host run](../storage/two-host-validation/README.md) used
direct Wi-Fi and observed a slower block cadence than the same signed
workload on one host. The per-node execution counters started after
transport. This follow-up records the missing send, receive and vote
boundaries by exact epoch, view, parent view and Header ID.

Set a separate, new `consensus_trace_path` in each replica's JSON
configuration. The trace uses the local monotonic clock. The file
starts with the wall-clock synchronization bracket. A bounded queue
feeds the diagnostic writer and the file caps at 262,144 events per
replica. A full queue drops observations and updates trace counters.
With tracing enabled, status adds per-kind wire frame, byte and drop
counters.

Use one topology file per arm with the load driver's real endpoints.
Build the native store harness with `cargo test --locked --release
--test native_store --no-run`. Generate the trial with the same
harness and lock binary throughout. [Distributed load driver](../storage/external/drive.py)

```sh
python3 research/storage/external/drive.py --harness "$HARNESS" --contracts "$CONTRACTS" \
  prepare --trial "$TRIAL" --topology "$TOPOLOGY" --rate 3000 --seconds 60 \
  --drain 35 --senders 256 --funded-cells 1440000 --block-transactions 2048 \
  --block-cycles 4000000000 --ready-senders --reuse-submissions --complete-volume
```

With the replicas running, use `measure --fund-only` once and save the
emitted funding JSON. Stop the replicas and take a copy of each store.
Restore these stores before each measured arm and use
`measure --reuse-funding "$FUNDING" --output "$RESULTS"`. Pass that
arm's report to a separate `verify --report "$REPORT"` invocation.
Supply the same `--harness`, `--contracts` and `--trial` arguments
for each phase.

Transport stamps mark local queue admission, dequeue, kernel write
entry and completion, and complete receipt before wire
authentication and decoding. Consensus stamps mark candidate
computation, the durable vote, counted shares and formed or durably
learned quorums. A kernel write completion is a sender-side event.
The receiving replica's timestamp identifies delivery.

Run `analyze.py --traces <directory> --report <signed payment report>`
with four `trace-0.jsonl` through `trace-3.jsonl` files. The script
times the Leader's own durable proposal through durable observation
of a QC on that same monotonic clock. For an unambiguous closing
remote share, subtract the Follower's read-to-vote-write duration
from the Leader's proposal-write-to-vote-read interval. This yields
the combined wire-path residual without a wall-clock offset.
Repeated writes that could have caused the same received frame are
excluded from the paired estimate. The count of exclusions is part of
the evidence. Cross-host one-way bounds use the pre- and post-run
clock probes in `clock-samples.json` and assume the wall clocks
remain within those bounds during the run.
For the direct Wi-Fi fixture place replicas 0 and 1 on the driver
host. Around the run, sample the other host's Unix timestamp 16
times through an existing SSH connection. Record the local Unix
nanoseconds before and after each remote `date +%s%N` in the
`pre` and `post` arrays as triples `[before, remote, after]`.
The one-host proxy fixture uses a zero cross-replica clock offset.

## Experimental settings

Start all arms from copies of one authenticated 1,440,002-Cell state.
Use 768 MiB for the node index, 1,536 MiB for the Live Cell index,
256 MiB for the RocksDB block cache, a 16 MiB mempool and two VM
workers. Keep a 2,048-transaction and four-billion-cycle block cap
with a 100 ms proposal interval and 2.4 s view timeout. Drive
3,000 signed payments/s for 60 seconds and drain for 35 seconds.
Score seconds 20–60 and replay each captured archive independently.

The physical arm uses two validators on the 7950X3D and two on the
13900H. Workstation validator masks are `2–7,18–23` and
`10–15,26–31`, and laptop masks are `0–5` and `6–11`.
Validator messages travel directly between the hosts. In the
emulated arms all four validators share the workstation's twelve
physical validator cores. The load driver keeps its separate
physical cores `0,1` and their SMT siblings in all arms.

In the loopback fixtures, redirect only peer connections crossing
the virtual split through `emulate.py`. Supply distinct loopback
listen ports mapped to each destination's real port. Keep each
replica's own bind address and the load driver's endpoints
direct. The pass-through and shaped runs use the same proxy.
Use `--delay-ms 20 --jitter-ms 12 --mbps 70 --seed 4357` for
the shaped arm. The proxy serializes traffic against a shared
bidirectional link budget and preserves each TCP stream's byte
order. It models a controlled propagation, jitter and bandwidth
constraint at the application-byte boundary. Hardware and OS
scheduling remain part of the observed run.

For example, bind the four validators to ports `29611 + i`. In
validator `i`'s configuration, use `30200 + 4*i + j` for peer `j`
only when they are on opposite sides of the split. The eight route
entries are `[30200 + 4*i + j, 29611 + j]` for those pairs. Keep
the local addresses and the load driver's topology direct. Start
the proxy with `python3 research/consensus-transport/emulate.py
--routes "$ROUTES"`, adding the shaping parameters for that arm.
Pin the proxy to cores outside the validator and driver masks.

## Proactive retries

The existing announce tick attempts a full proposal resend while
the proposing validator remains in that view. Its payload shares
the peer's control TCP stream with small consensus frames. On
the Wi-Fi arm, every extra payload consumes airtime even if its
recipient has already started independent verification.

The candidate controls the minimum interval between full
proposal broadcasts for one epoch and Point. Add
`"proposal_retry_interval_ms": 1000` to the four replica
settings. Omitting the field selects the existing schedule for
the control arm. The setting schedules proactive retries on the
existing 350 ms tick. The initial broadcast and on-demand GET
repair still bypass the spacing check. The votes, QC and timeout
announcements retain their cadence. The timer stores one Point
and elapsed-time marker per replica, and restart clears it.

## Matched 3,000 payments/s runs

Each arm used the same signed workload and held the same Cell and
System roots at the start of the offer. Empty preflight blocks
advanced the starting Header. The first three arms used the
instrumentation build. The remaining four used the identical
retry-capable binary with the setting toggled. The physical arm
order was control, candidate, control, candidate.

| Network and setting | Common TPS | Accepted pending at 20 → 60 s | Conditional p99 | Proposal bytes | QC p50 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Wi-Fi A, control | 2,406 | 23,166 → 46,907 | 46.40 s | 216.0 MB | 655 ms |
| Proxy pass-through, control | 3,006 | 1,587 → 1,373 | 0.70 s | 100.0 MB | 74 ms |
| Proxy shaped, control | 2,988 | 2,015 → 2,435 | 1.31 s | 123.9 MB | 143 ms |
| Wi-Fi A, 1 s | 3,041 | 4,349 → 2,743 | 2.18 s | 89.2 MB | 257 ms |
| Proxy shaped, 1 s | 2,994 | 1,881 → 2,110 | 1.30 s | 88.3 MB | 144 ms |
| Wi-Fi B, control | 1,587 | 15,233 → 71,723 | 51.29 s | 192.9 MB | 965 ms |
| Wi-Fi B, 1 s | 3,035 | 5,174 → 3,711 | 1.88 s | 88.1 MB | 253 ms |

Common TPS uses a fixed 40-second wall-clock denominator.
Confirmations can include work accepted before the scoring window.
Conditional p99 covers writes in that window acknowledged and
confirmed by the 95-second cutoff. The B control recorded 179,975
acknowledgements out of 180,000 writes. All other arms acknowledged
all 180,000. Every acknowledged payment confirmed by the cutoff, and
both indexes remained active throughout. The B control recorded
eight local view timeouts. The other arms recorded zero in the
scoring window. Successful application-frame bytes sum across all
four replicas and exclude TCP/IP and Wi-Fi framing.

Extra successful writes accounted for 285 of 426 proposal copies
in Wi-Fi A and 279 of 378 in Wi-Fi B. These counts cover proposals
prepared during the scoring window with an encoded frame of at
least 1,024 bytes. Cross-host extra copies contributed 95.4 and
92.9 MB, respectively. The matching candidate arms wrote one
copy per peer for every scored proposal. Transaction relays
also fell from 74.2 and 78.6 MB to 46.0 MB in both arms. Relay
policy was held fixed, so this is a measured consequence of
the overall change in execution and traffic.

| Closing share path | Unambiguous samples | Wire residual p50 / p95 | Follower compute p50 / p95 |
| --- | ---: | ---: | ---: |
| Proxy pass-through | 210 | 0.34 / 0.51 ms | 57 / 70 ms |
| Proxy shaped, control | 89 | 75 / 96 ms | 66 / 88 ms |
| Wi-Fi A, 1 s | 107 | 141 / 229 ms | 96 / 127 ms |

The wire residual includes kernel and link transfer in each
direction. In Wi-Fi A with 1 s spacing, p95 was 0.10 ms for
Leader outbound queueing, 0.29 ms for Follower inbox wait,
10.27 ms from Follower handling to computation and 15.27 ms
for durable vote persistence in the paired sample. The
proposal-only cross-host median bound was 99–149 ms. The
control had only one unambiguous closing share in Wi-Fi A
and none in Wi-Fi B, while the Leader-local QC medians cover
47 and 32 proposals. The cross-host ICMP p95 was 214 ms in
the B control and 51–76 ms in the two candidate arms. These
concurrent probes reflect both ambient Wi-Fi conditions and
contention from the workload.

The shaped proxy reproduces a large increase in wire time
and a 29% reduction in proposal bytes from retry spacing.
It retains enough capacity for both configurations at this
load. The physical comparison shows a larger latency and
backlog effect, on one Wi-Fi deployment with variable link
conditions. [Per-arm timings, replay summaries and digests](results.json)

## Higher offer rates on physical Wi-Fi

Keep the one-second proposal setting and use a 120-second
offer. Score seconds 30–120 and allow the same 35-second drain.
Both measured arms restarted from the same authenticated funding
image. An interrupted 4,000/s attempt ended before report capture
and the complete rerun supplies the evidence below.

| Scheduled offer | Actual writes/s | Common TPS | Accepted pending at 30 → 120 s | Conditional p99 | Confirmed / accepted at cutoff |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 3,500/s | 3,500.1 | 2,912.7 | 17,738 → 70,626 | 98.67 s | 420,000 / 420,000 |
| 4,000/s | 3,999.8 | 2,890.0 | 35,658 → 81,736 | 105.41 s | 426,041 / 426,041 |

These arms establish overload at both rates. The 4,000/s offer
wrote 480,000 requests and acknowledged 426,041. The two
shorter 3,000/s runs stayed close to balance under the same
configuration. This testing brackets the operating point on
the sampled Wi-Fi link and leaves longer-duration qualification
at that point open.

At 4,000/s the closing-share wire residual had a median of
354 ms versus 189 ms of Follower computation across 117
attributed proposals. Proposal retries produced two extra
copies among the scored proposals. Replica status recorded
249,169 transaction relay retry attempts in the 90-second
window, compared with 63 and 249 attempts in the 40-second
3,000/s candidate windows. The retransmission count identifies
relay recovery traffic as a separate bottleneck candidate. An
independent control with the relay retry policy varied is
needed to estimate its throughput impact.

The one-second proposal interval remains opt-in. The sampled
networks establish repeated gains under direct Wi-Fi and lower
bytes under emulation. Sustained packet loss and larger
committees need separate liveness measurements before adopting
the setting as a default.
