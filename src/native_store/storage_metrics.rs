//! Process-local measurements for the native database's shared durable writes.

use std::{sync::Mutex, time::Duration};

use anyhow::{Context, Result};
use ckb_db::internal::Options;
use serde::Serialize;

use super::{EXECUTION, HEADER, HEADER_ID, HISTORY, STATE_NODE, STATE_RECORD};

pub(super) struct StorageMetrics {
    options: Options,
    counts: Mutex<WriteCounters>,
}

#[derive(Clone, Copy)]
pub(super) enum Purpose {
    Bootstrap,
    Journal,
    Finality,
    Restore,
}

#[derive(Clone, Default, Serialize)]
pub(super) struct BatchBytes {
    pub serialized: u64,
    framing: u64,
    proposal_journal: u64,
    consensus_records: u64,
    execution: u64,
    headers_and_history: u64,
    live_cells: u64,
    system_records: u64,
    cell_trie: u64,
    system_trie: u64,
    other: u64,
}

impl BatchBytes {
    pub fn new(serialized: usize) -> Self {
        Self {
            serialized: serialized as u64,
            framing: serialized as u64,
            ..Self::default()
        }
    }

    /// Charge the exact difference reported by the RocksDB WriteBatch encoder.
    pub fn add(&mut self, key: &[u8], serialized: usize) {
        let serialized = serialized as u64;
        let delta = serialized - self.serialized;
        self.serialized = serialized;
        let entry = match key {
            [b'j', rest @ ..] if rest.starts_with(b"body") => &mut self.proposal_journal,
            [b'j', ..] => &mut self.consensus_records,
            [EXECUTION, ..] => &mut self.execution,
            [HEADER | HEADER_ID, ..] => &mut self.headers_and_history,
            [STATE_RECORD, 0, ..] => &mut self.live_cells,
            [STATE_RECORD, 1, ..] => &mut self.system_records,
            [STATE_NODE, 0, ..] => &mut self.cell_trie,
            [STATE_NODE, 1, ..] => &mut self.system_trie,
            _ if key == HISTORY => &mut self.headers_and_history,
            _ => &mut self.other,
        };
        *entry += delta;
    }

    fn accumulate(&mut self, sample: Self) {
        self.serialized = self.serialized.saturating_add(sample.serialized);
        self.framing = self.framing.saturating_add(sample.framing);
        self.proposal_journal = self
            .proposal_journal
            .saturating_add(sample.proposal_journal);
        self.consensus_records = self
            .consensus_records
            .saturating_add(sample.consensus_records);
        self.execution = self.execution.saturating_add(sample.execution);
        self.headers_and_history = self
            .headers_and_history
            .saturating_add(sample.headers_and_history);
        self.live_cells = self.live_cells.saturating_add(sample.live_cells);
        self.system_records = self.system_records.saturating_add(sample.system_records);
        self.cell_trie = self.cell_trie.saturating_add(sample.cell_trie);
        self.system_trie = self.system_trie.saturating_add(sample.system_trie);
        self.other = self.other.saturating_add(sample.other);
    }
}

#[derive(Clone, Default, Serialize)]
struct WriteMeasure {
    writes: u64,
    batch_bytes: BatchBytes,
    sync_microseconds: u64,
    sync_max_microseconds: u64,
    // Bucket zero covers 0 and 1 microsecond, then floor(log2(us)).
    sync_buckets: [u64; 32],
}

#[derive(Clone, Default, Serialize)]
struct WriteCounters {
    bootstrap: WriteMeasure,
    journal: WriteMeasure,
    finality: WriteMeasure,
    restore: WriteMeasure,
}

#[derive(Serialize)]
pub struct StorageSample {
    writes: WriteCounters,
    rocksdb: RocksDb,
}

#[derive(Serialize)]
struct RocksDb {
    wal_bytes: u64,
    wal_syncs: u64,
    flush_bytes: u64,
    compaction_bytes: u64,
}

impl StorageMetrics {
    pub(super) fn new(options: Options) -> Self {
        Self {
            options,
            counts: Mutex::default(),
        }
    }

    pub(super) fn commit(&self, purpose: Purpose, batch: BatchBytes, elapsed: Duration) {
        let mut counters = self.counts.lock().expect("storage write counters");
        let entry = match purpose {
            Purpose::Bootstrap => &mut counters.bootstrap,
            Purpose::Journal => &mut counters.journal,
            Purpose::Finality => &mut counters.finality,
            Purpose::Restore => &mut counters.restore,
        };
        entry.writes = entry.writes.saturating_add(1);
        entry.batch_bytes.accumulate(batch);
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        entry.sync_microseconds = entry.sync_microseconds.saturating_add(micros);
        entry.sync_max_microseconds = entry.sync_max_microseconds.max(micros);
        let bucket = micros.max(1).ilog2().min(31) as usize;
        entry.sync_buckets[bucket] = entry.sync_buckets[bucket].saturating_add(1);
    }

    pub(super) fn sample(&self) -> Result<StorageSample> {
        let stats = self
            .options
            .get_statistics()
            .context("RocksDB statistics unavailable")?;
        let ticker = |key: &str| -> Result<u64> {
            let line = stats
                .lines()
                .find_map(|line| {
                    let (name, rest) = line.split_once(' ')?;
                    (name == key).then_some(rest)
                })
                .with_context(|| format!("RocksDB statistics lacks {key}"))?;
            let (_, count) = line.split_once(':').context("invalid RocksDB ticker")?;
            Ok(count.trim().parse()?)
        };
        let rocksdb = RocksDb {
            wal_bytes: ticker("rocksdb.wal.bytes")?,
            wal_syncs: ticker("rocksdb.wal.synced")?,
            flush_bytes: ticker("rocksdb.flush.write.bytes")?,
            compaction_bytes: ticker("rocksdb.compact.write.bytes")?,
        };
        Ok(StorageSample {
            writes: self.counts.lock().expect("storage write counters").clone(),
            rocksdb,
        })
    }
}
