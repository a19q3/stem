# Patricia writes while preparing signed payment blocks

A single sorted update of a Cell tree starts from an immutable parent root. It descends into the old children and constructs replacements on its way back up. The candidate appends the node bytes and tombstones to a vector during that traversal and collects them into the sorted write map once. Reusable edits still index every pending write so later reads and proofs can see earlier changes. Both paths use the same traversal, key derivation and node encoding. [Patricia implementation and differential test](../../../ledger/src/patricia.rs) · [Storage call site](../../../src/native_store.rs)

## Time on identical blocks

The fixture signs 4,096 independent payments. It prepares a 2,048-payment parent, then prepares the same child against a committed copy and against the uncommitted proposal. The committed side uses RocksDB. The speculative side can read nodes from the parent proposal. Each executable ran two warmups and twelve measured repetitions at each size, alternating parent order. The measured region is the full follower preparation call. Receipt checks, serial reference execution and final database reopens are outside the timer. The reference and candidate used two VM workers, the same contract bytes and the same three resulting Header IDs. [Fixture](blocks.rs) · [Uninstrumented samples](blocks.json)

| Payments | Parent | Indexed A1 (ms) | Buffered B1 (ms) | Buffered B2 (ms) | Indexed A2 (ms) |
| ---: | --- | ---: | ---: | ---: | ---: |
| 256 | Committed | 4.941 | 4.650 | 4.655 | 4.956 |
| 256 | Speculative | 5.135 | 4.897 | 4.896 | 5.150 |
| 1,024 | Committed | 16.489 | 15.802 | 15.850 | 16.537 |
| 1,024 | Speculative | 17.905 | 17.383 | 17.343 | 17.995 |
| 2,048 | Committed | 30.634 | 29.719 | 29.744 | 30.772 |
| 2,048 | Speculative | 34.121 | 33.494 | 33.357 | 34.299 |

The separate instrumented reference read 6,449 encoded nodes (430,317 bytes) and returned 11,700 node writes for the 2,048-payment child. On a committed parent, median tree edit time was 7.793 ms, including 2.466 ms reading and restoring the stored nodes. On the speculative parent, 4,113 of the reads came from the parent proposal. Median edit time was 6.727 ms, including 1.420 ms for the reads. These warm measurements include instrumentation. [Phase samples](phases.json) · [Measurement points](timing.patch)

```mermaid
flowchart LR
    P["Committed database or parent proposal"] --> T["Read old nodes along sorted Cell changes"]
    T --> V["Append replacement nodes and deletions"]
    V --> S["Build canonical sorted write map"]
    S --> R["Prepare execution Header, receipts and write batch"]
    R --> C["Certify and commit"]
```

In a second four-process sequence GNU `time` measured peak resident memory over the whole fixed-block fixture, including its two stores, signing and the final reopen. Indexed runs peaked at 80,180 and 78,644 KiB. Buffered runs peaked at 83,772 and 81,848 KiB. Both paired increases were 3.1–3.5 MiB. This measurement includes allocator and RocksDB memory. Reproduce with `taskset -c 0,1 env STEM_TEST_CONTRACT_DIR="$contracts" /usr/bin/time -f %M "$binary" --exact patricia_finishing::measure_fixed_children` in the order in the table. [Peak RSS and binary digests](memory.json)

## Four validators at 3,000 scheduled writes per second

The independent driver scheduled the same 360,000 signed payments over 120 seconds in each run. All 360,000 reached the socket in every run. Six B2 writes landed after the offer window. The window throughput counts payments visible on all four certified replicas by the cutoff. The p99 starts at the socket write and covers acknowledged payments that reached common finality by the end of the 45-second drain. [Per-second arrivals, process CPU, storage counters and matched payments](network/results.json)

| Run | Written by 120 s | Acknowledged by 120 s | Common by 120 s | Common TPS | Common by 165 s | Full-pool refusals | Write-to-common p99 | Validator CPU in offer (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| [Indexed A1](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/patricia-finishing/network/a1/payments.json.xz) | 360,000 | 335,177 | 246,532 | 2,054.43 | 330,500 | 24,758 | 141.027 s | 449.56–450.41 |
| [Buffered B1](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/patricia-finishing/network/b1/payments.json.xz) | 360,000 | 347,486 | 264,169 | 2,201.41 | 347,499 | 12,501 | 138.074 s | 456.23–457.60 |
| [Buffered B2](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/patricia-finishing/network/b2/payments.json.xz) | 359,994 | 341,412 | 255,238 | 2,126.98 | 341,674 | 18,326 | 136.963 s | 460.98–462.31 |
| [Indexed A2](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/patricia-finishing/network/a2/payments.json.xz) | 360,000 | 339,721 | 252,831 | 2,106.93 | 339,755 | 20,245 | 140.826 s | 452.92–453.92 |

The candidate gained 146.98 common TPS in forward order and 20.06 in reverse order. The write-to-common p99 fell by 2.952 s and 3.864 s. Across payments certified in both members of a pair, median latency fell by 1.274 s and 0.084 s respectively. Each trial was independently replayed through its certified Header. A1 ended with 4,742 acknowledged payments still outside the common height. All acknowledged payments reached common finality by the drain cutoff in the other three trials.

Whole-process CPU rose in both comparisons. The largest difference in cumulative writes at a one-second boundary was 3,671 payments in A1/B1 and 849 in A2/B2. The receivers, admission results and resulting block boundaries varied even with the same signed corpus. The repeated fixed-block savings, matching byte-level journal and successful mixed-version database reopen support using the buffered journal in block preparation. The local network gains varied by over sevenfold between pairs. Cross-host performance and sustained 3,000 common TPS remain open.

## Reopening stores across versions

A four-validator weighted Jolteon network alternated implementations, stopped a leader, finalized 96 signed payments and recovered the stopped validator. All four agreed at height 16. After a full shutdown each validator reopened its own existing RocksDB store using the other implementation. The four served the original Header, then confirmed a newly signed spend of one of the original outputs at height 18. The independent WASM verifier checked the mixed-version certificate, inclusion of the old Cell before that spend, its subsequent absence, inclusion of its successor, and the new transaction and receipt. The ledger differential test separately compares exact node bytes, tombstones and previous-value callbacks for both namespaces across nine generations of mixed changes. [Signed workload, certificates and state paths](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/patricia-finishing/reopen.json.xz) · [WASM verification](reopen.mjs)

The locked release workspace passed 510 tests with 25 marked ignored. Two snapshot tests in the first invocation exhausted the shared temporary filesystem. Running the full suite again with a disk-backed temporary directory passed both tests. [Full test output and toolchain](https://github.com/a19q3/stem/blob/12338f2fea73c8df437694da473d08e950a0938b/research/storage/patricia-finishing/workspace-checks.json.xz)

## Reproduction

Use the same release toolchain, lockfile and RISC-V contracts for each variant. The fixed-block reference was built from `88456e8`. The live reference used the existing `b808686` executable, whose Rust sources, manifests and lockfile match the fixed-block baseline. To reproduce in a separate checkout at `88456e8`, copy this study directory from the reporting branch into `research/storage/`. Apply the harness patch. The receipt-equivalence helper already exists in that revision. Save the executable printed by the Rust test build before changing implementations.

```sh
study=research/storage/patricia-finishing
git apply "$study/harness.patch"
CXXFLAGS='-include cstdint' cargo test --release --locked --test native_store --no-run
```

For the diagnostic build, apply the timing patch to the reference in a separate checkout and rebuild the same harness. For the candidate build, run `git restore --source=ebfdaf7 -- ledger/src/patricia.rs src/native_store.rs` in the disposable checkout and rebuild with the harness patch only. Select distinct paths for the three saved executables.

```sh
taskset -c 2,3 python3 "$study/measure.py" diagnostic --diagnostic "$diagnostic" --contracts "$contracts" --output phases-repeat.json
taskset -c 2,3 python3 "$study/measure.py" compare --reference "$reference" --candidate "$candidate" --contracts "$contracts" --output blocks-repeat.json
python3 "$study/measure.py" summary "$study/blocks.json"
```

For the network trials, use the [independent driver](../external/README.md), the [four-validator topology](../proposal-cadence/100ms-peers.json) and the [matched-volume preparation and verification commands](../receipt-crossover/README.md#reproduction). Initialize four fresh stores for each trial and use the indexed, buffered, buffered, indexed binary order. The driver used 256 ready senders, persistent connections, the 2,048-payment/4-billion-cycle block ceiling, two VM workers, a 100 ms proposal interval and a 45 s drain. Nodes shared twelve physical cores and their SMT siblings on a single Ryzen 9 7950X3D workstation and a two-NVMe Btrfs volume. The driver used two separate physical cores. Some auxiliary work used the remaining cores and shared machine memory and storage. Replay each payments archive with a stock verifier.

```sh
python3 research/storage/receipt-crossover/analyze.py "$study/network" --labels indexed buffered
```

To reproduce the compatible database reopen, start with a fresh fixture directory. Keep both executable paths stable for the second command. The archive stores public proofs and signed transactions.

```sh
python3 tools/jolteon_localnet.py --binary "$buffered_node" --alternate-binary "$indexed_node" --lock "$contracts/stem-lock" \
  --output "$run" --port 26910 --validators 4 --transactions 96 --offline 1 --cadence 160 --timeout 750 --offered 32
python3 "$study/reopen.py" --output "$run" --candidate "$buffered_node" --reference "$indexed_node" --archive reopen-repeat.json.xz
node "$study/reopen.mjs" /path/to/built/wasm reopen-repeat.json.xz
```

## Compact prepared write journal

The default write journal allocates one key buffer per row. The first
speculative lookup also builds a hash index with a copy of every key.
The earlier 2,048-payment child recorded 11,700 node writes. This is
a count for that fixture and does not establish how much allocation
accounts for in the current critical path.

Build with `native-compact-write-journal` to store keys up to 34 bytes in
the journal row. This covers the namespace and content ID of all
canonical Patricia nodes and state records. Longer keys use a heap
buffer. The lookup table retains the position of the newest write and
compares the full key in the immutable journal, including for
tombstones. The certified writer consumes the same key bytes and the
same ordered puts and deletes. This comparison also covers versioned
JMT keys and the System journal. Generated Patricia journals still use
their dedicated typed path when enabled.

Use the [fixed parent and child fixture](#reproduction) with the same
source, signed corpus, resident index settings and worker budget. Toggle
only the new flag. Compare a committed parent, a speculative parent
with its first lookup and repeated reads of the same prepared parent.
Take per-thread CPU and wall measurements for preparation, write-batch
construction and WAL sync. Record allocations and peak RSS across
multiple pending candidates. Also compare the generated Cell node
journal separately, since it already bypasses the general Cell node
lookup. Repeat for the JMT experiment with the same corpus and budget.

Compare the parent and child Headers, receipts, charged resources,
durable keys and values, and proof bytes between same-engine arms.
Include certified durability and reopen controls. The targeted
regression checks inline and out-of-line keys, ordered writes, repeated
keys, deletes and absent lookup. It has been compiled alongside the
existing write and proof regression targets. Runtime tests and replay
were deferred for this iteration. Inline entries are wider than a
vector handle, so resident journal size and locality must be measured
for the actual mix of key lengths before changing the default.

## Reducing certified batch construction work

`native-prepared-cell-record-keys` retains the already-derived keys of
created records in OutPoint order. The tree still sorts its own edits
by key. On finality the writer pairs the retained keys with that same
ordered immutable Cell delta. This saves one key hash per put and costs
32 bytes of key payload per retained output, plus vector capacity.
Tombstones, record bytes and ordered write operations use the existing
paths. Patricia and the experimental JMT arm use the same Cell record
keys and the same finality path.

`jolteon-reuse-write-buffer` keeps one cleared RocksDB WriteBatch on
the certified writer. A successful synchronous write or an abandoned
state prebuild can provide the allocation for the next batch. The next
batch rebuilds its contents and metrics from an empty WriteBatch. A
failed write drops its buffer. Only batches with at most 8 MiB of
serialized data are eligible for retention. The RocksDB backing capacity
can be larger than the serialized size. The synchronous fallback,
genesis and restore still assemble a fresh batch.

Use the `baseline`, `prepared-cell-keys` and `reuse-write-buffer`
matched builds. Repeat the writer comparison with prebuilding enabled
in both arms. Measure state preparation, batch-construction CPU,
`commit_jobs.reused_write_buffers` and peak RSS on full payments and
larger Cell payloads. Compare the exact stored records, independently
replayed Headers and receipts, logical batch bytes and WAL syncs.
Recycling can increase idle memory or bring little gain when commit time
is dominated by WAL synchronization. Keeping derived keys moves a small
memory cost to every prepared branch, including one that is abandoned.

The compile-checked regressions cover ordered Live Cell puts and deletes,
recycling between commits, discarding an unselected prebuild and clean
reopen after consecutive writes. Runtime regressions, interruption,
independent replay and performance measurements remain pending.
