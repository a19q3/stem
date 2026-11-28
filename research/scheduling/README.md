# Dispatching independent native verification jobs

The ignored Rust experiment signs and resolves 160 distinct input Cells. It uses the first 32 to estimate time by script-group count. The other 128 mix zero to three type scripts, including four insufficient cycle budgets. A serial reference provides the exact cycle count or complete error string for each original transaction position. All 180 dispatch trials matched every position, with four rejections and 239,286,381 total accepted cycles per trial. Every schedule uses the same bounded Rayon pool. The candidate removes the barrier after each worker-sized wave. A third mode dispatches the higher estimated costs first.

Median wall time in milliseconds over five trials after one warmup, on a Ryzen 9 7950X3D with all 32 logical CPUs available. Each trial alternates the evaluation order. The archived file also includes a 4-logical-CPU affinity sweep, individual p99 start/service times, elapsed service sums and GNU time logs.

| Workers | Fixed waves | Work stealing | Estimated longest first |
| ---: | ---: | ---: | ---: |
| 2 | 8.225 | 6.883 | 7.700 |
| 4 | 4.518 | 3.602 | 4.221 |
| 8 | 3.776 | 2.172 | 2.203 |
| 16 | 2.804 | 1.641 | 1.865 |
| 32 | 3.967 | 1.877 | 1.859 |

At 16 workers the median p99 start time was 2.454 / 1.351 / 1.412 ms. The corresponding sum of elapsed job service divided by workers × elapsed batch time was 59% / 90% / 88%. This ratio measures occupied wall time and includes preemption. Peak process RSS for the entire 4-CPU and full-socket fixtures was 153,132 and 151,932 KiB, including fixture construction and every scheduler configuration.

**Initial decision.** Try cross-wave stealing for already-resolved immutable jobs in block preparation. Estimated sorting added little in this workload.

## Native block preparation

Build with `--features native-work-stealing` to test the scheduler in `Store::prepare`. It uses the existing Rayon pool and can combine up to four consecutive waves of ordinary VM jobs when the remaining block budget covers each transaction's full cycle limit. The original wave boundaries apply to Lineage execution, Keeper actions, L1 references and protocol operations. State staging, VM cycle charging and receipt updates follow proposal order. The flag is independent of the Jolteon Admission and Candidate Pipeline features.

The block regression checks identical signed transactions, Headers, receipts and serialized Live Cell proofs against serial execution. Its cases include a later transaction consuming an earlier output, a double spend, a missing witness and an exhausted block cycle budget.

### Four-replica payment comparison

Four same-host runs used the same 18,000 signed payments, with both builds enabling `jolteon-candidate-pipeline`. The builds differed only by `native-work-stealing`. The offer was 1,800 payments per second for 10 seconds with 8 seconds of drain. Each replica had four VM workers, a 100 ms proposal cadence, and block limits of four billion cycles and 2,048 transactions. All runs admitted and commonly confirmed all 18,000 payments. The driver authenticated the common finality proof and ran the standalone history index check.

| Run order | Schedule | Mean follower preparation | Common confirmations during offer, TPS | Write-to-common p99 |
| --- | --- | ---: | ---: | ---: |
| A1 | Fixed waves | 15.00 ms | 1,751.2 | 0.486 s |
| B1 | Work stealing | 12.25 ms | 1,735.9 | 1.150 s |
| B2 | Work stealing | 11.45 ms | 1,729.8 | 1.260 s |
| A2 | Fixed waves | 15.51 ms | 1,570.8 | 1.844 s |

Mean preparation time, pooled across follower samples within each run, fell by 18% and 26% in the two pairs. Block sizes varied across runs. Window throughput and p99 moved in opposite directions across the pairs.

### Mixed Type Groups with the native pipeline

The native payment driver now accepts `STEM_STORAGE_PAYMENT_TYPE_GROUPS=12`. Each independent genesis Cell spends to a successor with the same amount, classification and TypeSet. The corpus cycles through 0–12 Type Groups. They use distinct Script identities built from the existing RISC-V amount, collateral and classification rules. All transactions use the same Lock and four code dependencies. This isolates variation in Type Groups alongside an ordinary secp256k1 check. Both the Cell read limit (32,768) and the genesis funding approach are fixed for all mixed runs. [Fixture](../../tests/native_store/storage_load.rs)

A four-replica smoke test confirmed 390 transfers. Offline receipt replay recovered 30 transfers at each tier and charged between 1,920,507 cycles at zero groups and 2,137,685 cycles at twelve groups. The serial and four-worker test uses one ordered 260-transfer block with the same fixture and compares the complete Header and receipts.

For the load comparison, both `17d41160ed5e2d89ecec40b92f62711dee3e52d8` builds enabled Admission, Follower via Commit, Leader, concurrent commitments and Commit. The candidate additionally enabled `native-work-stealing`. Four same-host validators and the driver shared logical CPUs `2-7,10-15,18-23,26-31`. Each node used four VM workers, a 100 ms cadence, a four-billion-cycle limit, and a 2,048-transaction limit. The driver scheduled 52,000 signed transfers at 2,600/s for 20 seconds with 30 seconds of drain and 64 persistent senders. The run order was fixed waves, Work Stealing, Work Stealing, fixed waves. [Earlier resource-envelope checks](../execution-pipeline/README.md#block-resource-limits-under-a-payment-burst)

| Run | VM schedule | Common TPS during offer | Write-to-common p99 | Peak acknowledged backlog | Mean proposal | Mean Follower preparation | Validator CPU |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| A1 | Fixed waves | 2,471.10 | 1.279 s | 3,438 | 12.012 ms | 32.372 ms | 105.65 s |
| B1 | Work Stealing | 1,731.75 | 24.338 s | 17,365 | 94.832 ms | 42.786 ms | 148.06 s |
| B2 | Work Stealing | 2,507.85 | 0.898 s | 2,509 | 12.269 ms | 24.651 ms | 102.77 s |
| A2 | Fixed waves | 1,548.40 | 27.149 s | 21,030 | 143.974 ms | 71.782 ms | 162.73 s |

All four runs wrote and acknowledged all 52,000 transfers inside the offer and confirmed them by drain. The common proof and history index passed. A separate read-only replay authenticated the transactions and receipts for 851 observed blocks. The cycle charge for each Type Group tier matched across runs. B1 and A2 each persisted four local timeout votes. Their highest charged blocks used 3,999,981,320 and 3,999,825,353 cycles, respectively. A1 and B2 stayed below 2.61 billion and had zero local timeouts. Severe queuing and near-full VM budgets occurred once with each scheduler. Throughput and p99 reversed direction between pairs. Keep Work Stealing opt-in, and use this fixture to compare resource budgets under a genuinely cycle-limited load. The mix uses one Lock and a maximum per-transfer cycle difference of about 11%. It does not represent heavier proof validation or mutable Head conflicts.

The binaries hashed to `b8953afa511f2b75a5b90e667a449ee9d152e7a661d051f8ab0575575566f4ea` (fixed waves) and `b3153856d62e08baffc6304ae22bdb8c2d46b91d0a7b7a54e55628776587426b` (Work Stealing). The signed workload and genesis SHA-256 values were `5a1ed47acd8565640e86dba8965c54d5949617977b413a6429dc145e2b4d37ca` and `8f43fa48be3b92af25a6c8460ace8706da03453591f65a38f53521c0c2b65116`. The fixture compiled the Type Scripts with Clang 23.1.5 and LLD.

Run `storage_load::sustained_payment_storage` with `STEM_TEST_REPLICA_BIN` and `STEM_STORAGE_PAYMENT_TYPE_GROUPS=12`. Under the `STEM_STORAGE_` prefix set `REPLICAS=4`, `RATE=2600`, `SECONDS=20`, `DRAIN=30`, `SENDERS=64`, `VM_WORKERS=4`, `RETENTION=256`, `CADENCE=100`, `BLOCK_CYCLES=4000000000`, `BLOCK_TRANSACTIONS=2048` and enable `READY_SENDERS`, `COMPLETE_VOLUME` and `REUSE_SUBMISSIONS`. The mixed fixture allocates independent input Cells at genesis and requires `PREFUND_PAYMENTS=0`, `PAYMENT_CHAINS=0` and `PAYMENT_DATA_BYTES=0`.

## Resolving and verifying a window of Cell transactions

`native-dependency-execution` is an opt-in candidate for `Store::prepare`. The earlier Work Stealing candidate starts VM verification after ordered Cell resolution and staging. This candidate passes resolution, effect construction and VM verification to the workers. A transaction reads the latest declaration for an OutPoint strictly before its own position, including deletions, then the accepted block prefix and the pinned parent. The same lookup expands Dep Groups. A consumer can verify against its producer's immutable output while the producer's authorization is still running.

The coordinator accepts results in proposal order and applies each transaction's actual resource charges before publishing its effects. If a trial fails, its charge will not fit, or it depends on an edit from a prior unadopted trial, the coordinator runs the transaction through ordered staging. This leaves independent trials eligible to be reused after a fallback. Windows are bounded by four jobs per worker, 32 total jobs and the remaining block cycles at each transaction's full cycle limit. Jobs exceeding the speculative read limit of 4 MiB fall back to ordered staging. Lineage registry changes, Keeper, bridge operations, certified L1 references and group messages use ordered staging.

The branch checks cover a same-block spend chain, conflicting spends, a replaced speculative parent, and a Dep Group created, read and consumed within one block. The branch was checked with `cargo check --locked --offline --release --all-features --tests`, `cargo build --locked --offline --release --features native-dependency-execution --bin stem` and `cargo check --locked --offline --release --no-default-features --lib --bin stem`. Regression execution, independent replay and load comparisons are pending. The preceding Work Stealing results apply to the earlier candidate.

When workload testing resumes, compare builds differing only in `native-dependency-execution` with the same workers, parent snapshots and input corpus. Independent mixed Type Groups and same-block spend chains exercise different parts of the mechanism. Record end-to-end confirmation and Follower preparation alongside worker CPU, fallbacks and discarded work. Projection adds an OutPoint index and a hash of each transaction up front. Read contention and repeat work after an invalid prefix may offset the savings on VM-cache hits or under tight resource limits.

### Greedy Leader selection

`native-leader-dependency-execution` enables the same preparation workers for the Leader's ordered mempool scan. It is independent of `native-dependency-execution`. The Leader reserves space for a window against the current block and proposal wire byte limits. Workers record which earlier projected edits they observed, including dependency-group expansion and checks for a duplicate output. The scan still visits entries in input order and applies its current block limits. It can adopt a worker result only if it adopted every source of the projected edits that result read. Failed and invalidated trials return to the original staging path, which classifies rejections and decides whether to end the scan. Independent trials can be adopted after a rejected proposal candidate. Header construction and replay use the existing origin and transaction-root rules.

Leader scans expose `parallel_cell_jobs`, `parallel_cell_reused`, `parallel_cell_fallback` and `parallel_cell_us`. The gap between jobs and the sum of reused and fallback trials counts work the scan skipped or never reached. Parallel time covers projection and worker execution. It contributes to overall selection wall time and should not be added to it. Regressions compare exact selected transaction bytes, Headers and receipts after a rejected producer, a dependent spend, a replacement spend, and a double spend, including a tight wire budget. The Dep Group fixture also checks that the Leader skips a reader following the group's consumption.

The implementation is compile-checked pending runtime and replay qualification. To attribute improvements when testing resumes, compare the original builder, Leader only, Follower only, and both stages with fixed workers and index settings. With valid independent transfers, the coordinator should reuse most trials. A churn workload should also contain stale inputs and competing spends to quantify work repeated or launched past a scan stop. Each window still waits for its workers before adopting results, and saturated blocks fall back to the existing builder once they can no longer reserve multiple full transaction cycle limits.

### Rolling Follower preparation

`native-rolling-cell-execution` extends ordinary Follower preparation. For a run longer than a fixed window, one immutable projection covers the run. Workers verify against that projection and the earlier accepted block prefix. The coordinator keeps a separate ordered delta and refills the queue after publishing a ready prefix. A slow later trial no longer holds back publication of earlier results. Trials carry the positions of the edits they read across previous window boundaries. Ordered fallback reads the actual prefix and delta, and only successful trials certify their projected writes.

The active job queue is limited to two jobs per worker. Running and completed work together stay within four slots per worker, 32 slots overall, and the remaining block cycles reserved at each transaction's full limit. Reads retain the 4 MiB per-trial limit. The projection is bounded by block transaction limits, and the short-window and ordered paths still handle small runs and protocol barriers. The Leader continues to use its separate fixed-window configuration.

`candidate_cell_stream` reports attempted jobs, adopted results, fallbacks, refills overlapping earlier unadopted jobs after the initial window, peak outstanding work, and elapsed run time including projection. Compiled fixtures compare full Headers, receipts and writes to serial preparation. They cover a refill beyond the initial window, spending across rolling boundaries and starting from a nonempty ordered prefix. Regression execution, independent replay and performance qualification are pending.

For matched tests, hold the Leader feature and resident index configuration constant and vary only the rolling option over the fixed-window Follower. Include fully independent payments and long same-block chains with unequal script costs. Inspect refill occupancy and full preparation time against coordinator and worker CPU. A large immutable projection front-loads indexing, and a slow earliest position can still fill the bounded reorder buffer. The queue hands completions through a channel, which can cost more than the barrier it removes on short VM-cache-hit workloads.

### Sharing immutable execution material

`native-execution-material-cache` lets a candidate transaction share its raw ID, complete witnessed commitment and materialized output Cells across projection, ordered staging, VM verification and commitment construction. Follower preparation attaches the cache after the block size checks. The Leader attaches it to the candidates it visits without retaining it in the mempool. An existing copy can also be reused during full preparation when the builder falls back to replay.

The hashes are computed from the original bytes and the exact network. A request for a different network computes its own digest. Newly encoded transactions start without the cache. Sharing output Cells also shares their existing content and data hash memoization. Resolution still checks live inputs, dependencies, Header ancestry, Capacity, witnesses and every Script Group. No encoding, proof format, resource budget or durable state is derived from local cache state.

The regression target checks byte and digest equivalence across independent networks, shared output material and invalidation when witnesses change. Release builds and test target compilation passed. Runtime regression, replay and load comparisons are pending. Compare the same fixed or rolling scheduler with and without the cache. Track transaction hashing, allocations, preparation wall time and RSS for ordinary payments, larger outputs and heterogeneous Polytype Cells. A per-candidate allocation and one-time output materialization can outweigh the savings for very small transactions, particularly when the Leader rejects a large fraction of the pool.

## Reproduce the resolved-job experiment

```sh
CXXFLAGS='-include cstdint' cargo test --locked --release -p stem-vm --test native --no-run
# Use the executable path printed by Cargo. The native fixture needs clang with RV64 support.
taskset -c 0-31 /usr/bin/time -v /path/to/native-test-binary measure_resolved_job_schedules --ignored --nocapture
```

## Inline Cell changes from worker trials

The optional `native-compact-cell-trial-edits` feature keeps up to two
validated output edits inline and returns up to four combined edits from
an ordinary Cell worker without allocating a per-trial tree. Larger
transactions spill both buffers as needed. Workers only hand back edits
after verification. The ordered coordinator applies input deletions and
output creations to the same block overlay after it has accepted the
dependencies and actual resource charges.

Compare this feature in isolation with either fixed windows or rolling
Leader and Follower execution. The serial, Leader and Follower fixture
includes a transaction whose edits exceed both inline buffers. For a
performance comparison, include cache-hot payments and contracts with
several outputs, and record CPU time, allocations, worker preparation
time and memory. Inline buffers enlarge each work result while reducing
small allocations. The builds and fixtures are compile-checked. Runtime
and replay evidence are pending.

## Rolling Leader preparation

The `native-rolling-leader-execution` feature extends the bounded
Follower queue to greedy Leader construction. The reference build uses
`native-leader-dependency-execution` and prepares a whole fixed window
before it starts settling that window. In rolling mode the Leader
refills available worker slots as ordered settlement advances.

The Leader inspects ordinary candidates in input order and gathers
at most 128 consecutive entries ahead of settlement. This lookahead
stays within the remaining item, transaction byte and proposal wire
budgets. Workers resolve against the exact parent, an immutable
accepted prefix and declared earlier Cell edits in the lookahead.
At most 32 jobs are outstanding, at most four per worker, with a
full transaction cycle limit reserved for each speculative position.
The running queue is limited to two jobs per worker. Protocol
operations and single entries use the existing ordered path.

The Leader and its fixed-window control share the same selection and
accounting logic. A job is reusable only after every projected producer
it read has actually been adopted and its actual resource charges fit.
A failed speculative job is rechecked against the canonical prefix.
When the remaining budget prevents another speculative job, the Leader
can finish the gathered suffix in order. Early stop cancels queued
work where possible and joins the bounded in-flight jobs. Discarded
results never enter the proposal. Follower and Leader use the same
rolling coordinator with their respective ordered settlement rules.

Compare `native-leader-dependency-execution` with
`native-rolling-leader-execution` at the same worker count and cache
settings. Keep the Follower, commitment, WAL and VM cache settings
fixed. Use independent payments, same-block spend chains and ordered
rejected entries that depend on discarded producers. Include mixed
Type Groups and resource-limited candidate sequences. Compare exact
selection, Headers, receipts and archived state, then compare proposal
selection and finality. The `rolling_cells` counters describe
submitted work, reused results, refills, discarded work, peak
outstanding work and whole-run elapsed time. That elapsed time
includes canonical fallback and should be compared with whole
selection timing rather than the fixed-window worker-only span.

The compiled fixtures compare serial and rolling proposals and check
bounded refill, early stop, conflicting dependencies and original
cycle scan behavior. The deeper lookahead can perform more speculative
work, and channel handoffs can cost more than the fixed barrier on warm
payments. Runtime correctness, worker utilization and throughput
remain to be measured.

### Reusing ordered Cell writes

The rolling schedulers track which earlier projected positions a worker
observed. Previously only an adopted worker result certified its
declarations. A rejected candidate can force the next candidate to fall
back, even when that replacement publishes the exact Cells its
descendants already verified against. A Follower can also see this
pattern when the ordinary producer needs more than the speculative read
allowance and succeeds in order.

Add `native-reuse-ordered-cell-writes` to the same rolling Leader and
Follower configuration. After the ordered path authorizes a transaction,
charges it and publishes its ordinary Cell writes, it can certify that
position. This applies when ordinary resolution keeps the original
inputs and outputs, including their transaction ID and candidate origin.
The stage withholds confirmation for materialized commands, specialized
Lineage, Keeper and message effects, and outputs omitted by bridge
handling. Rejected or skipped entries leave their projections
unconfirmed. A worker can still settle only after every source it used
is certified and the current resource budget covers the actual charges.

The `ordered_declarations` counters record successful certifications
from ordered settlement in each rolling role. `fallbacks` still counts
jobs that needed ordered handling. Compare `reused` between otherwise
identical builds to see whether confirmations actually save later work.

The compiled Leader fixture uses an unavailable input to reject a
projected spend, then includes a replacement and its child. The
Follower fixture gives the producer more than 4 MiB of dependency
data, within the ordinary block limit. Both compare exact selection
where applicable, Headers, receipts and writes against ordered
execution. They also check the fallback and reuse counters. Compare
the feature on and off using those fixtures, then use same-block
chains alongside independent payments and mixed Type Groups with
fixed caches and workers. Record total preparation, fallback and
worker time, Cell-read counts, charged resources and finality. The
benefit depends on useful consumers behind a successful fallback.
Independent payments and complete worker adoption may see little
change. Runtime regression, independent replay and timings are
pending.

### Incremental projection in the rolling schedulers

The rolling control builds the complete declaration index for a run
before it submits a worker. With `native-incremental-cell-projection`,
the coordinator only builds and publishes declarations strictly before
the next worker position. The first transaction can be submitted
without waiting for any declarations. An early stop skips entries
which could never be observed by a submitted worker. This applies to
both rolling Leader and Follower paths. Keep the shared dependency
reader setting fixed in comparisons, since its initial hint collection
can still precede the first submit.

The coordinator constructs each transaction's declarations outside
the projection lock. It publishes that transaction's edits together
before submitting a later position. Workers select the last edit
strictly before their own position even if later declarations have
now been published. They copy the owned Cell handle or check presence
while holding the read lock for that lookup. VM execution and
RocksDB reads run outside that lock. The canonical coordinator still
settles in proposal order and checks every projected source and actual
resource charge before adopting a result. An ordered fallback reads
its exact accepted prefix and delta.

Both build modes report `projected_transactions`, `projection_us` and
`first_submit_us` in rolling metrics. The timings accumulate across
runs. `first_submit_us` measures when the first job is submitted,
including other upfront work and enqueueing. Measure worker start
and occupancy separately. `projection_us` covers declaration
construction and insertion, including any insertion lock wait in
the incremental build. The incremental map can still grow to the
size of the processed run.

Toggle only `native-incremental-cell-projection` with the same rolling
roles, signed inputs, snapshot, resource limits, worker count and
caches. Use full independent payments, dependency chains, mixed Type
Groups, and a proposal sweep that reaches its early-stop policy.
Compare exact selected transactions, Headers, receipts and write
journals, then measure first submission, CPU, preparation wall
time, discarded work and finality. The compiled regression targets
add declaration counts to complete four-transaction runs and an
early-stop case. A shared lock on dependency reads could cost more
than the shorter startup saves, especially at high worker counts
and on VM-cache hits. Runtime regression, replay and performance
comparisons remain pending.

### Retiring settled projection edits

Add `native-reclaim-cell-projection` to the rolling build to fold actual
accepted Cell edits into a shared settled layer. The allocation still
keeps the authoritative candidate delta through commitment. The
additional speculative history now needs only the entries the active
worker suffix can still observe. A fold publishes canonical rows and
tombstones under the same lock that prunes the declaration history.
Both ordered and worker lookups check this layer before the original
run base. Workers first consider declarations strictly before their
own position. Jobs that already observed a declaration retain their
recorded source positions and still check them before reuse.

The coordinator folds at intervals of the initial job window.
There must be no unsettled position before the chosen boundary.
The projection cursor also skips settled positions whose edits were
never indexed, which can occur when the budget shrinks the queue.
A worker-adopted write or an explicitly certified ordered write agrees
with its projection. A skipped Leader candidate published no edits.
An outstanding worker that observed a skipped declaration still
fails the source check. A worker that reads after pruning sees the
actual accepted prefix in its place. If an ordered success did not
certify its ordinary Cell edits, the coordinator abandons the
remaining speculative results and stops launching jobs in that run.
The Leader can finish the inspected suffix in order. The Follower
returns to its ordinary block-preparation loop at that boundary.

Compare two same-source builds enabling both rolling roles. Enable
`native-incremental-cell-projection,native-reuse-ordered-cell-writes`
in the control. Add `native-reclaim-cell-projection` in the candidate.
Keep the parent, workers, Cell and VM caches, and all configured
resource limits fixed. Use full payment blocks, dependent spends
spanning multiple folds and Leader sweeps with rejected early spends.
Check selected bytes, exact Headers, ordered receipts, authenticated
roots and final durable journals against an independent reference.

Use `peak_projection_edits` and `retired_projection_edits` to measure
speculative metadata retention, and `projection_barriers` to measure
uncertified ordered settlements that end speculation. Also record
`projection_reclaim_us`, process CPU and peak RSS alongside settled
wall time and finality. Peak edit count covers the speculative
history, excluding the canonical delta and the temporary list used
to publish one transaction. The extra index lookups, lock, pruning
and final map merge can offset lower speculative retention. The
compiled fixtures cover an output read after its declaration retires
and a late replacement spend after an earlier candidate is skipped.
Runtime regressions, replay and performance comparisons are pending.

### Rolling Script Group verification

The fixed-wave verifier waits for every group in a worker-sized batch.
The `native-rolling-script-groups` candidate can refill a worker slot
while an earlier group is still running. It applies when the caller
permits parallel groups, there is more than one worker, and the
transaction spans more than one wave. It uses the existing Rayon pool
and cost estimator, with at most one executing job per worker slot
and at most two outstanding positions per worker in the transaction.
A verifier waiting on the pool can execute queued Rayon work.

Canonical settlement checks group results in group order and charges
against the current remaining cycle budget. A speculative cycle
failure with a smaller allowance is retried with that budget. A
script error from a larger allowance is retried to preserve any
earlier ordered cycle exhaustion. The coordinator prefers a ready
canonical result before refilling the queue. A failed verification
abandons pending work, and already running workers finish within
their assigned limits. The verification cache still publishes only
successful transaction verifications.

For a matched comparison use the same binary settings and toggle
only `native-rolling-script-groups`. Keep candidate and Leader task
schedulers, cache settings and CPU placement fixed. Include small
single-wave payments, multi-asset transactions with uneven script
costs, and transactions near their exact budget. Use an ordering
control that produces the same serial receipts, charged cycles, and
failure classes. Run the mixed Type Group workload with more groups
than worker slots and compare both orders. Measure worker CPU and
occupancy, verification wall time, and independent finality.
Speculative retries and channel handoffs can outweigh the avoided
barriers, particularly when groups are uniformly cheap or execution
is already fully occupied by independent transactions.

The compiled VM fixtures cover ordered charges after an undersized
speculative attempt, a script error masked by the canonical cycle
budget, and six real groups at the exact cycle limit and one cycle
below it. Runtime regression, replay and performance qualification
remain pending.
### Reusing a parallel rejection for an exact parent spend

The `native-reuse-parent-spend-rejections` option carries a missing
OutPoint from a failed fixed-window or rolling Leader trial to ordered
settlement. For an ordinary transaction without Header Deps, the first
explicit input is the first state read. If the missing point is that input
and the newest exact ancestor still has a deletion with no overriding
accepted edit, the Leader counts the rejection without resolving the
transaction a second time. Other failures use ordered staging. A missing
committed record without a parent deletion or a later input takes the
ordered path. A speculative absence by itself never authorizes a skip.

The candidate keeps visit order, the age lane, budgets and cycle-scan
limits. It changes neither mempool membership nor the Follower verifier.
In particular, the [earlier age-lane lookahead result](../storage/admission-order/README.md#branch-aware-priority-lookahead)
still supports keeping the existing order. The new path reuses a failed
preparation at its original position and applies no state effects.

Build the same revision with `native-leader-dependency-execution` and
toggle only `native-reuse-parent-spend-rejections`. Repeat the pair with
`native-rolling-leader-execution` enabled in both arms. Compare selected
bytes, complete Headers, receipts and durable writes on the same parent
and its sibling, and run independent replay for the live fixture. Report
`parallel_parent_rejections_reused`, parent and in-proposal spends,
ordered fallback, worker jobs, selection time, CPU and finality for
independent payments and mixed contracts. Parent-spend time measures the
ordered stage, while the first failed worker trial still contributes to
worker time. The historical sequential profile measured about 1.065
aggregate Leader seconds of parent-spend rejection during 180 seconds of
payments. The scope of savings here depends on how often the parallel
schedulers would have repeated that work. The regression cases for
parent spends, a sibling, nonexistent inputs and same-block duplicates
have been compile-checked. Runtime validation and performance measurements
remain pending.

### Masking rejected Leader Cell declarations

The rolling Leader can leave a skipped candidate's projected writes
visible until the next retirement interval or the end of the run. A
later payment can see a speculative spend that never reached the
accepted proposal. Its ordered recheck then repeats resolution and
possibly execution.

With `native-mask-rejected-cell-projections`, the coordinator marks a
candidate's position as rejected after the canonical scan skips it.
Subsequent reads under the existing projection lock select the newest
unmasked declaration before their own position, or fall through to the
canonical prefix or parent. A worker that read the old declaration
still carries its source position. Its result cannot be adopted when
that source was skipped, including a worker that read on both sides of
the mask update. Rejected entries remain subject to the existing bulk
retirement. The mask is indexed by positions within the bounded rolling
Leader run and is allocated only after a rejection. Only the Leader
marks rejections.

Compare the same source with rolling Leader and incremental projection
enabled in both arms, toggling only `native-mask-rejected-cell-projections`.
Repeat with identical reclaim and bounded cycle-scan settings. Use the
same ordered mempool snapshot, including a failed multi-input candidate
whose first input is available to a later replacement, dependencies
spanning the active window, and a low-conflict payment control. Check
selected bytes, exact Headers, receipts and durable writes against serial
construction. Report `masked_projection_positions`, worker fallbacks and
reuse, Leader selection time, CPU and finality. The saved rechecks depend
on when workers first read the rejected candidate. The extra projection
lookup and write-lock acquisitions can offset the gain at low conflict
or when workers have already completed their reads.

Compile coverage includes successive masked edits on the same OutPoint
and a replacement followed by its dependent spend. Runtime regressions,
independent replay and performance measurements are pending.
