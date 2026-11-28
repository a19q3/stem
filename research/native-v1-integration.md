# Native V1 candidate integration

The retired experimental outputs remain in their original reports. This branch starts from the deployed-path source at `2b534a6` and puts the reversible changes on the same tree for one-factor comparisons. Its default configuration retains the existing execution and routing choices. The new runtime options and feature flags select individual candidates.

| Candidate | Source | Comparison |
| --- | --- | --- |
| Cell execution and material sharing | `1a50505` | Fixed-window or rolling Follower, parallel Leader, and immutable transaction material [Execution notes](scheduling/README.md) |
| Rolling Leader | `native-rolling-leader-execution` | Bounded worker refill while preserving greedy selection [Comparison notes](scheduling/README.md#rolling-leader-preparation) |
| Leader tail cycle shares | `native-leader-tail-cycle-shares` | Keep low-cost candidates parallel when remaining cycles fund fewer than two full reservations [Comparison notes](scheduling/README.md#sharing-the-leaders-remaining-cycles) |
| Inline worker Cell edits | `native-compact-cell-trial-edits` | Return small validated effects without an intermediate tree [Comparison notes](scheduling/README.md#inline-cell-changes-from-worker-trials) |
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
| Shared proposal bytes | `jolteon-share-proposal-bytes` | Reuse canonical proposal bodies and periodic direct transport frames [Comparison notes](dissemination/README.md#reusing-the-direct-proposal-body-and-frame) |
| Inline Cell resolution bookkeeping | `native-inline-cell-resolution` | Keep small OutPoint sets and a sole Script Group outside heap-backed indexes [Comparison notes](follower-preparation/README.md#inline-cell-resolution-bookkeeping) |
| Owned Live Cell reads | `native-owned-live-cell-reads` | Reuse the committed RocksDB allocation through validation [Resolver notes](follower-preparation/README.md#reusing-the-owned-rocksdb-live-cell-buffer) |
| Lazy Cell digests | `native-lazy-cell-digests` | Compute only requested digests and share them across detached inline Cells [Resolver notes](follower-preparation/README.md#computing-cell-digests-independently) |
| Shared Cell dependency reads | `native-shared-cell-deps` | Reuse bounded parent lookups across ordinary Leader and Follower windows [Resolver notes](follower-preparation/README.md#shared-dependency-reads) |
| Rolling dependency hints | `native-rolling-dependency-hints` | Refresh parent lookup plans when the dependency mix changes [Resolver notes](follower-preparation/README.md#refreshing-dependency-hints-during-rolling-preparation) |
| Shared pinned Cell cache | `native-shared-pinned-cell-cache` | Share bounded decoded Cell records across workers on the same committed Header [Comparison notes](storage/live-cell-index.md#sharing-the-pinned-live-cell-cache) |
| Sharded decoded Live Cell cache | `native-sharded-live-cache` | Separate locks and capacity flushes across the same total allowance [Comparison notes](storage/live-cell-index.md#sharded-decoded-live-cell-caches) |
| Prior durable Live Cell view | `native-live-cell-parent-index` | Keep overlapping workers on resident reads through one tip advance [Index comparison notes](storage/live-cell-index.md#keeping-the-prior-durable-header-resident) |
| Moved Live Cell before-images | `native-live-cell-move-previous` | Remove and retain each old row in one resident-table lookup [Comparison notes](storage/live-cell-index.md#moving-removed-rows-into-the-prior-header-journal) |
| Pinned Live Cell contention fallback | `native-pinned-index-try-read` | Continue through an authenticated disk snapshot while index publication holds the write lock [Comparison notes](storage/live-cell-index.md#letting-pinned-readers-proceed-during-publication) |
| Prepared Inline Live Cell records | `native-prepared-live-cell-ownership` | Share exact record bytes from candidate preparation through resident publication [Comparison notes](storage/live-cell-index.md#preparing-inline-live-cell-records-for-publication) |
| Fixed-width resident OutPoints | `native-inline-outpoint-index` | Keep full packed keys in table buckets [Index comparison notes](storage/live-cell-index.md#fixed-width-resident-outpoint-keys) |
| Parallel resident recovery | `--live-cell-recovery-workers` | Validate and hash bounded row batches before the ordered authenticated fold [Comparison notes](storage/live-cell-index.md#bounded-parallel-index-recovery) |
| Resident Lock and TypeSet sharing | `a19ba05` | Inline and interned resident Live Cells at the same memory limit [Layout notes](storage/live-cell-index.md#resident-lock-and-typeset-sharing) |
| Leader Lineage materialization | `native-leader-lineage-execution` | Reuse ready Lineage work through greedy Leader selection [Comparison notes](execution-pipeline/README.md#leader-lineage-materialization) |
| Rolling Lineage materialization | `native-rolling-lineage-execution` | Refill ready materialization jobs during ordered settlement on the accepted prefix [Comparison notes](execution-pipeline/README.md#rolling-lineage-materialization) |
| Lineage Cell read reuse | `native-reuse-lineage-cell-reads` | Reuse exact fee, Executor and ordered-stage Cell records [Comparison notes](execution-pipeline/README.md#lineage-cell-read-reuse) |
| Lineage and Keeper | `45028dc` | Independent Head lookahead and one-shot wake preparation [Keeper notes](../BENCHMARKS.md#parallel-preparation-of-independent-due-commands) |
| Leader Keeper prefix reuse | `native-reuse-keeper-proposal-prefix` | Reuse settled one-shot wakes when finishing the selected proposal [Keeper notes](../BENCHMARKS.md#reusing-the-keeper-prefix-in-leader-proposals) |
| Certificates and votes | `34e4743` | Shared direct frames and exact current-certificate reuse [Communication notes](certificates/README.md#sharing-certificates-and-direct-vote-frames) |
| Decoded QC retention | `jolteon-retain-verified-quorums` | Share bounded exact recent certificates in the direct path [Comparison notes](certificates/README.md#retaining-decoded-quorum-certificates) |
| Resident Patricia publication | `0eba67f` | Publish on the certified writer after WAL sync or on the Replica when it installs the durable tip [Resident Patricia notes](storage/resident-patricia.md) |
| Resident Live Cell publication | `jolteon-resident-cell-publication` | Publish the Live Cell generation after WAL sync on the certified writer [Comparison notes](storage/live-cell-index.md#publishing-live-cells-on-the-certified-writer) |
| Prebuilt finality state writes | `jolteon-prebuilt-state-write` | Assemble one certified state prefix before the atomic finality write [Comparison notes](execution-pipeline/cross-block.md#prebuilding-the-certified-state-writebatch) |
| Verified resident nodes | `native-verified-cell-nodes` | Authenticate decoded nodes at cache insertion and reuse them by content ID [Comparison notes](storage/resident-patricia.md#verified-decoded-resident-nodes) |
| Hot Cell node fallback | `native-hot-cell-node-cache` | Cache authenticated reads when the complete Patricia index exceeds its budget [Comparison notes](storage/resident-patricia.md#demand-filled-fallback-after-a-residency-limit) |
| Batched Patricia proof reads | `native-batched-patricia-proofs` | Group proof-frontier RocksDB misses while preserving canonical proof bytes [Comparison notes](patricia/README.md#batched-reads-for-shared-prefix-proofs) |
| Batched Live Cell proof records | `native-batched-cell-proof-records` | Fetch unique records in bounded groups and verify them against their paths [Comparison notes](patricia/README.md#grouping-the-live-cell-records-behind-proofs) |
| Prepared Cell record keys | `native-prepared-cell-record-keys` | Reuse created Cell keys from the commitment on finality [Comparison notes](storage/patricia-finishing/README.md#reducing-certified-batch-construction-work) |
| Compact prepared Cell changes | `native-compact-prepared-cell-delta` | Share an exact position index and ordered Cell changes across speculative readers [Comparison notes](storage/live-cell-index.md#compact-prepared-cell-changes) |
| Parallel Cell record hashing | `native-parallel-cell-record-hashes` | Hash the ordered Live Cell changes on existing execution workers [Comparison notes](storage/patricia-finishing/README.md#parallel-cell-record-hashing) |
| Vectored Cell writes | `native-vectored-cell-writes` | Append large canonical records without a contiguous scratch copy [Comparison notes](storage/patricia-finishing/README.md#vectored-writes-for-large-cell-records) |
| Reuse certified writer buffer | `jolteon-reuse-write-buffer` | Clear and recycle a bounded write buffer on the existing writer [Comparison notes](storage/patricia-finishing/README.md#reducing-certified-batch-construction-work) |
| Compact write journal | `native-compact-write-journal` | Inline short row keys and index positions without copying keys [Comparison notes](storage/patricia-finishing/README.md#compact-prepared-write-journal) |
| Generated Cell node journals | `native-verified-cell-journal` | Reuse generated nodes through speculative reads, WAL encoding and resident publication [Comparison notes](storage/resident-patricia.md#generated-cell-node-journals) |
| Frontier Cell edits | `native-patricia-frontier-edit` | Feed authenticated upper-branch batches directly into parallel mutation [Comparison notes](patricia/README.md#editing-directly-from-batched-node-reads) |
| Compact generated node journals | `native-compact-patricia-journal` | Store tombstones and lookup positions separately from decoded nodes [Comparison notes](storage/resident-patricia.md#compact-generated-node-journal) |
| Segmented generated node journals | `native-segmented-patricia-journal` | Transfer generated node buffers at parallel subtree joins [Comparison notes](storage/resident-patricia.md#segmented-generated-node-journals) |
| Stream generated node publication | `native-stream-node-publication` | Apply the verified journal directly to contiguous resident shards [Comparison notes](storage/resident-patricia.md#streaming-verified-edits-into-the-resident-shards) |

This integration adds `jolteon-resident-node-publication` as an independent control. Only the selected writer path updates the node index before its acknowledgement. The installer publishes either index when that index was left to the Replica. The Live Cell writer has its own switch. The node-count and shard preallocation changes follow the same build option.

The integrated regression prepares one block containing dependent height wakes, an independent timed wake and a ten-payment spend chain. It compares selected bytes, complete Headers, receipts, versions, Cell proofs and reservation paths against a separate serial Store. The candidate uses both resident indexes, with the interned Live Cell layout. The all-feature regression targets, default Release build and combined Release build are compile-checked. Runtime correctness and performance remain to be qualified.

## Matched builds

Build each arm from the same commit and lockfile. Use a disposable target directory so the binary fingerprints and the evidence can be retained after build and data cleanup.

```sh
scratch=$(mktemp -d)
export CARGO_TARGET_DIR="$scratch/target"
export CXXFLAGS='-include cstdint'
cells=native-rolling-cell-execution,native-leader-dependency-execution,native-execution-material-cache
dependencies=native-shared-cell-deps
rolling_dependency_hints=native-rolling-dependency-hints
inline_resolution=native-inline-cell-resolution
owned_cell_reads=native-owned-live-cell-reads
lazy_cell_digests=native-lazy-cell-digests
rolling_leader=native-rolling-leader-execution
tail_cycle_shares=native-leader-tail-cycle-shares
compact_trial_edits=native-compact-cell-trial-edits
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
shared_proposal_bytes=jolteon-share-proposal-bytes
contracts=native-lineage-dependency-lookahead,native-keeper-parallel-preparation
rolling_lineage=native-rolling-lineage-execution
lineage_reads=native-reuse-lineage-cell-reads
leader_lineage=native-leader-lineage-execution
keeper_prefix=native-reuse-keeper-proposal-prefix
publication=jolteon-resident-node-publication
cell_publication=jolteon-resident-cell-publication
prebuilt_state_write=jolteon-prebuilt-state-write
prepared_cell_keys=native-prepared-cell-record-keys
compact_prepared_cells=native-compact-prepared-cell-delta
parallel_cell_hashes=native-parallel-cell-record-hashes
vectored_cell_writes=native-vectored-cell-writes
reuse_write_buffer=jolteon-reuse-write-buffer
parent_cells=native-live-cell-parent-index
moved_parent_cells=native-live-cell-move-previous
pinned_index_try_read=native-pinned-index-try-read
inline_cell_ownership=native-prepared-live-cell-ownership
pinned_cache=native-shared-pinned-cell-cache
sharded_cache=native-sharded-live-cache
inline_outpoints=native-inline-outpoint-index
verified_nodes=native-verified-cell-nodes
hot_cell_nodes=native-hot-cell-node-cache
batched_proofs=native-batched-patricia-proofs
batched_proof_records=native-batched-cell-proof-records
verified_journal=native-verified-cell-journal
frontier_edit=native-patricia-frontier-edit
compact_patricia_journal=native-compact-patricia-journal
segmented_patricia_journal=native-segmented-patricia-journal
stream_node_publication=native-stream-node-publication
compact_journal=native-compact-write-journal
certificates=jolteon-shared-certificate-path
retained_quorums=jolteon-retain-verified-quorums

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
build rolling-dependency-hints-control --features "$cells,$rolling_leader,$dependencies"
build rolling-dependency-hints --features "$cells,$rolling_leader,$dependencies,$rolling_dependency_hints"
build inline-resolution --features "$cells,$inline_resolution"
build owned-cell-reads --features "$owned_cell_reads"
build lazy-cell-digests --features "$lazy_cell_digests"
build rolling-leader --features "$cells,$rolling_leader"
build tail-cycle-shares --features "$cells,$rolling_leader,$tail_cycle_shares"
build compact-trial-edits --features "$cells,$rolling_leader,$compact_trial_edits"
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
build shared-proposal-bytes --features "$shared_proposal_bytes"
build contract-execution --features "$contracts"
build lineage-cell-reads --features "$contracts,$lineage_reads"
build rolling-lineage --features "$contracts,$rolling_lineage"
build leader-lineage --features "$contracts,$rolling_lineage,$leader_lineage"
build keeper-prefix --features "$contracts,$keeper_prefix"
build publication --features "$publication"
build cell-publication --features "$cell_publication"
build pinned-index-contention --features "$cell_publication,$pinned_index_try_read"
build prepared-inline-cells --features "$cell_publication,$inline_cell_ownership"
build prebuilt-state-write --features "$prebuilt_state_write"
build prepared-cell-keys --features "$prepared_cell_keys"
build compact-prepared-cells --features "$prepared_cell_keys,$compact_prepared_cells"
build parallel-cell-hashes --features "$prepared_cell_keys,$parallel_cell_hashes"
build vectored-cell-writes --features "$vectored_cell_writes"
build reuse-write-buffer --features "$reuse_write_buffer"
build prior-durable-cells --features "$parent_cells"
build moved-before-images --features "$moved_parent_cells"
build shared-pinned-cells --features "$pinned_cache"
build sharded-pinned-cells --features "$pinned_cache,$sharded_cache"
build inline-outpoints --features "$inline_outpoints"
build verified-nodes --features "$verified_nodes"
build hot-cell-nodes --features "$verified_nodes,$hot_cell_nodes"
build batched-patricia-proofs --features "$batched_proofs"
build batched-cell-proof-records --features "$batched_proofs,$batched_proof_records"
build verified-journal --features "$verified_journal"
build frontier-cell-edits --features "$verified_journal,$frontier_edit"
build compact-patricia-journal --features "$verified_journal,$compact_patricia_journal"
build segmented-patricia-journal --features "$segmented_patricia_journal"
build node-publication-control --features "$verified_journal,$publication"
build node-publication-stream --features "$verified_journal,$publication,$stream_node_publication"
build compact-journal --features "$compact_journal"
build certificate-reuse --features "$certificates"
build retained-quorums --features "$retained_quorums"
build combined --features "$cells,$dependencies,$rolling_dependency_hints,$inline_resolution,$owned_cell_reads,$lazy_cell_digests,$rolling_leader,$tail_cycle_shares,$compact_trial_edits,$parent_spend_reuse,$ordered_cells,$incremental_projection,$mask_rejected,$reclaim_projection,$rolling_groups,$trace_cycles,$exact_prefix,$cancellation,$pre_quorum,$shared_proposal_snapshot,$shared_proposal_bytes,$contracts,$lineage_reads,$rolling_lineage,$leader_lineage,$keeper_prefix,$publication,$cell_publication,$prebuilt_state_write,$prepared_cell_keys,$compact_prepared_cells,$parallel_cell_hashes,$vectored_cell_writes,$reuse_write_buffer,$parent_cells,$moved_parent_cells,$pinned_index_try_read,$inline_cell_ownership,$pinned_cache,$sharded_cache,$inline_outpoints,$verified_journal,$frontier_edit,$hot_cell_nodes,$batched_proofs,$batched_proof_records,$segmented_patricia_journal,$stream_node_publication,$compact_journal,$retained_quorums"
```

Use the same launcher, funded-state copies, signed workload, CPUs, VM workers, retention, block limits and storage configuration in every arm of a comparison. Test the build features with existing runtime settings held fixed. Compare inline and interned Live Cells by adding only `--live-cell-index-intern-scripts` to the same binary with `--live-cell-index-mib` set. Keep the node index budget fixed while comparing publication. For a group that improves results, vary one component at a time before selecting a default. Record the effective flags, runtime budget, executable fingerprint, genesis digest and signed-corpus digest for every run.

The native test launcher accepts `STEM_TEST_REPLICA_BIN` and the storage settings `STEM_TEST_ROCKSDB_BLOCK_CACHE_MIB`, `STEM_TEST_LIVE_CELL_INDEX_MIB`, `STEM_TEST_LIVE_CELL_RECOVERY_WORKERS`, `STEM_TEST_CELL_NODE_INDEX_MIB` and `STEM_TEST_LIVE_CELL_INDEX_INTERN_SCRIPTS=1`. The same launcher passes these options for payment, market and Keeper trials. A manually started funded-replica trial uses the corresponding CLI options directly.

The VM result cache supports same-binary comparisons using replica
configuration fields `verification_cache_entries`,
`verification_cache_shards` and `verification_cache_lock_sampling`.
The [cache comparison](evolve/README.md#vm-result-cache-concurrency)
specifies the local launcher settings and lock telemetry.
