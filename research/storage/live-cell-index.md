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

## Bounded parallel index recovery

`stem run --live-cell-index-mib MIB --live-cell-recovery-workers N`
uses a local worker pool for record recovery when `N` is greater than one.
The scanner reads one pinned RocksDB snapshot in disk-key order. Each
parallel batch carries at most 512 rows and about four MiB of canonical
record bytes. Workers validate the Cell encoding, bind the stored OutPoint
to its key and hash the complete record. The coordinator consumes the
results in the original order and passes them to the existing root
reconstructor. The reconstructed root must match the exact snapshot Header
before the table is visible. If the resident budget is exhausted, all
remaining batches still contribute to the root check.

The same scanner feeds Patricia and the JMT comparison build. The
authenticated tree folds, database and proof formats, and commit durability
are shared with each respective serial control. The worker setting applies
on startup and after installing a verified snapshot. A single worker uses
the direct serial recovery path and is the default. The recovery batch and
worker stacks are transient memory in addition to the existing resident
index budget. Rows processed in parallel allocate only their own Cell bytes,
so the index does not pin recovery batch buffers.

Compare 1, 2, 4 and 8 recovery workers from copies of the same committed
database and identical CPU placement, and report `recovery_us`,
`recovery_workers`, wall time to the first verified response, CPU and peak
RSS. Use the same resident layout and budget and record the RocksDB and
operating-system cache conditions. Repeat for both layouts, a tight budget
that falls back to durable reads, and an interrupted load followed by
restart and proof audit. The test launcher accepts
`STEM_TEST_LIVE_CELL_RECOVERY_WORKERS` together with
`STEM_TEST_LIVE_CELL_INDEX_MIB`. Compare serial and parallel recovery
separately on each engine before interpreting the paired JMT and Patricia
results.

The compile-checked regressions cover both layouts at reopen, persistent
worker settings after snapshot installation, rejection of records outside
the authenticated root and multi-batch verification after budget exhaustion.
The RocksDB scan, table insertion and final root fold remain ordered.
Scheduling overhead can dominate small states and the serial stages may
limit speedup on large states. Runtime and performance results are pending.

## Resident Lock and TypeSet sharing

Add `--live-cell-index-intern-scripts` alongside `--live-cell-index-mib` to test the compact resident representation. It retains one owned byte buffer per distinct Lock or nonempty TypeSet and releases that buffer after the final resident reference is removed. The remaining canonical bytes and origin belong to the Cell. Lookups copy the owned handles under the Header guard, then reconstruct and validate the canonical Cell outside the index lock. Recovery authenticates the complete original RocksDB record set against the Cell root, including when the compact index exceeds its budget. Snapshot installation retains the selected layout.

The index accounts for both hash tables and their growth, the shared descriptor bytes, and per-record allocations. The status query reports `layout` and `descriptors` alongside the existing size and hit counters. Snapshot wire bytes, proofs and durable RocksDB records use the existing encoding. The [RocksDB dictionary results](README.md#compressed-layout-and-churn-screen) still support the inline writer.

The compiled regressions cover exact typed Cell reconstruction, removal of the last sharing row, reads retaining a detached row, same-block branches, commit, reopen and snapshot replacement. Runtime tests and performance qualification are pending. Compare two runs of the same binary and budget by adding only the new flag. Keep the same funded directory copies, retention, node index, CPU set and load. Record process RSS, accounted bytes, descriptor cardinality, recovery time, Follower preparation, point-read tails and finality. Include repeated TypeSets and unique-argument controls. Reconstructing each resident read takes allocation and validation, and a low-reuse dictionary can increase both CPU and memory.

## Fixed-width resident OutPoint keys

The `native-inline-outpoint-index` build option uses the complete 36-byte
packed OutPoint directly as the hash-table key for both Live Cell layouts.
Lookups borrow those bytes from the caller. Publication copies them into the
bucket without allocating a separate key buffer or cloning a packed key.
Recovery still decodes the row and authenticates the original record set against
the durable root before activating the table. The change is confined to the
resident tables. Candidate tombstones, prior-Header before-images, the RocksDB
format and canonical proof generation continue to use the same state rules.

The memory budget charges the actual key-and-value bucket layout and table
growth, and counts those 36 bytes once, in the bucket. The allowance for
record buffers, origins and allocator overhead stays in place. Wider buckets
can offset part of the key savings, especially when the table is sparse. The
option does not change the separate representation used to decode durable
records during recovery.

For a matched comparison, build from the same revision with and without
`native-inline-outpoint-index`. Enable the resident index in both arms with
the same memory budget and run separately for inline and interned Cell values.
Reuse the fixed candidate parents and signed transactions from the isolated
access fixture, then compare at the same block resource limits and replica
configuration during sustained churn. Record row count, accounted bytes,
actual RSS, recovery time, index update and point-read latency, and preparation
time. Compare the exact Header, ordered receipts and Cell proofs with the
disk-backed control. The compile-checked prior-Header regression uses keys
with the same transaction hash and low output-index byte. It distinguishes
spent, created, unchanged and absent rows through branch, commit and reopen.
Runtime validation and performance measurements are pending.

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

## Keeping the prior durable Header resident

A worker that acquired its RocksDB snapshot before a write can continue
executing on the old Header while the Replica installs the new tip. The
current Live Cell index returns a fallback for all such lookups after its
Header advances, even for Cells untouched by the write. Earlier isolated
measurements count the matching-Header fast path. They do not measure the
frequency or cost of this transition during an overlapping job.

The optional `native-live-cell-parent-index` feature keeps the before-image
of each changed OutPoint for the immediately preceding durable Header.
A present before-image supplies the old record. An explicit absent entry
hides a newly created record. Untouched lookups read the current table.
Both the current table and the sparse before-images are published under
the same lock after the certified write returns. Speculative branch Cells
take precedence over either durable view. A reader retains its original
database snapshot for anything older or unavailable.

The previous-Header view works with inline and interned Cells. It retains
immutable owned buffers and charges their full estimated cost, including
shared descriptors and the sorted key array, against the same
`--live-cell-index-mib` limit. When a new table allocation would exceed that
limit, drop the previous view before retrying the current update. The
usual RocksDB fallback applies if the current index itself does not fit.
Each advance discards the older journal. Reopen reconstructs only the
current view from the authenticated database. Canonical Cell proof paths
and the synchronous WAL transaction are shared by both builds.

For a matched comparison, toggle only the feature and hold the Live Cell
layout and budget, Patricia index, RocksDB settings, worker count and
signed workload fixed. The relevant fixed-block control opens a snapshot
before finalization, installs a write altering membership, then runs an
identical preparation on the pinned view. Include an unaffected Cell, a
spend, a creation, an absent Cell and a reader two Headers old. Use the
existing paused-writer fixture to exercise natural overlaps and include
a no-overlap control. Compare exact Header, receipts, Cell proof bytes,
finality and recovery. `previous_header_lookups` includes both hits and
authoritative absences. `previous_header_bytes` is included in the index's
`accounted_bytes`. Record these alongside fallback counts, publication
wall time, consensus control wait, RSS and worker CPU. Copying references
and sorting changed OutPoints extend the publication lock, so the variant
could regress runs that rarely query the previous Header.

Regression targets were compiled for both memory layouts, exact old and
new proofs, budget pressure, expiry after an additional durable write and
reopening. Runtime qualification and independent replay remain pending.

#### Moving removed rows into the prior-Header journal

`native-live-cell-move-previous` transfers the owned row from each
changed OutPoint's table removal directly to the journal. It saves a
hash-table lookup and reference-count updates per resident before-image.
The interned layout releases each removed row's descriptor ownership in
the active table while the moved row keeps its own descriptor references.
The same journal captures explicit absences for creates and sorts keys by
their full packed bytes before the publisher releases the write lock.

The journal's budget still comes from the table size before removal and
charges the same rows and descriptors. If capture exceeds that budget,
the partial journal is discarded and removals continue to completion.
Pinned readers fall back to their RocksDB snapshot when the complete
journal is unavailable. The durable write and recovery paths are shared.

Compare `prior-durable-cells` and `moved-before-images` with identical
funded inputs, layouts, index budgets, churn, and commit pipelines. For
the certified writer, add `jolteon-resident-cell-publication` to both
builds and record `commit_jobs.live_cell_publication`, previous-Header
coverage, snapshot fallbacks, RSS, and CPU time. Compare with and without
pinned readers and at headroom levels that retain or discard the journal.
Sorting, allocation, and descriptor bookkeeping still occur under the
publication lock and can limit the effect. Current evidence is restricted
to compilation. Runtime replay on both layouts and measured gains are
pending.

### Sharing the pinned Live Cell cache

Leader, Follower and background admission pin their RocksDB reads to an
exact durable Header. Previously each worker pin allocated its own
decoded Cell cache. With `native-shared-pinned-cell-cache` these workers
can acquire the same bounded cache after the snapshot has matched the
full Header. A rebase through a certified prefix chooses the cache for
the recovered durable Header. The speculative suffix still checks its
own Cell changes first. The Replica's mutable committed cache stays
separate from the bank.

The bank retains one generation with the existing 4,096-record and
16 MiB record-byte limits. A worker already reading an older snapshot
can finish against its own cache handle. A later acquisition at an older
height cannot evict a newer generation. During ordinary tip installation,
if the bank is at the exact parent and no reader holds the cache, the
installer invalidates the committed block's changed OutPoints and moves
the remaining entries forward. During overlap with an older reader the
next Header uses a new generation. Peak process memory can therefore
include the retained bank and caches held by in-flight workers. This
option caches positive committed records.

Compare the default and `shared-pinned-cells` builds on the same
disk-backed funded payment and mixed-contract fixtures. Repeat with
the same verified resident index budget in both arms. Sample
`pinned_cell_cache` in the status response for shared acquisitions,
fresh generations, in-place transitions and retained record bytes.
An acquisition only measures eligible sharing. Measure actual Cell
resolution CPU and wall time, worker contention, RSS and finalized
throughput, in addition to the standard replay. A resident index can
serve these lookups first. The extra shared-cache lock can offset
saved reads when worker lifetimes overlap heavily or the index serves
most reads.

The compiled regression warms two pins on one Header, overlaps an old
pin with a commit and a new pin, and then advances an idle cache across
another commit. It checks deleted records and reuse of an unchanged
record. Runtime regressions, replay and performance comparisons remain
pending.

### Sharded decoded Live Cell caches

`native-sharded-live-cache` divides the existing decoded record cache into
eight shards. Each generation still allows at most 4,096 Live Cell records
and 16 MiB of record bytes in total. Each shard has its own lock, a
512-record allowance and a two MiB byte allowance. Shard assignment uses a
process-local hash of the complete OutPoint. A capacity flush replaces the
contents of one shard, and a record larger than its shard allowance goes
through the normal read path without being retained.

The active Replica cache and worker caches both use the selected layout.
Committed invalidation takes the shard locks in index order while applying
changed OutPoints. The pinned cache bank still admits workers only after
matching the exact durable Header. Existing readers of an older Header
keep their snapshot and separate generation. Candidate writes and
tombstones retain priority over cache hits.

Compare the same build configuration with and without
`native-sharded-live-cache`. Hold `native-shared-pinned-cell-cache`, the
resident indexes, worker counts, affinity, RocksDB settings and signed
workload fixed. When shared pins are enabled, `pinned_cell_cache.shards` and
`capacity_flushes` describe the current retained generation. Account for
generation changes when comparing counters. Record peak RSS, record bytes,
cache hits from the workload profile, Cell-resolution wall time and CPU,
and common finality during mixed-type and payment churn. Include a warm
several-worker control and a disk-backed run without the full resident
index. The prior unpinned [MultiGet comparison](../follower-preparation/README.md#other-full-block-candidates)
continues to support the existing point reader.

Shard-local limits can underuse the total allowance when access is uneven.
Mapping every lookup adds a hash, and commit invalidation still spans all
shards. The compile-checked regressions cover a capacity flush isolated
from another shard, exact invalidation and the pinned generation's
old/new Header behavior. Runtime qualification and performance results
are pending.

## Publishing Live Cells on the certified writer

With `jolteon-resident-cell-publication`, the certified writer advances
the optional Live Cell index after the atomic WAL sync and before
sending its acknowledgement. It applies the same durable Cell delta
and enforces the same memory budget for both layouts. The Replica
installs the acknowledged Header using that exact index generation,
so it applies the delta only once. The synchronous commit path still
uses the installer.

Pinned readers look up their own Header. When a worker's pinned
generation has been superseded, it takes the normal RocksDB path or
the previous-generation index if enabled and still retained.
During publication the status response can use the summary captured
for the installed Header before the write was handed to the worker.
This avoids waiting on the Live Cell index write lock or reporting a
new index under the old Replica tip. The compiled fixture covers
spending and creating Cells through the writer, an old pinned view,
Replica installation and reopening for both resident layouts.

Compare at the same live index layout, budget and worker count.
Hold `jolteon-resident-node-publication` fixed or vary it independently.
Read `commit_jobs.persistence`, `commit_jobs.live_cell_publication`,
commit acknowledgement and control queue time, plus old-view fallback
and index residency counters. Advancing the index extends the writer
window, and descendant readers can still contend on the index lock
or fall back to disk. Compare effective descendant reuse and finality
before enabling the feature more broadly. Runtime, independent
replay and recovery evidence are pending.

### Letting pinned readers proceed during publication

`native-pinned-index-try-read` lets an execution view use its
validated RocksDB snapshot when the resident Live Cell index is
write-locked. The view opts in only after its snapshot has been
matched to the committed Header or rebased to an exact certified
prefix. Branch edits still take precedence. On contention, the
ordinary lookup can use the committed decoded cache for that view
or the same RocksDB snapshot. Unpinned views keep blocking index reads.
Both the preparation-time residency check and individual OutPoint
lookups use the same lock policy.

`live_cell_index.snapshot_contention_fallbacks` counts lookups that
skipped a busy index. It is part of the existing fallback total.
It can include served decoded-cache reads, so it is distinct from
physical read counts. The compiled fixture holds the publication
lock while both the old snapshot and an exact rebased snapshot
resolve a spent and a created Cell.

Compare with identical writer placement, index budgets, memory
layout and worker count. Record contention, storage reads, useful
descendant work, tail latency and finality. This path avoids waiting
on publication, while disk fallbacks can cost more than a short
wait or increase shared storage load. Runtime, replay and
performance evidence are pending.

### Preparing Inline Live Cell records for publication

`native-prepared-live-cell-ownership` detaches new records at the end of
Leader or Follower candidate preparation. It uses a nonblocking check for
an Inline index covering the reader's base Header and drops the read
guard before copying. It skips the copy if the index is busy, absent or
interned, or the changed record payload alone exceeds the index budget.
The existing publication logic still checks the full index budget and
advances only after the atomic durable write.

Only the candidate's existing Live Cell changes are updated. The detached
Cell and context ID own their exact bytes. The synchronous installer or
certified writer can share these buffers with the index, bypassing the
record copy during publication. Speculative branch readers and persistence
consume the same canonical bytes. RocksDB, authenticated roots, and
reopening use the existing formats and recovery path.

Compare `cell-publication` and `prepared-inline-cells` from the
[integration build matrix](../native-v1-integration.md), with the Inline
index budget, funded state, signed inputs and worker count held fixed.
Capture candidate and commitment time, `commit_jobs.live_cell_publication`,
peak RSS and snapshot contention fallbacks. Include a fork or timeout
workload to count work spent on discarded candidates. Each live candidate
can retain exact copies before finality while the original proposal bytes
are still in use. Large or frequently discarded candidates may pay more
in preparation time and memory than they save during publication. The
writer fixture checks byte equality, Inline buffer sharing, old-snapshot
isolation and reopen coverage. Runtime, independent replay and performance
measurements are pending.

## Compact prepared Cell changes

`native-compact-prepared-cell-delta` freezes the candidate's settled
Live Cell changes in OutPoint order as the Cell commitment begins. The
hash stage, branch readers, Live Cell publication, and the durable writer
share that array. After successful commitment, candidates with 128–16,384
changes get a bounded hash table of 32-bit array positions. Lookup verifies the full OutPoint,
and reuses one point hash across indexed ancestors. Smaller or larger
candidates and candidates that cannot reserve the index use binary
search over the same array. Freezing moves existing record buffers.

The resolver visits ancestors from newest to oldest, so a newer
spend still masks any older creation. A miss continues to the next
ancestor or the committed snapshot. The array contains explicit
removals and preserves the exact iteration order used by both certified
writers and committed index publication. Branch validation and snapshot
rebasing use the existing contiguous Header checks. Patricia and JMT
consume the same ordered Cell changes and write the existing record
format.

The earlier disjoint-parent Follower sample reported 6,144 branch probes
and 3.99 ms of summed probe time, with zero hits. Compare
`prepared-cell-keys` and `compact-prepared-cells` with identical blocks,
VM workers, Live Cell index budget and state engine. Exercise positive
parent dependencies, speculative spends, unrelated inputs, sibling
branches, and snapshots that rebase across a committed prefix. Record
branch probe CPU, preparation and publication time, allocations, RSS,
and finality. Array construction is included in `cell_keys`, and building
the position table is included in total commitment time. Both operations
occur before voting, including for candidates never read as parents.
The table can add memory when many candidates remain in flight. Measure
shallow and deep branches as well as the committed-only control.

The compile-checked lookup fixture checks iteration order, positive
records, deletions, misses, and the binary-search path on both sides of
the index size range. A two-branch fixture checks that a newer spend
masks an indexed parent creation on only its own branch. Existing
pipeline fixtures exercise ordered persistence and snapshot rebasing. Runtime, replay and performance results are
pending.

## Retaining confirmed misses in pinned Cell snapshots

`native-pinned-cell-absence-cache` retains successful committed disk
misses in the decoded cache of a validated, pinned Header. This covers
repeated output existence checks on the same pinned view and reads
repeated by admission, Leader, Follower or a canonical fallback.
Current-block writes, speculative branches and the exact resident index
keep their existing precedence. Prefetch can read cached absences;
its own temporary disk results remain local to the candidate.

Each pinned generation has at most 4,096 absence slots distributed
among its cache shards. Shard arrays are allocated on first use and
hold fixed-width OutPoints. A slot hit requires all OutPoint bytes to
match. A collision replaces the slot. Positive decoded records keep
their separate capacity. A generation carried forward after a durable
write removes all changed OutPoints, including newly created Cells.
Workers retaining older generations keep their original snapshots.

Compare `shared-pinned-cells` and `pinned-cell-absences` at the same
worker count, state, pipeline and resident budgets. Include repeated
lookups, unique-transaction churn, resident-index hits and snapshot
replacement during a certified write. Record `pinned_cell_cache`'s
`absence_hits` and `absence_entries` for the retained cache, plus
physical reads, candidate CPU, finality and peak RSS. Unique misses
can add a cache-lock acquisition and slot replacement without yielding
future hits. Generation changes can discard reusable entries. The
previous coordinator MultiGet comparison favored the point reader.
The new option keeps the point reader in both arms.

The regression fixture checks a cached miss and a speculative creation,
exclusive reuse across finalization, an overlapping old snapshot, and
refreshing the exact new view. Runtime checks, independent replay and
performance measurements are pending.
