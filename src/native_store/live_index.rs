//! Whole committed Live Cell state, indexed by OutPoint. Pinned readers keep
//! their RocksDB snapshot when this local index advances to a newer Header.

use std::{
    hash::RandomState,
    mem::size_of,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ckb_db::internal::{ReadOptions, prelude::*};
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::*,
};
use hashbrown::HashMap;
use stem_ledger::{
    LiveCell,
    patricia::{Key, Namespace},
    record,
};

use super::{LiveChanges, STATE_RECORD, Store, TIP};

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct LiveCellIndexStatus {
    pub active: bool,
    pub entries: usize,
    pub accounted_bytes: usize,
    pub limit_bytes: usize,
    pub recovery_us: u64,
    pub hits: u64,
    pub absent: u64,
    pub fallbacks: u64,
}

#[derive(Default)]
struct Records {
    rows: HashMap<OutPoint, LiveCell, RandomState>,
    payload_bytes: usize,
}

impl Records {
    fn payload(cell: &LiveCell) -> usize {
        // Include owned key and record buffers, the lazy digest allocation
        // and allocator metadata. Bucket storage is accounted separately.
        cell.cell.as_slice().len() + OutPoint::TOTAL_SIZE + 32 + 256
    }

    fn bytes(&self) -> usize {
        self.payload_bytes + self.rows.allocation_size()
    }

    fn remove(&mut self, point: &OutPoint) {
        if let Some(cell) = self.rows.remove(point) {
            self.payload_bytes -= Self::payload(&cell);
        }
    }

    fn insert(&mut self, inserted_bytes: usize, limit: usize, own: impl FnOnce() -> (OutPoint, LiveCell)) -> bool {
        let payload = self.payload_bytes.saturating_add(inserted_bytes);
        if payload.saturating_add(self.rows.allocation_size()) > limit {
            return false;
        }
        if self.rows.len() == self.rows.capacity() {
            // Reserving a single slot can double the table while the old
            // allocation is still live. Also cover the first allocation.
            let table = self.rows.allocation_size();
            let first = 8 * (size_of::<(OutPoint, LiveCell)>() + 1) + 64;
            let peak = table.saturating_mul(3).max(first);
            if payload.saturating_add(peak) > limit || self.rows.try_reserve(1).is_err() {
                return false;
            }
        }
        if payload.saturating_add(self.rows.allocation_size()) > limit {
            return false;
        }
        let (point, cell) = own();
        if let Some(old) = self.rows.insert(point, cell) {
            self.payload_bytes -= Self::payload(&old);
        }
        self.payload_bytes += inserted_bytes;
        true
    }
}

struct State {
    header: Key,
    records: Option<Records>,
}

struct Index {
    state: RwLock<State>,
    limit_bytes: usize,
    recovery_us: u64,
    hits: AtomicU64,
    absent: AtomicU64,
    fallbacks: AtomicU64,
}

#[derive(Clone)]
pub(super) struct IndexView {
    index: Arc<Index>,
    header: Key,
}

impl IndexView {
    pub(super) fn recover(store: &Store, limit_bytes: usize) -> Result<Self> {
        let started = Instant::now();
        let snapshot = store.db.snapshot();
        let tip = snapshot.get(TIP)?.context("missing committed Stem tip")?;
        ensure!(tip.as_ref() == store.tip.encode(), "Live Cell index tip changed during recovery");
        let prefix = [STATE_RECORD, Namespace::Cells.tag()];
        let mut options = ReadOptions::default();
        options.fill_cache(false);
        let mut cursor = snapshot.get_raw_iter(&options);
        cursor.seek(&prefix);

        let mut records = Some(Records::default());
        let committed = std::iter::from_fn(|| {
            let key = cursor.key()?;
            if !key.starts_with(&prefix) {
                return None;
            }
            let result = (|| {
                let id: Key = key[2..].try_into()?;
                let value = cursor.value().context("missing committed Cell record")?;
                let (point, cell) = record::decode_cell(value)?;
                ensure!(record::cell_key(&store.cells, &point) == id, "miskeyed committed Cell record");
                let digest = record::cell_value(&store.cells, &point, &cell);
                if let Some(index) = &mut records
                    && !index.insert(Records::payload(&cell), limit_bytes, || (point, cell))
                {
                    records = None;
                }
                Ok((id, digest))
            })();
            cursor.next();
            Some(result)
        });
        let root = store.cells.root_from_sorted_records(committed)?;
        ensure!(
            root.as_slice() == store.tip.state.cell_root.as_slice(),
            "durable Live Cell records differ from authenticated Cell root"
        );
        let header = store.tip.id(&store.network).unpack();
        Ok(Self {
            index: Arc::new(Index {
                state: RwLock::new(State { header, records }),
                limit_bytes,
                recovery_us: started.elapsed().as_micros() as u64,
                hits: AtomicU64::new(0),
                absent: AtomicU64::new(0),
                fallbacks: AtomicU64::new(0),
            }),
            header,
        })
    }

    pub(super) fn at_header(&mut self, header: Key) {
        self.header = header;
    }

    pub(super) fn retire(self) -> usize {
        // Snapshot replacement may still have pinned readers holding this
        // generation. Release its index before recovering a replacement.
        let records = self.index.state.write().expect("Live Cell index").records.take();
        drop(records);
        self.index.limit_bytes
    }

    pub(super) fn resident(&self) -> bool {
        let state = self.index.state.read().expect("Live Cell index");
        state.header == self.header && state.records.is_some()
    }

    /// An inner None is authoritative absence at this exact committed Header.
    pub(super) fn lookup(&self, point: &OutPoint) -> Option<Option<LiveCell>> {
        let state = self.index.state.read().expect("Live Cell index");
        if state.header == self.header
            && let Some(records) = &state.records
        {
            let cell = records.rows.get(point).cloned();
            let counter = if cell.is_some() { &self.index.hits } else { &self.index.absent };
            counter.fetch_add(1, Ordering::Relaxed);
            return Some(cell);
        }
        self.index.fallbacks.fetch_add(1, Ordering::Relaxed);
        None
    }

    pub(super) fn advance(&mut self, header: Key, changes: &LiveChanges) {
        let mut state = self.index.state.write().expect("Live Cell index");
        let mut exhausted = state.header != self.header;
        if !exhausted && let Some(records) = &mut state.records {
            for point in changes.keys() {
                records.remove(point);
            }
            for (point, change) in changes {
                let Some(cell) = change else {
                    continue;
                };
                // A Cell in a prepared batch may share the entire signed
                // transaction allocation. Keep only the exact record bytes.
                if !records.insert(Records::payload(cell), self.index.limit_bytes, || {
                    let point = OutPoint::new_unchecked(Bytes::copy_from_slice(point.as_slice()));
                    let record = LiveCell {
                        cell: cell.cell.detached(),
                        origin: stem_ledger::Origin {
                            context_id: Byte32::new_unchecked(Bytes::copy_from_slice(cell.origin.context_id.as_slice())),
                            ..cell.origin.clone()
                        },
                    };
                    (point, record)
                }) {
                    exhausted = true;
                    break;
                }
            }
        }
        let retired = if exhausted { state.records.take() } else { None };
        state.header = header;
        self.header = header;
        drop(state);
        drop(retired);
    }

    pub(super) fn status(&self) -> LiveCellIndexStatus {
        let state = self.index.state.read().expect("Live Cell index");
        LiveCellIndexStatus {
            active: state.records.is_some(),
            entries: state.records.as_ref().map_or(0, |index| index.rows.len()),
            accounted_bytes: state.records.as_ref().map_or(0, Records::bytes),
            limit_bytes: self.index.limit_bytes,
            recovery_us: self.index.recovery_us,
            hits: self.index.hits.load(Ordering::Relaxed),
            absent: self.index.absent.load(Ordering::Relaxed),
            fallbacks: self.index.fallbacks.load(Ordering::Relaxed),
        }
    }
}
