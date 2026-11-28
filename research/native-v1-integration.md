# Native V1 candidate integration

The retired experimental outputs remain in their original reports. This branch starts from the deployed-path source at `2b534a6` and puts the reversible changes on the same tree for one-factor comparisons. Its default configuration retains the existing execution and routing choices. The new runtime options and feature flags select individual candidates.

| Candidate | Source | Comparison |
| --- | --- | --- |
| Cell execution and material sharing | `1a50505` | Fixed-window or rolling Follower, parallel Leader, and immutable transaction material [Execution notes](scheduling/README.md) |
| Rolling Leader | `native-rolling-leader-execution` | Bounded worker refill while preserving greedy selection [Comparison notes](scheduling/README.md#rolling-leader-preparation) |
| Exact certified prefix | `jolteon-exact-prefix-pipeline` | Reuse pinned successor jobs across consecutive certified writes [Comparison notes](execution-pipeline/README.md#reusing-work-across-an-exact-certified-prefix) |
| Cancel obsolete preparation | `jolteon-cancel-stale-execution` | Stop work on expired Leader proposals or irrecoverable Follower branches [Comparison notes](execution-pipeline/README.md#cancelling-obsolete-preparation) |
| Shared Cell dependency reads | `native-shared-cell-deps` | Reuse bounded parent lookups across ordinary Leader and Follower windows [Resolver notes](follower-preparation/README.md#shared-dependency-reads) |
| Prior durable Live Cell view | `native-live-cell-parent-index` | Keep overlapping workers on resident reads through one tip advance [Index comparison notes](storage/live-cell-index.md#keeping-the-prior-durable-header-resident) |
| Resident Lock and TypeSet sharing | `a19ba05` | Inline and interned resident Live Cells at the same memory limit [Layout notes](storage/live-cell-index.md#resident-lock-and-typeset-sharing) |
| Lineage and Keeper | `45028dc` | Independent Head lookahead and one-shot wake preparation [Keeper notes](../BENCHMARKS.md#parallel-preparation-of-independent-due-commands) |
| Certificates and votes | `34e4743` | Shared direct frames and exact current-certificate reuse [Communication notes](certificates/README.md#sharing-certificates-and-direct-vote-frames) |
| Resident Patricia publication | `0eba67f` | Publish on the certified writer after WAL sync or on the Replica when it installs the durable tip [Resident Patricia notes](storage/resident-patricia.md) |
| Verified resident nodes | `native-verified-cell-nodes` | Authenticate decoded nodes at cache insertion and reuse them by content ID [Comparison notes](storage/resident-patricia.md#verified-decoded-resident-nodes) |
| Generated Cell node journals | `native-verified-cell-journal` | Reuse generated nodes through speculative reads, WAL encoding and resident publication [Comparison notes](storage/resident-patricia.md#generated-cell-node-journals) |

This integration adds `jolteon-resident-node-publication` as an independent control. Only the selected writer path updates the node index before its acknowledgement. The normal installer updates the Live Cell index and publishes nodes when the writer did not. The node-count and shard preallocation changes follow the same build option.

The integrated regression prepares one block containing dependent height wakes, an independent timed wake and a ten-payment spend chain. It compares selected bytes, complete Headers, receipts, versions, Cell proofs and reservation paths against a separate serial Store. The candidate uses both resident indexes, with the interned Live Cell layout. The all-feature regression targets, default Release build and combined Release build are compile-checked. Runtime correctness and performance remain to be qualified.

## Matched builds

Build each arm from the same commit and lockfile. Use a disposable target directory so the binary fingerprints and the evidence can be retained after build and data cleanup.

```sh
scratch=$(mktemp -d)
export CARGO_TARGET_DIR="$scratch/target"
export CXXFLAGS='-include cstdint'
cells=native-rolling-cell-execution,native-leader-dependency-execution,native-execution-material-cache
dependencies=native-shared-cell-deps
rolling_leader=native-rolling-leader-execution
exact_prefix=jolteon-exact-prefix-pipeline
cancellation=jolteon-cancel-stale-execution
contracts=native-lineage-dependency-lookahead,native-keeper-parallel-preparation
publication=jolteon-resident-node-publication
parent_cells=native-live-cell-parent-index
verified_nodes=native-verified-cell-nodes
verified_journal=native-verified-cell-journal
certificates=jolteon-shared-certificate-path

build() {
    local label=$1
    shift
    cargo build --locked --release --bin stem "$@" || return
    cp "$CARGO_TARGET_DIR/release/stem" "$scratch/$label" || return
    sha256sum "$scratch/$label"
}

build baseline
build cell-execution --features "$cells"
build shared-dependencies --features "$cells,$dependencies"
build rolling-leader --features "$cells,$rolling_leader"
build exact-prefix --features "$exact_prefix"
build cancellation --features "$cancellation"
build contract-execution --features "$contracts"
build publication --features "$publication"
build prior-durable-cells --features "$parent_cells"
build verified-nodes --features "$verified_nodes"
build verified-journal --features "$verified_journal"
build certificate-reuse --features "$certificates"
build combined --features "$cells,$dependencies,$rolling_leader,$exact_prefix,$cancellation,$contracts,$publication,$parent_cells,$verified_journal,$certificates"
```

Use the same launcher, funded-state copies, signed workload, CPUs, VM workers, retention, block limits and storage configuration in every arm of a comparison. Test the build features with existing runtime settings held fixed. Compare inline and interned Live Cells by adding only `--live-cell-index-intern-scripts` to the same binary with `--live-cell-index-mib` set. Keep the node index budget fixed while comparing publication. For a group that improves results, vary one component at a time before selecting a default. Record the effective flags, runtime budget, executable fingerprint, genesis digest and signed-corpus digest for every run.

The native test launcher accepts `STEM_TEST_REPLICA_BIN` and the storage settings `STEM_TEST_ROCKSDB_BLOCK_CACHE_MIB`, `STEM_TEST_LIVE_CELL_INDEX_MIB`, `STEM_TEST_CELL_NODE_INDEX_MIB` and `STEM_TEST_LIVE_CELL_INDEX_INTERN_SCRIPTS=1`. The same launcher passes these options for payment, market and Keeper trials. A manually started funded-replica trial uses the corresponding CLI options directly.

The VM result cache supports same-binary comparisons using replica
configuration fields `verification_cache_entries`,
`verification_cache_shards` and `verification_cache_lock_sampling`.
The [cache comparison](evolve/README.md#vm-result-cache-concurrency)
specifies the local launcher settings and lock telemetry.