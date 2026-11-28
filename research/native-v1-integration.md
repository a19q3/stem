# Native V1 candidate integration

The retired experimental outputs remain in their original reports. This branch starts from the deployed-path source at `2b534a6` and puts the reversible changes on the same tree for one-factor comparisons. Its default configuration retains the existing execution and routing choices. The new runtime options and feature flags select individual candidates.

| Candidate | Source | Comparison |
| --- | --- | --- |
| Cell execution and material sharing | `1a50505` | Fixed-window or rolling Follower, parallel Leader, and immutable transaction material [Execution notes](scheduling/README.md) |
| Rolling Leader | `native-rolling-leader-execution` | Bounded worker refill while preserving greedy selection [Comparison notes](scheduling/README.md#rolling-leader-preparation) |
| Reuse parent-spend rejections | `native-reuse-parent-spend-rejections` | Avoid re-resolving an exact parent spend after a failed Leader trial [Comparison notes](scheduling/README.md#reusing-a-parallel-rejection-for-an-exact-parent-spend) |
| Reuse after ordered Cell settlement | `native-reuse-ordered-cell-writes` | Keep matching speculative consumers after an ordinary producer falls back [Comparison notes](scheduling/README.md#reusing-ordered-cell-writes) |
| Incremental Cell projection | `native-incremental-cell-projection` | Publish declaration prefixes as rolling tasks are submitted [Comparison notes](scheduling/README.md#incremental-projection-in-the-rolling-schedulers) |
| Mask rejected Leader declarations | `native-mask-rejected-cell-projections` | Reveal the previous Cell version as soon as the ordered scan rejects a candidate [Comparison notes](scheduling/README.md#masking-rejected-leader-cell-declarations) |
| Reclaim settled Cell declarations | `native-reclaim-cell-projection` | Keep speculative history near the active rolling window [Comparison notes](scheduling/README.md#retiring-settled-projection-edits) |
| Rolling Script Groups | `native-rolling-script-groups` | Refill group workers and charge their cycles in canonical order [Comparison notes](scheduling/README.md#rolling-script-group-verification) |
| Standard trace cycle reuse | `native-trace-cycle-reuse` | Retain the built-in cycle total of byte-matched native VM traces [Comparison notes](critical-path/decoder-cache.md#reuse-the-built-in-cycle-total) |
| Exact certified prefix | `jolteon-exact-prefix-pipeline` | Reuse pinned successor jobs across consecutive certified writes [Comparison notes](execution-pipeline/README.md#reusing-work-across-an-exact-certified-prefix) |
| Cancel obsolete preparation | `jolteon-cancel-stale-execution` | Stop work on expired Leader proposals or irrecoverable Follower branches [Comparison notes](execution-pipeline/README.md#cancelling-obsolete-preparation) |
| Pre-quorum Leader construction | `jolteon-pre-quorum-proposal` | Prepare after the local parent vote while the next view waits for its exact QC [Comparison notes](execution-pipeline/cross-block.md#preparing-the-next-proposal-while-its-parent-is-being-certified) |
| Shared proposal snapshot | `jolteon-shared-proposal-snapshot` | Move ordered mempool traversal from the Replica to the Leader worker [Comparison notes](execution-pipeline/cross-block.md#sharing-the-ordered-mempool-snapshot-with-the-leader-worker) |
| Inline Cell resolution bookkeeping | `native-inline-cell-resolution` | Keep small OutPoint sets and a sole Script Group outside heap-backed indexes [Comparison notes](follower-preparation/README.md#inline-cell-resolution-bookkeeping) |
| Shared Cell dependency reads | `native-shared-cell-deps` | Reuse bounded parent lookups across ordinary Leader and Follower windows [Resolver notes](follower-preparation/README.md#shared-dependency-reads) |
| Shared pinned Cell cache | `native-shared-pinned-cell-cache` | Share bounded decoded Cell records across workers on the same committed Header [Comparison notes](storage/live-cell-index.md#sharing-the-pinned-live-cell-cache) |
| Prior durable Live Cell view | `native-live-cell-parent-index` | Keep overlapping workers on resident reads through one tip advance [Index comparison notes](storage/live-cell-index.md#keeping-the-prior-durable-header-resident) |
| Fixed-width resident OutPoints | `native-inline-outpoint-index` | Keep full packed keys in table buckets [Index comparison notes](storage/live-cell-index.md#fixed-width-resident-outpoint-keys) |
| Resident Lock and TypeSet sharing | `a19ba05` | Inline and interned resident Live Cells at the same memory limit [Layout notes](storage/live-cell-index.md#resident-lock-and-typeset-sharing) |
| Lineage and Keeper | `45028dc` | Independent Head lookahead and one-shot wake preparation [Keeper notes](../BENCHMARKS.md#parallel-preparation-of-independent-due-commands) |
| Certificates and votes | `34e4743` | Shared direct frames and exact current-certificate reuse [Communication notes](certificates/README.md#sharing-certificates-and-direct-vote-frames) |
| Resident Patricia publication | `0eba67f` | Publish on the certified writer after WAL sync or on the Replica when it installs the durable tip [Resident Patricia notes](storage/resident-patricia.md) |
| Prebuilt finality state writes | `jolteon-prebuilt-state-write` | Assemble one certified state prefix before the atomic finality write [Comparison notes](execution-pipeline/cross-block.md#prebuilding-the-certified-state-writebatch) |
| Verified resident nodes | `native-verified-cell-nodes` | Authenticate decoded nodes at cache insertion and reuse them by content ID [Comparison notes](storage/resident-patricia.md#verified-decoded-resident-nodes) |
| Compact write journal | `native-compact-write-journal` | Inline short row keys and index positions without copying keys [Comparison notes](storage/patricia-finishing/README.md#compact-prepared-write-journal) |
| Generated Cell node journals | `native-verified-cell-journal` | Reuse generated nodes through speculative reads, WAL encoding and resident publication [Comparison notes](storage/resident-patricia.md#generated-cell-node-journals) |
| Compact generated node journals | `native-compact-patricia-journal` | Store tombstones and lookup positions separately from decoded nodes [Comparison notes](storage/resident-patricia.md#compact-generated-node-journal) |
| Stream generated node publication | `native-stream-node-publication` | Apply the verified journal directly to contiguous resident shards [Comparison notes](storage/resident-patricia.md#streaming-verified-edits-into-the-resident-shards) |

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
inline_resolution=native-inline-cell-resolution
rolling_leader=native-rolling-leader-execution
parent_spend_reuse=native-reuse-parent-spend-rejections
ordered_cells=native-reuse-ordered-cell-writes
incremental_projection=native-incremental-cell-projection
mask_rejected=native-mask-rejected-cell-projections
reclaim_projection=native-reclaim-cell-projection
rolling_groups=native-rolling-script-groups
trace_cycles=native-trace-cycle-reuse
exact_prefix=jolteon-exact-prefix-pipeline
cancellation=jolteon-cancel-stale-execution
pre_quorum=jolteon-pre-quorum-proposal
shared_proposal_snapshot=jolteon-shared-proposal-snapshot
contracts=native-lineage-dependency-lookahead,native-keeper-parallel-preparation
publication=jolteon-resident-node-publication
prebuilt_state_write=jolteon-prebuilt-state-write
parent_cells=native-live-cell-parent-index
pinned_cache=native-shared-pinned-cell-cache
inline_outpoints=native-inline-outpoint-index
verified_nodes=native-verified-cell-nodes
verified_journal=native-verified-cell-journal
compact_patricia_journal=native-compact-patricia-journal
stream_node_publication=native-stream-node-publication
compact_journal=native-compact-write-journal
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
build inline-resolution --features "$cells,$inline_resolution"
build rolling-leader --features "$cells,$rolling_leader"
build parent-spend-reuse --features "$cells,$rolling_leader,$parent_spend_reuse"
build ordered-cell-writes --features "$cells,$rolling_leader,$ordered_cells"
build incremental-projection --features "$cells,$rolling_leader,$incremental_projection"
build mask-rejected --features "$cells,$rolling_leader,$incremental_projection,$mask_rejected"
build projection-reclaim-control --features "$cells,$rolling_leader,$incremental_projection,$ordered_cells"
build projection-reclaim --features "$cells,$rolling_leader,$reclaim_projection"
build rolling-script-groups --features "$cells,$rolling_leader,$rolling_groups"
build trace-cycle-reuse --features "$trace_cycles"
build exact-prefix --features "$exact_prefix"
build cancellation --features "$cancellation"
build pre-quorum-control --features "$exact_prefix,$cancellation"
build pre-quorum --features "$exact_prefix,$cancellation,$pre_quorum"
build shared-proposal-snapshot --features "$pre_quorum,$shared_proposal_snapshot"
build contract-execution --features "$contracts"
build publication --features "$publication"
build prebuilt-state-write --features "$prebuilt_state_write"
build prior-durable-cells --features "$parent_cells"
build shared-pinned-cells --features "$pinned_cache"
build inline-outpoints --features "$inline_outpoints"
build verified-nodes --features "$verified_nodes"
build verified-journal --features "$verified_journal"
build compact-patricia-journal --features "$verified_journal,$compact_patricia_journal"
build node-publication-control --features "$verified_journal,$publication"
build node-publication-stream --features "$verified_journal,$publication,$stream_node_publication"
build compact-journal --features "$compact_journal"
build certificate-reuse --features "$certificates"
build combined --features "$cells,$dependencies,$inline_resolution,$rolling_leader,$parent_spend_reuse,$ordered_cells,$incremental_projection,$mask_rejected,$reclaim_projection,$rolling_groups,$trace_cycles,$exact_prefix,$cancellation,$pre_quorum,$shared_proposal_snapshot,$contracts,$publication,$prebuilt_state_write,$parent_cells,$pinned_cache,$inline_outpoints,$verified_journal,$compact_patricia_journal,$stream_node_publication,$compact_journal,$certificates"
```

Use the same launcher, funded-state copies, signed workload, CPUs, VM workers, retention, block limits and storage configuration in every arm of a comparison. Test the build features with existing runtime settings held fixed. Compare inline and interned Live Cells by adding only `--live-cell-index-intern-scripts` to the same binary with `--live-cell-index-mib` set. Keep the node index budget fixed while comparing publication. For a group that improves results, vary one component at a time before selecting a default. Record the effective flags, runtime budget, executable fingerprint, genesis digest and signed-corpus digest for every run.

The native test launcher accepts `STEM_TEST_REPLICA_BIN` and the storage settings `STEM_TEST_ROCKSDB_BLOCK_CACHE_MIB`, `STEM_TEST_LIVE_CELL_INDEX_MIB`, `STEM_TEST_CELL_NODE_INDEX_MIB` and `STEM_TEST_LIVE_CELL_INDEX_INTERN_SCRIPTS=1`. The same launcher passes these options for payment, market and Keeper trials. A manually started funded-replica trial uses the corresponding CLI options directly.

The VM result cache supports same-binary comparisons using replica
configuration fields `verification_cache_entries`,
`verification_cache_shards` and `verification_cache_lock_sampling`.
The [cache comparison](evolve/README.md#vm-result-cache-concurrency)
specifies the local launcher settings and lock telemetry.
