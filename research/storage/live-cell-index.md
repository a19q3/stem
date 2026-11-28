# Resident Live Cell index

## Recovery and branch isolation

The optional `stem run --live-cell-index-mib` index owns a complete set of
decoded Live Cells at one committed Header. Startup reads the Cell record
range from a RocksDB snapshot and checks key-to-OutPoint binding. A streaming
Patricia fold verifies the full record set against the committed Cell root
before activation. The fold keeps at most 256 unfinished branches and requires
no tree-node reads. This check also completes if the memory budget is exceeded.

Candidate spends and creations take precedence over the index. Only a matching
committed Header can use the index for membership or absence. A pinned reader
whose Header has been superseded uses its own database snapshot. The writer
updates the index incrementally after acknowledgement of the certified WAL
batch. Proof production still reads the authenticated tree. No additional
durable records or recovery logs are required.

The limit counts hash-table storage, detached canonical record bytes, OutPoint
and origin data, and a conservative allowance for the allocator and lazy Cell
hashes. Growth checks also cover the old and new tables coexisting during a
rehash. A replica exceeding the limit drops the index and uses RocksDB for
that process. The limit applies to index-owned allocations. The general page
cache, RocksDB block cache, execution arena and a pinned reader's own handles
have separate lifetimes and memory costs.

## Resident Lock and TypeSet sharing

Add `--live-cell-index-intern-scripts` alongside `--live-cell-index-mib` to test the compact resident representation. It retains one owned byte buffer per distinct Lock or nonempty TypeSet and releases that buffer after the final resident reference is removed. The remaining canonical bytes and origin belong to the Cell. Lookups copy the owned handles under the Header guard, then reconstruct and validate the canonical Cell outside the index lock. Recovery authenticates the complete original RocksDB record set against the Cell root, including when the compact index exceeds its budget. Snapshot installation retains the selected layout.

The index accounts for both hash tables and their growth, the shared descriptor bytes, and per-record allocations. The status query reports `layout` and `descriptors` alongside the existing size and hit counters. Snapshot wire bytes, proofs and durable RocksDB records use the existing encoding. The [RocksDB dictionary results](README.md#compressed-layout-and-churn-screen) still support the inline writer.

The compiled regressions cover exact typed Cell reconstruction, removal of the last sharing row, reads retaining a detached row, same-block branches, commit, reopen and snapshot replacement. Runtime tests and performance qualification are pending. Compare two runs of the same binary and budget by adding only the new flag. Keep the same funded directory copies, retention, node index, CPU set and load. Record process RSS, accounted bytes, descriptor cardinality, recovery time, Follower preparation, point-read tails and finality. Include repeated TypeSets and unique-argument controls. Reconstructing each resident read takes allocation and validation, and a low-reuse dictionary can increase both CPU and memory.

## Four local replicas

The deployed native pipeline at `4e14599` is the comparison baseline. All
arms use the same signed payment corpus
`7e67e746f550645301fd5ec515045480e6ef67446375dcbf24359f35350e34c1`
and independent copies of an authenticated common state containing 1,440,002
Live Cells. One replica retains an archive and three retain 64 recent blocks.
Two VM workers per replica share a Ryzen 9 7950X3D on logical CPUs
`2–7,10–15,18–23,26–31`. The driver uses `0–1,16–17`. All runs use 131,072
VM verification entries, a 100 ms proposal interval, a 2.4 s view timeout,
4 billion cycles, and 2,048 transactions per block. The serial build disables
only parallel Cell subtree updates. The indexed arms allow 1,536 MiB for
the index. All arms allow 256 MiB for the RocksDB cache except the 768 MiB
cache control.

The driver offers 4,000 signed payments/s for 60 seconds, with 20 seconds of
warmup, 40 seconds measured and 35 seconds of drain. Common TPS counts payments
observed at every replica in the measurement window, including warmup payments
still confirming. The conditional p99 covers measurement writes which were
acknowledged and confirmed by the end of drain. Backlog is acknowledged
payments without common confirmation at the indicated boundary. Each archive
was replayed by the independent driver and its terminal certificate checked.

| Configuration | Common TPS | Follower mean | Leader mean | Durable commit mean | Conditional p99 | ACKs pending at drain | Validator CPU during window |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Parallel, RocksDB 256 MiB, first | 3,277 | 333 ms | 244 ms | 44.9 ms | 64.49 s | 0 | 247.2 s |
| Parallel, index, first | 3,942 | 269 ms | 189 ms | 48.5 ms | 50.81 s | 0 | 245.1 s |
| Serial, index | 2,611 | 431 ms | 322 ms | 49.5 ms | 57.01 s | 48,716 | 172.6 s |
| Serial, RocksDB 256 MiB | 1,641 | 654 ms | 518 ms | 48.1 ms | 68.95 s | 32,368 | 158.3 s |
| Parallel, index, repeat | 2,253 | 476 ms | 351 ms | 72.5 ms | 68.01 s | 56,087 | 206.1 s |
| Parallel, RocksDB 256 MiB, repeat | 3,277 | 323 ms | 238 ms | 44.9 ms | 62.80 s | 0 | 251.8 s |
| Parallel, RocksDB 768 MiB | 3,482 | 308 ms | 221 ms | 45.8 ms | 60.95 s | 0 | 251.8 s |

The indexed parallel arms changed common throughput by +20.3% and −31.2%
against their controls. The repeated indexed arm recorded six local timeouts
and three stale preparations. Its p99 covers 85,870 measurements against
155,170 in the paired control. The serial indexed arm acknowledged more
payments and left more outstanding at the end. The differing cohorts make
p99 alone insufficient to rank these arms.

The indexed replicas recovered 1,440,002 rows in 2.57–2.63 s per process in
the parallel runs. The index accounted for 783 MiB per replica. Peak process
RSS was 1,186–1,222 MiB, against 701–754 MiB for the 256 MiB RocksDB control
and 1,304–1,375 MiB for the 768 MiB comparison. The larger cache is a
nearby measured RSS control. Both larger-memory approaches have workloads
and cache states that can change their peak footprint.

The resident layer removes point reads on a matching Header and leaves
Patricia node reads, the write batch and the WAL path in place. The overall
WAL byte count varied with the number of finalized payments. Aggregate
WAL totals do not establish storage savings. The hardware and test window
leave the cause of the slow indexed run unresolved. The index stays opt-in
and the deployed execution configuration remains the default.

## Isolated access and candidate preparation

The stopped archive at height 1,148 supplies the
[fixed 2,048-payment candidates](../follower-preparation/README.md). Each run
opens a fresh reflink, preloads its SSTs into the OS page cache, and warms
the process on two disjoint candidate blocks. Each candidate timing covers
16 blocks with two fixed physical cores, two execution workers and 1,365
VM cache hits per block. The lookup test bypasses VM execution and
authenticated state construction. Each sample resolves 2,048 present inputs
and 2,048 absent candidate outputs.

| Work | RocksDB median cases | Resident median cases |
| --- | ---: | ---: |
| 4,096 exact input and absence lookups | 29.65 / 30.98 ms | 0.81 / 0.84 ms |
| Full candidate, parallel subtrees | 141.35 / 142.34 ms | 120.93 / 120.79 ms |
| Full candidate, serial subtrees | 191.20 / 191.30 ms | 168.95 / 166.17 ms |
| Full candidate, speculative parent and parallel subtrees | 169.91 ms | 127.54 ms |
| Cell tree phase, parallel subtrees | 61.41 / 62.90 ms | 54.42 / 54.49 ms |
| Cell tree phase, serial subtrees | 111.83 / 110.89 ms | 101.66 / 99.30 ms |

The read-only arms returned identical canonical record digests and
presence results. The prepared arms matched exact Header encoding,
receipt digests and charged cycles for every candidate, including the
separate speculative-parent fixture. The matching committed base had no
reader fallback in these offline runs. The indexed 16-block parallel
runs saved about 14–15% preparation wall time and 13–14% process CPU
per candidate. The direct lookup results isolate the effect the index can
provide when it replaces DB reads. The full preparation results include
the remaining VM, Cell commitment and execution work.

## Crash recovery

The Linux commit fault fixture killed the indexed replica before issuing the
write, immediately before its WAL synchronization and immediately afterward.
The control replicas all used the deployed native features and the ordinary
RocksDB Cell reader. The pre-write image retained height 3. The before-sync
and after-sync images reached height 4. Each indexed process rebuilt from its
durable image, independently verified its post-restart finality proof, and
confirmed all 24 payments at the four-replica common height 10. In every
case the pending Header and proof were unavailable before write
acknowledgement. A process crash can leave unsynced data in the kernel
cache. The before-sync case does not simulate power loss.

Rust regressions compare branch spends and creations against the ordinary
reader, enforce a full-state check on reopening, detect valid-looking
uncommitted records through the root, and exercise budget
exhaustion during startup and commit. They also verify a superseded pinned
reader and compare exact receipts, Headers and proof bytes after reopen.

## Reproduction

Follow the [funded saturation fixture](../saturation/README.md#实验条件与复现)
and the [cross-block runner](../execution-pipeline/cross-block.md). Keep a
stopped four-replica copy of the funded database and its funding evidence.
Reopen independent copies under the matching archive or 64-block retention
policy. Use the external driver to replay each archive. Toggle only
`--live-cell-index-mib 1536` for the indexed comparison. Build the serial
Cell commitment control with
`--no-default-features --features native-candidate-cell-prefetch,jolteon-cross-block-pipeline`.
Run the 768 MiB control with `--rocksdb-block-cache-mib 768`.

Run the [existing certified commit fault fixture](../execution-pipeline/README.md#integrated-commit-faults-with-live-payments)
in each Linux crash mode with `--with-payments` and a target executable
wrapper which adds `--live-cell-index-mib 16` to the `run` subcommand. Query
`live_cell_index.active` after the restarted target has opened its database
and check its exact Header using the ordinary offline verifier.
