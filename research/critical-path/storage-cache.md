# RocksDB block cache under sustained payment load

## Isolated 32 MiB and 256 MiB comparison

The same timing and statistics build of `eeefe97` ran the [sustained payment fixture](README.md#measurement-and-reproduction) with the configured 100 ms proposal cadence and fresh databases. The pinned RocksDB starts with a 32 MiB data block cache. The second run overrode the capacity to 256 MiB per replica using the same table factory defaults. The cache setting leaves the committed bytes and write protocol intact. Both runs acknowledged and finalized all 720,000 signed payments, with independent replay covering 1,782 blocks. The offer used the same 60-second warmup, 180-second measurement and 45-second drain.

| Measurement | 32 MiB | 256 MiB |
| --- | ---: | ---: |
| Common TPS in the measurement window | 3,001.12 | 3,004.60 |
| Write-to-common p99 | 4.556 s | 3.165 s |
| Validator CPU in the measurement window | 855.57 s | 792.88 s |
| Mean Leader build | 138.73 ms | 84.41 ms |
| Mean Follower preparation | 264.13 ms | 186.72 ms |
| Mean consensus control queue | 44.47 ms | 22.44 ms |
| Median proposal to durable visibility on all four validators | 880.94 ms | 647.62 ms |
| Data block cache hits | 25,890,773 | 71,926,952 |
| Data block cache misses | 58,968,172 | 13,266,505 |
| Data block cache hit rate | 30.5% | 84.4% |
| Aggregate WAL syncs over offer and drain | 9,761 | 11,545 |
| Aggregate WAL bytes over offer and drain | 8.83 GB | 9.14 GB |

Increasing capacity saved 7.3% of validator CPU and shortened submission p99 by 30.5% in this pair. The p99 population is payments written during measurement, including confirmations during drain. Leader and Follower time fell as proposals became smaller and more frequent. Median nonempty proposal size moved from 1,612 to 1,178 payments, and the number hitting 2,048 items fell from 59 to 5. These are system effects of the cache intervention. The comparison does not separately estimate lookup cost on a fixed block. The 18.3% increase in total sync count and 3.5% growth in WAL bytes show the storage cost of processing more blocks. Per-process cache capacity can rise by 224 MiB, and the test did not measure exact added peak RSS.

The full-throughput difference is small at a fixed 3,000/s offer. `common TPS` includes transactions submitted during warmup and first observed during measurement. A 256 MiB cache is justified here as a node-local option for CPU and finality in the payment workload. The result comes from one sustained pair, with no cross-host claim. No extra capacity is required for an existing node to reopen or continue to verify the same chain. The [digest-labelled payment timeline](finality.md#sustained-payment-timeline-and-rejected-work) provides the full consensus and storage timing. The admission trace measured `MissingLiveCell` at handler evaluation, which can occur after a relay has already waited in the inbound queue.

Run the checked branch with `stem run --rocksdb-block-cache-mib 256` plus the usual node arguments. Omitting the flag uses the library's stock cache capacity. The locked release workspace passed 530 tests, with 29 ignored. The existing durable reopen and exact Cell proof regression opens its previously written database with a 256 MiB cache. The profiling build below uses an environment variable to keep both arms binary-identical. Its SHA-256 was `e7e1f9b46030c5413968d7ea67818647ddd7fd189b1ac8215601ef5a2ceee4af`. The data-block and WAL counters are before/after RocksDB deltas summed across replicas. The cache measurements include the entire offer and drain.

<details>
<summary>Timing and database statistics patch</summary>

Apply with `git apply --unidiff-zero` to the profiled baseline. Set `STEM_PROFILE_SPANS` to a directory for each run and set `STEM_TRIAL_ROCKS_BLOCK_CACHE_MIB=256` only on the larger-cache arm. Run the usual external driver and independent verifier. Admission output rows are monotonic flush time, origin (`0` relay, `1` client), outcome index, call count, wall ns and thread CPU ns. Outcome indices in order are duplicate, capacity, missing Cell, validation failure, insertion, insertion failure.

```diff
diff --git a/src/jolteon.rs b/src/jolteon.rs
index b25566f..ddb4b21 100644
--- a/src/jolteon.rs
+++ b/src/jolteon.rs
@@ -35,0 +36,2 @@ mod client;
+mod finality_timing;
+use finality_timing::{event as trace, clock as trace_clock};
@@ -568,0 +571 @@ impl Replica {
+            trace("view_enter", self.committee.epoch(), next, self.high.point, 0, None);
@@ -598,0 +602,2 @@ impl Replica {
+            let epoch = self.committee.epoch();
+            trace("learn_before_wal", epoch, self.high.point.view, self.high.point, 0, None);
@@ -599,0 +605 @@ impl Replica {
+            trace("learn_after_wal", epoch, self.high.point.view, self.high.point, 0, None);
@@ -868,0 +875 @@ impl Replica {
+                trace("follower_begin", self.committee.epoch(), point.view, *point, proposal.header.context.height, None);
@@ -890,0 +898 @@ impl Replica {
+                trace("follower_end", self.committee.epoch(), point.view, *point, proposal.header.context.height, None);
@@ -1000,0 +1009,2 @@ impl Replica {
+            trace("commit_build", old_epoch, point.view, *point, height, None);
+            trace("proof_child", old_epoch, point.view, high.point, height, None);
@@ -1047,0 +1058 @@ impl Replica {
+            trace("commit_before_wal", old_epoch, point.view, *point, height, None);
@@ -1062,0 +1074 @@ impl Replica {
+            trace("commit_after_wal", old_epoch, point.view, *point, height, None);
@@ -1072,0 +1085 @@ impl Replica {
+            trace("commit_visible", old_epoch, point.view, *point, height, None);
@@ -1194,0 +1208 @@ impl Replica {
+        trace("vote_before_wal", self.committee.epoch(), point.view, point, 0, None);
@@ -1211,0 +1226 @@ impl Replica {
+        trace("vote_send", self.committee.epoch(), point.view, point, u64::from(member), None);
@@ -1276,0 +1292 @@ impl Replica {
+        trace("vote_accept", self.committee.epoch(), point.view, point, u64::from(share.member), None);
@@ -1277,0 +1294 @@ impl Replica {
+            trace("quorum_built", self.committee.epoch(), point.view, point, 0, None);
@@ -1433,0 +1451 @@ impl Replica {
+            trace("leader_begin", self.committee.epoch(), self.view, parent.point, parent_header.context.height.saturating_add(1), None);
@@ -1486,0 +1505 @@ impl Replica {
+        trace("leader_prepared", self.committee.epoch(), self.view, point, header.context.height, None);
@@ -1506,0 +1526 @@ impl Replica {
+        trace("proposal_send", self.committee.epoch(), self.view, point, header.context.height, None);
@@ -1673,0 +1694 @@ impl Replica {
+        let arrival = event.control_received_at.map(|received| trace_clock().saturating_sub(received.elapsed().as_nanos() as u64));
@@ -1721 +1742,10 @@ impl Replica {
-            wire::PROPOSAL => self.admit_proposal(wire::canonical_bytes(frame.body)?, peer)?,
+            wire::PROPOSAL => {
+                let handled = trace_clock();
+                let proposal: wire::Proposal = wire::canonical_bytes(frame.body)?;
+                let header = Header::decode(&proposal.header)?;
+                let id: Hash32 = header.id(self.store.network()).unpack();
+                let point = Point { view: proposal.view, parent_view: 0, header: if proposal.seal { self.committee.seal_header(id) } else { id } };
+                trace("proposal_handle", epoch, point.view, point, header.context.height, Some(handled));
+                trace("proposal_ingress", epoch, point.view, point, header.context.height, arrival);
+                self.admit_proposal(proposal, peer)?;
+            }
@@ -1758,0 +1789 @@ impl Replica {
+        let span = finality_timing::Admission::start(client);
@@ -1766,0 +1798 @@ impl Replica {
+            span.finish(0);
@@ -1769,3 +1801,14 @@ impl Replica {
-        self.admission_capacity(raw.len())?;
-        let tx = self.check_admission(raw)?;
-        self.mempool.insert(id, tx)?;
+        if let Err(error) = self.admission_capacity(raw.len()) {
+            span.finish(1);
+            return Err(error);
+        }
+        let tx = match self.check_admission(raw) {
+            Ok(tx) => tx,
+            Err(error) => {
+                span.finish(if error.is::<crate::native_store::MissingLiveCell>() { 2 } else { 3 });
+                return Err(error);
+            }
+        };
+        let result = self.mempool.insert(id, tx);
+        span.finish(if result.is_ok() { 4 } else { 5 });
+        result?;
diff --git a/src/jolteon/finality_timing.rs b/src/jolteon/finality_timing.rs
new file mode 100644
index 0000000..609446a
--- /dev/null
+++ b/src/jolteon/finality_timing.rs
@@ -0,0 +1,72 @@
+use std::{cell::RefCell, fs::File, io::{BufWriter, Write}, path::PathBuf, sync::OnceLock};
+use stem_stake::jolteon::Point;
+
+#[repr(C)]
+struct Timespec { sec: i64, nsec: i64 }
+unsafe extern "C" { fn clock_gettime(clock: i32, result: *mut Timespec) -> i32; }
+
+fn get_clock(id: i32) -> u64 {
+    let mut result = Timespec { sec: 0, nsec: 0 };
+    assert_eq!(unsafe { clock_gettime(id, &mut result) }, 0);
+    result.sec as u64 * 1_000_000_000 + result.nsec as u64
+}
+
+pub(super) fn clock() -> u64 { get_clock(1) }
+
+static DIRECTORY: OnceLock<Option<PathBuf>> = OnceLock::new();
+thread_local! { static WRITER: RefCell<Option<BufWriter<File>>> = const { RefCell::new(None) }; }
+
+pub(super) fn event(kind: &str, epoch: u64, view: u64, point: Point, value: u64, time: Option<u64>) {
+    let Some(directory) = DIRECTORY.get_or_init(|| std::env::var_os("STEM_PROFILE_SPANS").map(PathBuf::from)) else { return };
+    let at = time.unwrap_or_else(clock);
+    WRITER.with_borrow_mut(|slot| {
+        let writer = slot.get_or_insert_with(|| BufWriter::new(File::create(directory.join(format!("{}.trace", std::process::id()))).unwrap()));
+        writeln!(writer, "{at},{kind},{epoch},{view},{},{},{value}", point.view, hex::encode(point.header)).unwrap();
+        if kind == "commit_visible" { writer.flush().unwrap(); flush_admissions(directory); }
+    });
+}
+
+#[derive(Clone, Copy)]
+struct Totals { count: u64, wall: u64, cpu: u64 }
+const EMPTY: Totals = Totals { count: 0, wall: 0, cpu: 0 };
+thread_local! {
+    static ADMISSION: RefCell<[[Totals; 6]; 2]> = const { RefCell::new([[EMPTY; 6]; 2]) };
+    static ADMISSION_FILE: RefCell<Option<BufWriter<File>>> = const { RefCell::new(None) };
+}
+
+pub(super) struct Admission(Option<(bool, u64, u64)>);
+
+impl Admission {
+    pub(super) fn start(client: bool) -> Self {
+        let active = DIRECTORY.get_or_init(|| std::env::var_os("STEM_PROFILE_SPANS").map(PathBuf::from));
+        Self(active.as_ref().map(|_| (client, clock(), get_clock(3))))
+    }
+
+    pub(super) fn finish(self, result: usize) {
+        let Some((client, started, cpu)) = self.0 else { return };
+        let wall = clock() - started;
+        let cpu = get_clock(3) - cpu;
+        ADMISSION.with_borrow_mut(|totals| {
+            let part = &mut totals[usize::from(client)][result];
+            part.count += 1;
+            part.wall += wall;
+            part.cpu += cpu;
+        });
+    }
+}
+
+fn flush_admissions(directory: &PathBuf) {
+    let at = clock();
+    ADMISSION.with_borrow_mut(|totals| ADMISSION_FILE.with_borrow_mut(|slot| {
+        let writer = slot.get_or_insert_with(|| BufWriter::new(File::create(directory.join(format!("{}.admission", std::process::id()))).unwrap()));
+        for (origin, row) in totals.iter_mut().enumerate() {
+            for (result, value) in row.iter_mut().enumerate() {
+                if value.count > 0 {
+                    writeln!(writer,"{at},{origin},{result},{},{},{}",value.count,value.wall,value.cpu).unwrap();
+                    *value = EMPTY;
+                }
+            }
+        }
+        writer.flush().unwrap();
+    }));
+}
diff --git a/src/native_store.rs b/src/native_store.rs
index 770e3a3..a6700ed 100644
--- a/src/native_store.rs
+++ b/src/native_store.rs
@@ -81 +81 @@ pub(crate) struct StateReadFault;
-struct MissingLiveCell(OutPoint);
+pub(crate) struct MissingLiveCell(OutPoint);
@@ -700,0 +701,10 @@ impl Store {
+        if let Some(mib) = std::env::var_os("STEM_TRIAL_ROCKS_BLOCK_CACHE_MIB") {
+            use ckb_db::internal::{BlockBasedOptions, Cache};
+            let mib = mib.to_str().context("non-UTF8 block cache size")?.parse::<usize>()?;
+            let bytes = mib.checked_mul(1 << 20).context("block cache size overflow")?;
+            ensure!(bytes > 0, "block cache must be nonzero");
+            let cache = Cache::new_lru_cache(bytes);
+            let mut table = BlockBasedOptions::default();
+            table.set_block_cache(&cache);
+            options.set_block_based_table_factory(&table);
+        }
diff --git a/src/native_store/storage_metrics.rs b/src/native_store/storage_metrics.rs
index c7604ec..311810a 100644
--- a/src/native_store/storage_metrics.rs
+++ b/src/native_store/storage_metrics.rs
@@ -117,0 +118,7 @@ struct RocksDb {
+    block_data_hits: u64,
+    block_data_misses: u64,
+    block_index_hits: u64,
+    block_index_misses: u64,
+    memtable_hits: u64,
+    memtable_misses: u64,
+    block_cache_bytes_read: u64,
@@ -165,0 +173,7 @@ impl StorageMetrics {
+            block_data_hits: ticker("rocksdb.block.cache.data.hit")?,
+            block_data_misses: ticker("rocksdb.block.cache.data.miss")?,
+            block_index_hits: ticker("rocksdb.block.cache.index.hit")?,
+            block_index_misses: ticker("rocksdb.block.cache.index.miss")?,
+            memtable_hits: ticker("rocksdb.memtable.hit")?,
+            memtable_misses: ticker("rocksdb.memtable.miss")?,
+            block_cache_bytes_read: ticker("rocksdb.block.cache.bytes.read")?,
```

</details>
