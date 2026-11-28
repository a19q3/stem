# Auxiliary payload dissemination at 16 to 1,024 vertices

The experiment models one unicast copy per destination, matching the destination set of Stem's direct proposal broadcast. Stem shares the encoded frame across peer queues. Its transaction admission relay uses different destinations. Physical NIC limits, packet scheduling and a deployed WAN were not measured here. The simulated overlay edge counts refer to a hypothetical auxiliary network. Retaining Stem's actual direct control connections would retain those connections. [Pinned broadcast implementation](https://github.com/a19q3/stem/blob/fefb8cc01771d59e228c72755ddd1242291f7f22/src/jolteon/transport.rs#L147-L186)

## Topologies and measurements

Start from the order-16 circulant on offsets ±1 through ±4. Double the graph with seeded edge signings. Accept a lift only after its positive and negative nonconstant spectral extrema fall strictly inside ±2√7 with numerical margin. The experiment checks simplicity, degree and connectivity. The search produces specific, reproducible instances. It implements a finite signing search distinct from the September preprint's deterministic construction for sufficiently large orders. All 25 reported lift instances satisfied the numerical bound. The largest observed nonconstant magnitude was 5.289571, against 5.291503. The order-16 base graph is shared across seeds. The independent control is a uniform-degree random graph with its spectrum recorded whether it passes or fails. [Nonbipartite Ramanujan preprint](https://github.com/openai/math/tree/main/preprints/Deterministic-nonbipartite-Ramanujan-graphs-in-every-fixed-degree-September-23-2026)

Seeds 19, 83 and 251 informed the follow-up design. Seeds 409 and 877 were held out for the latency-aware backup tree. At each of five sizes, both graph families use the same origin, four regions, link latencies, host rates, absent vertices and payloads. Baselines include direct, first-receipt flooding, BFS, propagation-delay shortest paths and a random Kruskal tree. Other candidates minimize maximum tree degree among twelve trees, add a second low-overlap tree, or add a second shortest-path tree with an 80 ms reuse penalty. All trees use edges of the same underlying graph. Flooding on a union forwards once to each neighbor except the first sender. Duplicate receipts do not trigger another forwarding wave.

One-way delay is 2–3.6 ms within a region and 29–44.6 ms across regions. Each sender has a fixed 5,000, 12,500 or 25,000 bytes/ms egress rate. One model shares the rate across destinations. A second gives every peer an independent equally fast link. The offline profiles remove 10% of sampled nodes or one of the four regions. Coverage is reported as live peer count and synthetic live stake, with weight 8 in one region and 1 elsewhere. An ideal post-pass charges one direct copy per missed live peer. The model assumes the origin remains available and knows whom to repair. TCP, control traffic, signing, burst contention across distinct payloads and dynamic membership are outside this measurement.

## 1,024 vertices, 64 KiB, healthy

Each cell gives held-out seeds 409 / 877. Recipient p99 uses nearest rank within each run. MiB counts all initial cross-region payload copies.

| Route | p99, shared egress (ms) | p99, independent links (ms) | Cross-region MiB, shared egress |
| --- | ---: | ---: | ---: |
| Direct | 2,686 / 5,339 | 47 / 50 | 48.0 / 48.0 |
| Degree-eight lift, flood | 178 / 194 | 120 / 119 | 352.0 / 350.1 |
| Random regular, flood | 203 / 201 | see archive | 350.4 / 351.1 |
| Single shortest-delay tree | 283 / 306 | 159 / 146 | 14.9 / 15.8 |
| Degree-balanced tree + low-overlap backup | 264 / 274 | 213 / 225 | 154.5 / 152.0 |
| Two latency-aware trees | 190 / 208 | 129 / 129 | 104.1 / 103.4 |

A healthy flood sends 448.06 MiB of total payload; direct sends 63.94 MiB. Its origin sends eight copies instead of 1,023. At ten percent offline the lift flood needed zero repair copies in both held-out runs; the shortest-delay tree needed 306 / 240 and the latency-aware pair needed 1 / 1. With one region offline, the latency-aware pair needed 2 / 12. The pair reduced healthy p99 and cross-region volume relative to the low-overlap pair in both held-out cases. Aggregate p99 can conceal a few isolated recipients; inspect coverage and repair copies alongside latency.

## Cut analysis

We enumerated all 32,767 nontrivial cuts up to complement at 16 nodes. The base graph has edge expansion 2.5. The degree-balanced tree has maximum tree/graph cut ratio 0.438–0.462 over the five seeds, compared with 1.0 for BFS. On larger graphs the archive records the maximum over singleton, region, sampled neighborhood and random cuts. These maxima are lower bounds on the worst tree ratio. For graph expansion the sampled minimum is an upper bound, with a separate spectral lower estimate. The sampled tree family optimizes a singleton-cut proxy. The polynomial strong-thin-tree algorithm uses a universal constant and a cut guarantee; a finite degree heuristic supplies neither its construction nor its guarantee. [Strong thin-tree preprint](https://github.com/openai/math/tree/main/preprints/A-polynomial-time-construction-of-strong-thin-trees-September-23-2026)

**Decision.** The direct/overlay latency ranking reverses when the egress assumption changes. A real measurement of sender sharing and auxiliary bytes is necessary before an opt-in network trial. For tree research, the latency-aware pair is the better simulator candidate for these two held-out profiles. Coverage still requires repair. Keep production routing and finality as implemented.

## Replay

Python 3.15.2 on x86-64, dependency versions in requirements.txt. The archive includes per-topology construction wall times (regular generation plus spectrum, or one accepted lift stage including search), seeds, graph hashes, spectra, cut estimates and 4,800 traffic rows. Graph construction times are host dependent.

```sh
uv venv --python 3.15 /tmp/stem-graphs
uv pip install --python /tmp/stem-graphs/bin/python -r research/dissemination/requirements.txt
OPENBLAS_NUM_THREADS=1 /tmp/stem-graphs/bin/python research/dissemination/simulate.py --seeds 19 83 251 409 877 > /tmp/stem-graphs.json
/tmp/stem-graphs/bin/python research/dissemination/verify.py /tmp/stem-graphs.json
```

## Persistent TCP sockets and four-payload bursts

The follow-up sent 64 KiB messages over real IPv4 loopback sockets at 128 and 1,024 logical nodes, with the same two held-out graph seeds, origins and 10% omission sets. Each undirected live edge uses one full-duplex TCP connection. The direct baseline opens just the origin's star, enough for this single-origin experiment. All connections are established before timing. A single pinned Python selector loop processes the endpoints, verifies each complete payload and forwards on first receipt. Every endpoint/direction holds at most one pending frame per distinct message. Payload byte counts exclude transport headers. For each case the harness compared delivery counts against the root's connected component and accounted for every copy, including duplicates.

At 1,024 healthy nodes, each 64 KiB message cost 63.94 MiB direct, 448.06 MiB on the flood, and 186.19 / 188.31 MiB on the delay-aware tree pair. Across three four-message bursts, median recipient p99 for the fourth message from burst launch was **90.9 / 89.6 ms** direct, **678.0 / 686.8 ms** flooded, and **305.9 / 269.3 ms** on the tree pair. Each pair is seeds 409 / 877. With 102 absent, the direct and flood paths reached all 922 live nodes; the tree pair reached 921 in both cases. No synthetic repair was included. Graph, queue and recipient counts are comparable to the simulator; the host's NIC, RTT diversity and independent endpoint CPU costs require a multi-host trial.

```sh
OPENBLAS_NUM_THREADS=1 taskset -c 2 python3 research/dissemination/loopback.py > /tmp/stem-tcp-bursts.json
```

The archive loopback.json.xz includes all 144 trials, per-message p99 and last receipt, queue maxima, unique coverage, duplicates, payload bytes and process resource usage. Timing excludes connection setup.

## Matched egress and bounded direct repair

At 128 logical endpoints, seeds 409 and 877 used the same lifted graph, root, four-payload burst and region labels in each route comparison. The fixture ran direct, flood and the latency-aware tree pair twice in opposite orders with three endpoint sets. Random loss omits 12 endpoints, and correlated loss omits all 32 members of a non-origin region. The three routes use the same live endpoint population in each row.

Each 64 KiB payload traverses established local TCP sockets. Release times model the archived one-way link delays and 5, 12.5 or 25 MB/s sender rates. The two controls share that rate across a sender's destinations or apply it separately to each direction. Direct repair uses preconnected origin sockets. At 500 ms plus a deterministic stagger of up to 39 ms, a recipient missing a payload emits one modeled 96-byte request, with one-way delivery delay. The origin can send at most 16 repairs per payload under the same egress cap. A repaired recipient can forward through its existing overlay edges. Every data frame is checked against its expected SHA-256 digest. The request timing and bytes are modeled, and neither validator authentication nor deployment socket savings are measured.

| Healthy egress | Direct p99 | Flood p99 | Tree-pair p99 |
| --- | ---: | ---: | ---: |
| Shared per sender | 1,344–2,661 ms | 257–261 ms | 242–259 ms |
| Independent per destination | 57–66 ms | 189–204 ms | 112–137 ms |

Each range pools two measured repeats for each seed. Recipient p99 pools delivered payloads from the four-message burst and excludes the origin. In healthy shared-egress trials, the origin sent 31.75 MiB direct and 2 MiB with either overlay. Total payload bytes were 31.75 MiB direct, 224.25 MiB flooded and 89.8–90.2 MiB for the tree pair, with 24.0, 172.7–173.6 and 44.8–48.5 MiB crossing modeled regions. Direct delivery missed the 1.5 s deadline on the lower-rate seed in every shared-egress profile, while both overlays met it. All direct sends eventually drained.

For seed 877 the tree pair needed one repair per payload with random omission and five per payload when a region was absent. It needed zero for seed 409. All repaired payloads arrived by 1.5 s, and the largest correlated-loss tree-pair p99 was 626 ms. Flooding required zero repairs and used 118.8–123.2 MiB under region omission. The tree pair used 43.8–60.2 MiB there including repairs. Independent replay reconstructed the initial overlay connected component for every trial, checked the early receipt counts against that component, and verified capped repairs, origin copies, delivered counts and request accounting across all 72 results.

**Decision.** Retain direct Stem routing. The tree pair is a useful auxiliary control when origin egress is a demonstrated bottleneck and it saves aggregate traffic compared with flooding. It spends materially more bytes than direct, needs repair under correlated loss and loses to direct in both independent-link seeds. The comparison runs one loopback selector and assumes a live origin. A production relay would need measured deployment egress, authenticated requests and independent links before reconsideration.

```sh
OPENBLAS_NUM_THREADS=1 taskset -c 8 python3 research/dissemination/egress_repair.py \
  --nodes 128 --seeds 409 877 --repeat 2 > /tmp/stem-auxiliary-egress.jsonl
```

## Reusing the direct proposal body and frame

With `jolteon-share-proposal-bytes`, the Leader serializes a local proposal once and uses the resulting immutable body for the vote journal and the first broadcast. The transaction slices in the Leader's cached proposal use that body as their backing storage. A received or restored proposal still passes canonical decoding and the existing consensus checks. Its validated bytes then serve vote persistence, certified writes and handoff persistence. The operation of the synchronous WAL, the journal keys and the protocol encoding are unchanged.

After a successful vote write, the Leader retains one direct transport frame for its own proposal. A periodic announcement can reuse that frame only for the same epoch and voted point. Advancing the view releases the frame. On a cache miss or after restart, the Leader reads the durable proposal body and frames it through the existing transport. Other validators skip the Leader's proposal lookup. Vote delivery, destinations, timers and historical GET repair use the existing paths.

The regression covers byte equality for local and received bodies, shared transaction allocation, rejection of noncanonical bytes and the certified writer's packet body and journal visibility. Compile checks cover the default and optional configurations. Runtime regression, independent replay and throughput measurements are pending. Run `cargo test --locked --release --lib --features jolteon-share-proposal-bytes canonical_frame_and_proposal_share_the_validated_transaction_bytes` and `cargo test --locked --release --lib --features jolteon-share-proposal-bytes,jolteon-commit-pipeline certified_write_moves_the_durable_header_and_journal_together`.

For a matched comparison, add only this flag and use the same transaction corpus, peer authentication, cadence, loss schedule and limits. Record build time, control latency, journal and finality write timings, peak RSS, process CPU and `execution.proposal_retry_frame_hits`. Include short healthy views, stopped-leader runs that exercise retransmits and mixed-version restart. Independently check the finality proofs and covered Headers. The first-send path saves serialization work even when a view has no periodic retry. The frame cache retains one proposal frame during the active view, which adds resident memory once peer queues drain. The receiver still allocates for the canonicality check and RocksDB still copies bytes into its write batch. The routing comparisons above continue to apply to the wire traffic.
