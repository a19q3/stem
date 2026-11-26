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
