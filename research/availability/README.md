# Equal-budget comparison of Reed–Solomon and local parity repair

The higher-dimensional coboundary-expansion preprint proves a family of simplicial complexes. An implementable storage design would additionally need an explicit code, encoder, decoder, concrete parameters, proof binding and retrieval assumptions. Locally testable codes concern proximity to a code; a responsive sample also requires assumptions about who can supply the remaining data. [Coboundary expanders](https://github.com/openai/math/tree/main/preprints/Bounded-degree-coboundary-expanders-in-every-dimension-September-24-2026) · [Dinur et al.](https://doi.org/10.4007/annals.2026.203.2.3) · [Al-Bassam, Sonnino and Buterin](https://arxiv.org/abs/1809.09044)

We implemented two small systematic codes at the same rate, 9/16. Reed–Solomon evaluates nine coefficients at sixteen distinct GF(256) points and normalizes its first nine rows to the identity. The local candidate is the edge-parity code of the finite bipartite graph K₄,₄. Four edges meet each vertex. Equivalently the construction uses three-by-three data with row and column parity, giving distance four, eight local checks of four shards, and three input shards for a single repair. A rectangle forms a minimum codeword. RS has distance eight. This finite candidate provides a local-check comparator, distinct from a high-dimensional code instantiating the preprint.

The replay enumerated all 65,536 erasure masks for local peeling and verified every one of the 11,440 nine-row Reed–Solomon decoding matrices over the field. Peeling succeeds precisely on forests of erased edges: a leaf gives a repair and a residual cycle admits multiple consistent completions. At four erasures, 1,784/1,820 masks recovered locally and 1,820/1,820 recovered with RS. At seven erasures, the corresponding counts were 4,096/11,440 and 11,440/11,440. Successful timed repairs matched the entire encoded stripe and the original payload digest. With one altered symbol, six of eight local checks passed. Three uniformly chosen distinct checks miss it with probability 20/56. Another valid codeword passes all parity checks while changing the trusted payload digest. With four fixed unavailable shards, three uniform distinct availability samples miss all four with probability 220/560.

## Repair optimization

On one pinned Ryzen 9 7950X3D core, the Python/NumPy implementations used identical data and alternating evaluation order. With 64 KiB shards (576 KiB payload, 1 MiB encoded), median single-shard repair times after one warmup were 703 µs for the simple RS implementation, 255 µs for local peeling, and 206 µs after directing the two XORs straight into the destination. Peak traced bytes during repair were 1,839,185 / 1,311,897 / 1,049,024 respectively, including a copy of the complete stripe. At 1 and 8 KiB shards, direct local repair took 3 and 6 µs. These timings describe these implementations. Optimized native libraries, disk reads, network retrieval, proof sizes, and consensus voting were outside the measurement.

**Decision.** Retain RS as the recovery baseline. Local parity is an inexpensive repair experiment for isolated erasures. The same redundancy left 36 four-erasure patterns ambiguous, so the local code cannot substitute for RS's worst-case guarantee. Keep Stem's payload commitments and availability obligations as implemented.

```sh
# Python 3.15.2 and NumPy 2.5.5; use a core allowed by the local cpuset.
taskset -c 2 python3 research/availability/measure.py > /tmp/stem-availability.json
xz -dc research/availability/results.json.xz > /tmp/stem-availability-recorded.json
```

The archived output includes deterministic counts and digests, and raw per-run nanoseconds for three shard sizes. Timing and allocator figures vary on replay.
