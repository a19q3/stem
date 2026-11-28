# Native V1 candidate integration

The retired experimental outputs remain in their original reports. This branch starts from the deployed-path source at `2b534a6` and puts the reversible changes on the same tree for one-factor comparisons. Its default configuration retains the existing execution and routing choices. The new runtime options and feature flags select individual candidates.

| Candidate | Source | Comparison |
| --- | --- | --- |
| Cell execution and material sharing | `1a50505` | Fixed-window or rolling Follower, parallel Leader, and immutable transaction material [Execution notes](scheduling/README.md) |
| Resident Lock and TypeSet sharing | `a19ba05` | Inline and interned resident Live Cells at the same memory limit [Layout notes](storage/live-cell-index.md#resident-lock-and-typeset-sharing) |
| Lineage and Keeper | `45028dc` | Independent Head lookahead and one-shot wake preparation [Keeper notes](../BENCHMARKS.md#parallel-preparation-of-independent-due-commands) |
| Certificates and votes | `34e4743` | Shared direct frames and exact current-certificate reuse [Communication notes](certificates/README.md#sharing-certificates-and-direct-vote-frames) |
| Resident Patricia publication | `0eba67f` | Publish on the certified writer after WAL sync or on the Replica when it installs the durable tip [Resident Patricia notes](storage/resident-patricia.md) |

This integration adds `jolteon-resident-node-publication` as an independent control. Only the selected writer path updates the node index before its acknowledgement. The normal installer updates the Live Cell index and publishes nodes when the writer did not. The node-count and shard preallocation changes follow the same build option.

The integrated regression prepares one block containing dependent height wakes, an independent timed wake and a ten-payment spend chain. It compares selected bytes, complete Headers, receipts, versions, Cell proofs and reservation paths against a separate serial Store. The candidate uses both resident indexes, with the interned Live Cell layout. The all-feature regression targets, default Release build and combined Release build are compile-checked. Runtime correctness and performance remain to be qualified.

## Matched builds

Build each arm from the same commit and lockfile. Use a disposable target directory so the binary fingerprints and the evidence can be retained after build and data cleanup.

```sh
scratch=$(mktemp -d)
export CARGO_TARGET_DIR="$scratch/target"
export CXXFLAGS='-include cstdint'
cells=native-rolling-cell-execution,native-leader-dependency-execution,native-execution-material-cache
contracts=native-lineage-dependency-lookahead,native-keeper-parallel-preparation
publication=jolteon-resident-node-publication
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
build contract-execution --features "$contracts"
build publication --features "$publication"
build certificate-reuse --features "$certificates"
build combined --features "$cells,$contracts,$publication,$certificates"
```

Use the same launcher, funded-state copies, signed workload, CPUs, VM workers, retention, block limits and storage configuration in every arm of a comparison. Test the build features with existing runtime settings held fixed. Compare inline and interned Live Cells by adding only `--live-cell-index-intern-scripts` to the same binary with `--live-cell-index-mib` set. Keep the node index budget fixed while comparing publication. For a group that improves results, vary one component at a time before selecting a default. Record the effective flags, runtime budget, executable fingerprint, genesis digest and signed-corpus digest for every run.

The native test launcher accepts `STEM_TEST_REPLICA_BIN` and the storage settings `STEM_TEST_ROCKSDB_BLOCK_CACHE_MIB`, `STEM_TEST_LIVE_CELL_INDEX_MIB`, `STEM_TEST_CELL_NODE_INDEX_MIB` and `STEM_TEST_LIVE_CELL_INDEX_INTERN_SCRIPTS=1`. The same launcher passes these options for payment, market and Keeper trials. A manually started funded-replica trial uses the corresponding CLI options directly.

| Workload | Existing control | Integrated evidence to collect |
| --- | --- | --- |
| Sustained payments | [Resident saturation](storage/resident-saturation.md) | Actual offer and admission, common confirmations, backlog slope, tail population, node CPU and peak RSS |
| Heterogeneous Polytype | [Mixed Type Groups](storage/resident-patricia.md#mixed-type-group-control) | Identical group tiers and charges, preparation costs, worker and cache counters |
| Hotspot AMM and multiple markets | [Lineage conflict fixtures](execution-pipeline/README.md#lineage-materialization-across-conflicts) · [AMM replay](lineage-engine/README.md) · [CLOB reserve oracle](clob/README.md) | Per-market Head versions, offered operations, fills, conserved inventory, fee-only rejections and independent oracle agreement |
| Keeper | [Agenda and wake fixtures](../BENCHMARKS.md#keeper-agenda-decoding-and-wake-replay) | Height and timed obligations, wake lateness, retry accounting, reserved work and ordinary-payment finality |
| Faults and recovery | [Certified commit](execution-pipeline/README.md#integrated-commit-faults-with-live-payments) · [Certificate restart](certificates/README.md#matched-reuse-with-two-stopped-leaders) | Archive replay, exact state and receipt proofs, verified finality and crash/reopen behavior |

Use single-host matched runs. Raise actual offer to find each configuration's saturation shoulder and compare queue growth as well as confirmation rates. Existing 3,000 TPS qualification remains reference evidence. The [JMT engine comparisons](storage/authenticated-state-comparison.md) and isolated development-chain prototype at `f6a00fd` retain their own proof and root boundary. The native V1 build matrix uses canonical Patricia commitments.
