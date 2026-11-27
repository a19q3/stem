# Leader and Follower critical paths on Stem V1

Baseline `main@eeefe975aede7c7aef433a2ab4639c0dfaed5a3f`. The first optimisation target is **instruction decoding and trace construction during VM verification**. These costs account for at least 16.1% of sampled user CPU in sustained payments and 20.1% in mixed contracts, counting only samples with a confirmed admission or Follower role. Reducing the work at this shared point helps both synchronous admission and proposal validation. For Leader preparation alone, Patricia access and traversal is the largest target.

[Decoder experiments](decoder-cache.md) · [Digest-labelled finality timeline](finality.md)

## Ranked bottlenecks

1. **Follower VM verification and machine setup.** VM and cache waves account for 34.6% of sampled Follower preparation wall time in sustained payments and 55.6% in mixed contracts. The decoder and trace builder represent 52.23 and 6.37 sampled CPU seconds inside Follower preparation. Admission spends another 74.49 and 12.24 sampled CPU seconds in the same code. The Leader has already cached most script outcomes by the time it selects them. A per-transaction verification cache cannot remove the first required validation on each validator. The first candidate should reuse pure instruction decoding while retaining executable fetch checks, guest execution, macro-op fusion, metering and verification cache keys.
2. **Patricia reads and tree updates.** The Cell tree accounts for 56.7% of sampled Leader preparation in sustained payments and 38.2% in mixed contracts. Most of that time lies inside node lookup and decoding. This span covers branch lookup, committed RocksDB fallback and canonical node decoding. CPU stacks distinguish RocksDB and Patricia/hash work, but the span cannot independently allocate latency to the individual database read and hash. The residual tree span includes traversal, hashing and worker joins. Keys, sorting and batch insertion together take under 3% of the payment Leader sample. The shared node/hash path has higher leverage than tuning that sort.
3. **Finality progression, synchronous persistence and queueing.** Once the second Follower has prepared a block, the common observer still takes a median 948 ms in the instrumented payment run and 565 ms in mixed. This interval spans voting, subsequent certified proposals, finality persistence and polling. The exact-main synchronous finality write averages 14.09 ms in payments and 8.57 ms in mixed, with separate journal writes averaging 4.30 ms and 4.56 ms over the same whole-run counter window. This establishes a lower bound for storage time on each durable path. The post-preparation interval is the largest elapsed portion of the observed finality timeline and needs a per-view message trace before any proposal-cadence or durability change.
4. **Input and dependency resolution.** Including branch and committed Cell reads, resolution accounted for 28.2% and 40.0% of Leader preparation in payments and mixed, and 20.2% and 21.0% of Follower preparation. Committed Cell resolution exceeds speculative Cell lookup in both workloads. The speculative branch scan is real work, especially for the Leader, but removing measured already-spent input rejection altogether would recover only about 1.1 aggregate Leader seconds over the 180-second payment window.
5. **Other per-transaction work.** Effects generation and canonical application are below 7% combined on either role. The RocksDB write-batch assembly probe averages 2.54 ms per sync in payments and 0.66 ms in mixed. Separating assembly from the synchronous write matters because each includes different CPU and idle time. Aggregate runnable wait from the kernel was 1.988 seconds in the sustained window and 0.341 seconds in mixed on exact `main`. This leaves work inside VM waves and Rayon synchronization, plus application queues, as more useful software contention targets than host CPU placement alone.

## Measurements

| Trial | Exact `main` payments | Probe payments | Exact `main` mixed | Probe mixed |
| --- | ---: | ---: | ---: | ---: |
| Actual offers/s, full offer | 3,000.00 | 3,000.00 | 2,193.36 | 2,200.00 |
| Common visible/s, full offer | 2,978.35 | 2,932.78 | 2,093.56 | 2,105.52 |
| Common visible/s, measurement only | 3,005.37 | 2,938.82 | 2,093.56 | 2,105.52 |
| p99 for offers in the measurement window, s | 4.112 | 17.563 | 1.459 | 1.444 |
| Aggregate validator CPU, measurement window, s | 854.12 | 868.19 | 98.16 | 99.70 |
| Signed transactions independently replayed | 720,000 | 720,000 | 55,000 | 55,000 |
| Blocks independently replayed | 877 | 805 | 262 | 264 |

Every written transaction was acknowledged and finalized after drain. The payment measurement-window figure includes confirmations of transactions delivered during warmup. Qualification has separate availability, backlog and latency criteria. The sustained probe's larger blocks also took more work per proposal, and its p99 regression is material. Treat its percentages as diagnosis under load and use exact-main counters for absolute comparisons.

### Critical-path wall time

The phase table partitions detailed preparation elapsed time on the coordinator thread. It covers 18/54 Leader/Follower preparations in payments and 6/18 in mixed. Parallel phases include their waits once. Rows within a column are mutually exclusive. An intervening runtime delay between phase boundaries is included in the enclosing phase.

| Preparation phase, % of sampled role wall time | Payment Leader | Payment Follower | Mixed Leader | Mixed Follower |
| --- | ---: | ---: | ---: | ---: |
| Mempool traversal and selection excluding named subphases | 4.3 | — | 6.3 | — |
| Input and dependency resolution excluding measured Cell lookups | 7.9 | 5.1 | 20.1 | 9.2 |
| Speculative-parent Cell lookups | 4.2 | 2.5 | 7.8 | 3.2 |
| Committed Cell cache and database fallback | 16.1 | 12.6 | 12.1 | 8.6 |
| VM, verification-cache and worker join | 2.2 | 34.6 | 4.1 | 55.6 |
| Effects generation | 2.7 | 2.0 | 3.5 | 1.7 |
| Canonical application | 1.7 | 1.2 | 2.3 | 1.1 |
| Cell keys and sort | 0.7 | 0.5 | 1.1 | 0.6 |
| Patricia node reads and canonical node decode | 44.1 | 28.8 | 24.8 | 10.6 |
| Patricia traversal, hashing and join remainder | 12.6 | 8.3 | 13.4 | 5.6 |
| State commitment batch construction | 2.2 | 1.6 | 2.2 | 0.9 |
| Other preparation | 1.3 | 2.9 | 2.3 | 2.9 |

| Separate stage, mean wall ms per call | Exact `main` payments | Exact `main` mixed |
| --- | ---: | ---: |
| Leader build including selection and commitment | 125.65 | 27.29 |
| Follower candidate verify including decode and preparation | 240.46 | 89.84 |
| Leader Patricia phase, inclusive | 79.97 | 12.01 |
| Follower Patricia phase, inclusive | 89.62 | 12.63 |
| Finality commit, inclusive | 30.50 | 15.86 |
| Inbound consensus control queue | 35.76 | 12.27 |
| Client submission queue | 56.32 | 21.43 |
| Client submission service after dequeue | 0.20 | 0.33 |

Exact-main means use 367/1,102 Leader/Follower calls in payments and 102/303 in mixed. WAL data is taken over the offer plus drain because the storage endpoint was sampled only before and after. It includes 3,511 finality writes and 6,976 journal writes for payments, and 1,052 and 2,101 for mixed. Across the timing probe both purposes combined average 13.74 ms wall and 6.60 ms calling-thread CPU per write in payments, or 6.49 and 2.84 ms in mixed. `write_opt` includes RocksDB write processing and synchronisation in one call. The CPU-to-wall gap is a blocked-time bound which may include I/O, mutexes and scheduling. Proposal authentication averaged 0.042 ms of wall time in both workloads.

The probe records 381.21 aggregate seconds in the per-replica union of Leader build/publish, Follower verification, proposal authentication, vote and persistence intervals in sustained payments. Adding nested intervals would yield 663.36 seconds. Mixed gives 39.19 seconds of union time against 68.38 seconds of inclusive spans. This is why a summed candidate/commit/queue total cannot be subtracted from end-to-end latency. The Follower replica thread itself uses 1.61 ms of CPU on average inside a 286.60 ms payment verification, while the preparing worker accounts for 270.32 ms CPU. Mixed uses 1.00 ms on the replica thread and 75.91 ms on the preparing worker within a 90.87 ms outer call. Other worker CPU overlaps these intervals.

| Matched per-height probe, median ms | Sustained payments, 18 blocks | Mixed contracts, 5 blocks |
| --- | ---: | ---: |
| Leader build | 181.23 | 39.59 |
| Publish including its local journal | 8.88 | 8.77 |
| Publish to second Follower starting verification, bounded by the publish span | 4.13–15.61 | 5.36–15.36 |
| Second Follower verifies and prepares | 300.89 | 111.46 |
| Second Follower ready to common observer | 948.47 | 565.45 |
| Leader build start to common observer | 1,408.73 | 709.48 |

Each row uses its own median. The second completing Follower approximates the preparation requirement for a quorum on this equally weighted four-validator fixture. The sample does not identify the exact vote that formed each certificate.

### CPU flamegraphs

[Payment flamegraph](payment.svg) · [Mixed-contract flamegraph](mixed.svg). The bottom bands separate the call paths. Bar widths represent sampled user CPU across all four validators. Tooltips show weighted sample time and proportion. The exact-main leaf classification assigns 33.0% and 41.4% of samples to recognisable VM and RISC-V cryptography symbols, and 27.0% and 18.6% to recognisable Patricia, RocksDB and hash symbols. Those symbols can occur under different callers. The stack probe partitions by temporal role and exposes those callers.

| Probe sampled user CPU, seconds | Payments | Mixed |
| --- | ---: | ---: |
| Follower prepare on all relevant workers | 364.97 | 34.05 |
| Local and relayed admission | 146.06 | 31.31 |
| Leader prepare on all relevant workers | 64.79 | 3.54 |
| RocksDB background | 41.55 | 0.62 |
| Durable batch assembly and synchronous writes | 30.91 | 3.59 |
| Vote, publish, proposal checks and recognisable communication | 5.48 | 0.84 |
| Other or truncated | 132.13 | 18.67 |
| Total | 785.90 | 92.63 |

A stack without a recognisable caller can still be classified when its thread is inside a measured span. Fully unsymbolised frame-pointer samples account for 10.8% and 8.5% respectively, included in the table rather than renormalised away. `schedstat` CPU includes system work unavailable to this user-only sampler.

| Subpath with a recognised caller, sampled CPU seconds | Payment Leader | Payment Follower | Mixed Leader | Mixed Follower |
| --- | ---: | ---: | ---: | ---: |
| Active RocksDB reads during preparation | 31.59 | 112.46 | 1.34 | 4.42 |
| Patricia traversal and hashing with a recognised state commitment caller | 6.60 | 21.08 | 0.41 | 1.55 |
| VM and verification-cache work | 0.58 | 105.02 | 0.06 | 16.39 |

The RocksDB row covers both input and Patricia reads. The tree row captures identifiable traversal and hash frames outside the RocksDB bucket. Samples with unresolved callers are outside these lower bounds. Separately, identified admission and Follower VM samples included 126.72 s of decoding and trace construction, 107.54 s of remaining guest work and 4.82 s of verification-cache machinery or unresolved VM work for payments. The corresponding mixed figures were 18.61 s, 26.66 s and 0.68 s.

### Useful, duplicated, rejected and speculative work

- **Useful and replicated.** Exact `main` had 2,158,162 verification misses and 1,389,877 hits during the sustained window, and 218,639 misses and 148,862 hits in mixed. The exact full-run deltas were 2,880,000 and 220,000 misses respectively, four per signed transaction, with zero cache entries rejected for a parent mismatch. VM workers were performing required independent replica execution. Admission, the Leader and each Follower still resolved and applied related transactions at different lifecycle stages. Neither cache capacity growth nor parent-key relaxation is supported by this miss pattern.
- **Rejected input scans.** In the sustained window, Leaders inspected 864,725 entries, selected 539,420 and skipped 325,305 already spent in the candidate parent, using 1.065 s of measured rejection time. Mixed inspected 85,214, selected 54,737 and skipped 30,477 parent-spent entries in 0.107 s. Both windows recorded zero measured block-cycle scan rejections.
- **Repeated submissions and relay work.** The payment window recorded 1,487,084 client or relay requests and 1,451,119 admission checks. The 35,965-request difference includes deduplication and pre-verification rejection. It cannot be allocated between the two from these counters. There were 89,325 retry attempts. The full payment run also recorded 75,363 rejected replica messages and 11,836 transport drops across four replicas. `invalid_messages` is a shared counter for all peer-handler errors. It cannot distinguish delayed relays, invalid proposals and other rejected messages. Mixed registered zero replica-message errors or transport drops. All client payments still finalized.
- **Speculation.** Preparing an eligible candidate before its parent has committed uses the authenticated branch view. The role table charges those reads. Archive replay distinguishes the committed history, while the height-only spans do not prove the identity of every block considered before finality. There were no unmatched heights in the per-height sampled cohorts and no local view timeouts in the exact-main runs. A speculative preparation later revoked outside the sample can only be quantified with a digest-labelled trace. Stale background-job counters are inapplicable to the synchronous configuration.

## Measurement and reproduction

Tests ran on one AMD Ryzen 9 7950X3D with four loopback validators, two VM workers per replica, three 64-block history windows and one archive. Validators used CPUs `2-7,10-15,18-23,26-31`, the load driver `0-1,16-17` and the profiler `8,9,24,25`. Stores were fresh on the same Btrfs NVMe volume with asynchronous discard and `zstd:1` compression. Both workloads used 100 ms proposal cadence, 2,400 ms view timeout, 4 billion block cycles and a 2,048-transaction cap. The cache held 131,072 verification entries for payments and 65,536 for mixed. Both binaries used the default feature set. No admission, candidate, Leader or commit experiment was enabled.

The payment run offered 3,000 per second for 60 seconds of warmup plus 180 seconds of measurement and 45 seconds of drain, with 256 persistent ready senders. Its signed workload digest was `5ecfc85a430edd167c04f6b568235785ddf6fb61d5dad6b89fec3328f1985939`. The mixed run offered 2,200 per second for 25 seconds plus 20 seconds of drain, with 64 persistent ready senders and zero to twelve Type Script Groups on each payment. Its digest was `85f80e6e8a2448944da69b7faaa3e662d6e8506c12749a0dc2ed4aa1ba7f652e`. Both fixtures come from the [external payment trial harness](../storage/external/README.md) and the [mixed subtree ablation](../patricia/README.md#mixed-type-group-ablation-and-default). The latter workload exercises the standard Lock and mixed Type Groups. AMM, CLOB, hot Lineage and Keeper calls belong to separate fixtures.

Release executable SHA-256 fingerprints were `3932e122d470201ff64f4a2b07a58ca895aefefc977426b9d722a9b49df16afc` for exact `main` and `cdbf1e8d9dd8df4b15c65ebfca9521bfac642042aca8a88ccfda2b6bc179235c` for the symbolised probe. The probe adds clocks and stage spans only. `CLOCK_MONOTONIC` timestamps align with `perf --clockid mono` and `CLOCK_THREAD_CPUTIME_ID` measures scoped user plus system CPU. Detailed phases sample heights congruent to 3 modulo 17. The probe preserves frame pointers in Stem and local Rust dependencies with `-Cforce-frame-pointers=yes -Cforce-unwind-tables=yes`. Exact-main unwinding frequent stops made its call graphs unreliable, so its sampled stack evidence is limited to leaf symbols.

Launch `research/storage/external/drive.py measure` on each prepared trial while `perf record --clockid mono --all-user --user-callchains -e task-clock:u -F 97 --call-graph fp -p <validator-pids>` samples the probe. Use `--call-graph dwarf,8192` for the exact-main leaf reference. Collect per-thread `/proc/<pid>/task/<tid>/stat` and `schedstat` snapshots once a second. Convert stack events with `perf script -F comm,pid,tid,time,period,ip,sym,dso`. For exact-main leaves use `perf script -G` with the same fields. Run `drive.py verify` in a separate process on each output. It reconstructs the signed workload and ordered transaction roots, checks the delivery ledger, and authenticates the final certificate. Independent replay verifies each history on its own timestamps and block boundaries.

A span can have children on the same thread and work on other Rayon workers. The phase breakdown charges each instant to the deepest same-thread span. For example, a Patricia read inside resolution is not added again to the enclosing resolve time. A parallel VM wave is counted once on the waiting critical path. Separate thread CPU can exceed elapsed time when workers overlap. The path timeline matches heights only when there is one Leader and three distinct Follower preparations in the trace. The second Follower is the earliest readiness implied by three-of-four voting power plus the Leader's own execution. Preparation completion precedes its actual vote. Egress is bounded by the start and end of `leader_publish`; the gap also includes decode, authentication, queueing and scheduling. The post-preparation remainder includes votes, subsequent Jolteon proposals and quorum certificates, checkpoint-local finality write and observer polling. No packet timestamp or causal cross-view vote trace supports splitting those further. Same-host placement and profiling overhead limit generalization to production RTT or cross-host load.

The proof of this experiment is the independent replay, exact-main counter measurements, per-role wall attribution and sampled CPU evidence. The unclassified samples and height-join exclusions are retained in the report. The main run and the timing probe form the first controlled comparison with identical signed inputs. Follow-up experiments compare candidate code against a fresh uninstrumented baseline in both execution orders.

<details>
<summary>Timing-only probe patch</summary>

Apply this to a detached checkout of the baseline with `git apply --unidiff-zero`. Use separate release binaries and do not enable the timing environment variable on the exact-main runs.

```diff
diff --git a/src/critical_probe.rs b/src/critical_probe.rs
new file mode 100644
index 0000000..c9208ba
--- /dev/null
+++ b/src/critical_probe.rs
@@ -0,0 +1,77 @@
+//! Temporary diagnostics used only by the critical-path research build.
+use std::{
+    cell::{Cell, RefCell},
+    fs::File,
+    io::{BufWriter, Write},
+    path::PathBuf,
+    sync::OnceLock,
+};
+
+#[repr(C)]
+struct Timespec { sec: i64, nsec: i64 }
+
+unsafe extern "C" {
+    fn clock_gettime(clock: i32, result: *mut Timespec) -> i32;
+    fn gettid() -> i32;
+}
+
+fn clock(id: i32) -> u64 {
+    let mut result = Timespec { sec: 0, nsec: 0 };
+    assert_eq!(unsafe { clock_gettime(id, &mut result) }, 0);
+    result.sec as u64 * 1_000_000_000 + result.nsec as u64
+}
+
+#[derive(Clone, Copy)]
+struct Context { role: &'static str, height: u64, detailed: bool }
+thread_local! {
+    static CONTEXT: Cell<Option<Context>> = const { Cell::new(None) };
+    static WRITER: RefCell<Option<BufWriter<File>>> = const { RefCell::new(None) };
+}
+static DIRECTORY: OnceLock<Option<PathBuf>> = OnceLock::new();
+
+fn directory() -> Option<&'static PathBuf> {
+    DIRECTORY.get_or_init(|| std::env::var_os("STEM_PROFILE_SPANS").map(PathBuf::from)).as_ref()
+}
+
+pub struct Span(Option<Event>);
+struct Event {
+    name: &'static str,
+    context: Context,
+    start: u64,
+    cpu: u64,
+    root: Option<Option<Context>>,
+}
+
+pub fn root(role: &'static str, height: u64) -> Span {
+    if directory().is_none() { return Span(None); }
+    let context = Context { role, height, detailed: height % 17 == 3 };
+    let previous = CONTEXT.with(|cell| cell.replace(Some(context)));
+    Span(Some(Event { name: role, context, start: clock(1), cpu: clock(3), root: Some(previous) }))
+}
+
+pub fn phase(name: &'static str) -> Span {
+    let context = CONTEXT.get();
+    Span(context.filter(|context| context.detailed).map(|context| Event {
+        name, context, start: clock(1), cpu: clock(3), root: None,
+    }))
+}
+
+impl Drop for Span {
+    fn drop(&mut self) {
+        let Some(event) = self.0.take() else { return };
+        let end = clock(1);
+        let cpu = clock(3) - event.cpu;
+        WRITER.with_borrow_mut(|writer| {
+            let writer = writer.get_or_insert_with(|| {
+                let path = directory().unwrap().join(format!("{}-{}.csv", std::process::id(), unsafe { gettid() }));
+                BufWriter::new(File::create(path).expect("create profile output"))
+            });
+            writeln!(writer, "{},{},{},{},{},{}", event.context.role, event.context.height,
+                event.name, event.start, end, cpu).expect("write profile record");
+            if event.root.is_some() { writer.flush().expect("flush profile output"); }
+        });
+        if let Some(previous) = event.root {
+            CONTEXT.set(previous);
+        }
+    }
+}
diff --git a/src/jolteon.rs b/src/jolteon.rs
index b25566f..6551a00 100644
--- a/src/jolteon.rs
+++ b/src/jolteon.rs
@@ -628,0 +629 @@ impl Replica {
+        let _check = crate::critical_probe::root("proposal_authentication", 0);
@@ -867,0 +869 @@ impl Replica {
+                let _whole = crate::critical_probe::root("follower_verify", proposal.header.context.height);
@@ -868,0 +871 @@ impl Replica {
+                let decoding = crate::critical_probe::phase("proposal_decode");
@@ -874,0 +878 @@ impl Replica {
+                drop(decoding);
@@ -875,0 +880 @@ impl Replica {
+                    let _work = crate::critical_probe::root("follower_store", proposal.header.context.height);
@@ -1156,0 +1162 @@ impl Replica {
+        let _vote = crate::critical_probe::root("vote", 0);
@@ -1433,0 +1440,2 @@ impl Replica {
+            let height = parent_header.context.height.saturating_add(1);
+            let whole = crate::critical_probe::root("leader_build", height);
@@ -1436,0 +1445 @@ impl Replica {
+                let _work = crate::critical_probe::root("leader_store", height);
@@ -1437,0 +1447 @@ impl Replica {
+                    let traversal = crate::critical_probe::phase("mempool_ordering");
@@ -1438,0 +1449 @@ impl Replica {
+                    drop(traversal);
@@ -1454,0 +1466,2 @@ impl Replica {
+            drop(whole);
+            let _publish = crate::critical_probe::root("leader_publish", height);
@@ -1639,0 +1653 @@ impl Replica {
+        let _handling = event.control_received_at.map(|_| crate::critical_probe::root("control", 0));
diff --git a/src/lib.rs b/src/lib.rs
index bf52ab8..afbda70 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,0 +2,2 @@
+mod critical_probe;
+#[cfg(unix)]
diff --git a/src/native_store.rs b/src/native_store.rs
index 770e3a3..f4f5729 100644
--- a/src/native_store.rs
+++ b/src/native_store.rs
@@ -1287,0 +1288 @@ impl Store {
+        let lookup = crate::critical_probe::phase("speculative_cell_lookup");
@@ -1292,0 +1294,2 @@ impl Store {
+        drop(lookup);
+        let _lookup = crate::critical_probe::phase("committed_cell_lookup");
@@ -1413,0 +1417 @@ impl Store {
+        let _read = crate::critical_probe::phase("patricia_node_read");
@@ -1570,0 +1575 @@ impl Store {
+                let verification = crate::critical_probe::phase("parallel_vm_and_cache");
@@ -1576,0 +1582 @@ impl Store {
+                drop(verification);
@@ -1694,0 +1701 @@ impl Store {
+        let keys = crate::critical_probe::phase("cell_keys_and_sort");
@@ -1705,0 +1713,2 @@ impl Store {
+        drop(keys);
+        let tree = crate::critical_probe::phase("patricia_cell_tree");
@@ -1725,0 +1735,2 @@ impl Store {
+        drop(tree);
+        let _batch = crate::critical_probe::phase("cell_commitment_batch");
@@ -1875,0 +1887 @@ impl Store {
+        let select = crate::critical_probe::phase("mempool_selection");
@@ -1877,0 +1890 @@ impl Store {
+        drop(select);
@@ -2304,0 +2318 @@ impl Store {
+        let resolution = crate::critical_probe::phase("transaction_resolution");
@@ -2346,0 +2361,2 @@ impl Store {
+        drop(resolution);
+        let effects = crate::critical_probe::phase("generate_effects");
@@ -2460,0 +2477,2 @@ impl Store {
+        drop(effects);
+        let verification = crate::critical_probe::phase("inline_vm_and_cache");
@@ -2510,0 +2529,2 @@ impl Store {
+        drop(verification);
+        let _application = crate::critical_probe::phase("canonical_application");
@@ -2825,0 +2846 @@ fn sync_with_consensus(
+    let assembly = crate::critical_probe::root("durable_batch", 0);
@@ -2881,0 +2903 @@ fn sync_with_consensus(
+    drop(assembly);
@@ -2882,0 +2905 @@ fn sync_with_consensus(
+    let _sync = crate::critical_probe::root("sync_wal", 0);
```

</details>
