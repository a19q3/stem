# Offline candidate evaluation

Run `taskset -c 2 python3 research/evolve/replay.py` from the repository root. The Linux evaluator requires bubblewrap, a system Python at /usr/bin/python3, Git and Rust 1.95.0 through rustup. Select an allowed CPU on another host. The manifest pins the source revision and oracle statements. Each build and measurement runs with an empty environment, a separate network namespace, read-only sources and toolchain, and a disposable writable work directory. Each process has 60 CPU seconds and 3 GiB of address space. The evaluator generates an unsigned JSON report. The archive includes raw samples, hashes, per-process CPU, peak RSS and filesystem block counts. No process merges, deploys or accesses a remote account.

## Workload provenance

The capture patch adds measurement output to the pinned Store and StemVM. In a four-replica native run, 96 signed payments finalized with one replica offline and the restarted replica agreed at height 18. The capture contains the actual verification identity, parent, observed cycles, parent sensitivity, budget, hit/miss, and state sort lengths per process. Identity hashes carry no signing material. Each node log is replayed independently from empty state. Node 0's restart is a separate trace. The capture records completed verification paths; it does not capture every VM rejection. Latency from an instrumented node is not used for comparison.

To collect fresh inputs, apply capture.patch in a disposable checkout of fefb8cc, build the release binary and run tools/jolteon_localnet.py with four validators, 96 payments, one offline and an offered rate of 32/s. The archived corpus stabilizes event order across subsequent evaluations. Its initial-order controls use node 1 for comparison and hold node 3 out; the replicas share the payment workload. Two additional deterministic synthetic traces use separate seeds, parent-sensitive successes and per-call cycle budgets.

## Pure ordering gate

The reference sorts pairs of a 32-byte key and an optional 32-byte value using Rust's standard unstable key sort. The experiment compares adjacent-exchange odd-even networks and insertion sort for lengths 2–8. Larger cases call the reference. A missing-last-comparator mutant fails on [1, 0]. The correctness gate compares complete records over 46,745 cases including all distinct permutations through eight elements, Boolean duplicates and long common prefixes. Each timed workload also checks all 2,048 records per length against the oracle. Each candidate runs in forward and reversed evaluation order with a warmup followed by nine timed batches.

Of 152 observed native sort calls, 66 were empty, 44 had one element, four had two, four had six, and 34 had more than eight. On the held-out seed, pooled median ns per sort for reference / odd-even / insertion were 19.66 / 28.47 / 28.67 at four elements and 60.91 / 94.84 / 140.48 at eight. These include the actual optional payload movement. Branch hardware counters and an instrumented chain-wide sorting percentage were not collected. **Decision:** retain the standard sort. Neither candidate earned a native runtime change.

## Success-cache gate

LRU, four-way sharded LRU, SIEVE, a small/main/ghost FIFO policy and a four-worker local L1 with shared L2 use the same total resident entry limit. The L1 model assigns workers round-robin, as the captured trace has no worker IDs. Ghost metadata, locks, allocator overhead and duplicate L1 entries mean equal entry counts are not equal byte budgets. Every result checks required parent identity and the current cycle limit. A synthetic two-event mutant that omits the parent comparison produces a conflicting result and is rejected.

On held-out node 3, all five policies hit 66 of 162 requests at 64 slots. At 16 slots, reference LRU hit 57, SIEVE 51 and the FIFO candidate 57. On the independent synthetic trace at 64 slots, LRU hit 5,410 of 8,192 and the FIFO candidate hit 5,489. Nominal cycles reused are reported separately from CPU. **Decision:** retain the native LRU. The synthetic increase did not transfer to the held-out payment trace; a live contention study with byte budgets would be needed to change policy.

## Review and rollback

This workspace evaluates research implementations only. Review deterministic decisions and corpus hashes first, then paired distributions, memory and workload representativeness. A runtime candidate needs independent review of mixed-version, proof, restart and handoff evidence before adoption. The reference remains the normal code path. Re-run the reference to investigate a regression; the offline scripts and their output can be removed without changing database or wire formats.

## VM result-cache concurrency

The held-out eviction experiments above retain the original LRU policy.
The runtime now supports a same-binary comparison of a local LRU and
independent LRU shards for successful transaction verifications. Set
`verification_cache_shards` in the replica configuration to a power of
two between 1 and 64. The default is one shard. The full witnessed,
resolved transaction key selects a shard. The original parent binding
and charged cycles apply on every hit. A miss runs the usual verifier.

The shard limits sum to the configured `verification_cache_entries`.
Remainder entries go to the lowest-index shards. Each shard evicts its
own least-recently-used entry. A zero-capacity shard falls through to
ordinary verification. Script Group cost estimates retain the separate
LRU and its original default capacity. The result-cache hit path borrows
the resolved key and parent ID and copies them only on a miss.

For contention attribution, set `verification_cache_lock_sampling` to
true. The node status reports cumulative sample count, wait and hold
nanoseconds, and sampled maxima separately for result and estimate
locks. It samples approximately one access in 256 per thread and lock
family. It mixes sample positions to cover both lookup and publication.
Status reads and resizing are outside the sampled operations. The
sampled wait includes scheduling and clock costs. Counter snapshots can
differ slightly across shards and in-flight observations.

Use the identical binary, signed corpus, genesis, scheduler and
storage settings. Compare 1, 2, 4, 8, 16, 32 and 64 shards with the same
result entry limit. Compare multiple worker counts and include cold
and warmed payment, heterogeneous Type Group and parent-reading
contracts. Collect per-window lock counter deltas alongside cache hit
rate, verification CPU, confirmed throughput and peak RSS. Repeat the
throughput controls with sampling disabled to estimate instrumentation
cost. A fixed entry budget can produce different allocation sizes and
earlier eviction in a hot shard. The separate estimate lock can still
dominate on workloads with many uncached groups.

The local load launcher accepts `STEM_TEST_VERIFICATION_CACHE_ENTRIES`,
`STEM_TEST_VERIFICATION_CACHE_SHARDS` and
`STEM_TEST_VERIFICATION_CACHE_LOCK_SAMPLING=1`. The recovery, finality
and mixed workload acceptance conditions remain the existing ones.
This candidate has a successful Release build and compiled regressions
for total capacity, zero-quota eviction, lock sampling and parent-bound
verification across shard layouts. Its runtime and throughput effects
remain to be measured.
