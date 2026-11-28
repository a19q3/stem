//! Whole committed Live Cell state, indexed by OutPoint. Pinned readers keep
//! their RocksDB snapshot when this local index advances past their Header.

use std::{
    borrow::Cow,
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
use ckb_db::internal::{DBRawIterator, ReadOptions, prelude::*};
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::*,
};
use hashbrown::HashMap;
use rayon::prelude::*;
use stem_ledger::{
    Cell, LiveCell, MAX_CELL_BYTES, Origin,
    patricia::{Key, Namespace, Patricia},
    record,
};

use super::{LiveChanges, STATE_RECORD, Store, TIP};

mod interned;
use interned::InternedRecords;

#[cfg(feature = "native-inline-outpoint-index")]
type ResidentKey = [u8; OutPoint::TOTAL_SIZE];
#[cfg(not(feature = "native-inline-outpoint-index"))]
type ResidentKey = OutPoint;

const KEY_PAYLOAD_BYTES: usize = if cfg!(feature = "native-inline-outpoint-index") {
    0
} else {
    OutPoint::TOTAL_SIZE
};
const ROW_PAYLOAD_OVERHEAD: usize = KEY_PAYLOAD_BYTES + 32 + 256;

fn resident_key_ref(point: &OutPoint) -> Option<&ResidentKey> {
    #[cfg(feature = "native-inline-outpoint-index")]
    return point.as_slice().try_into().ok();
    #[cfg(not(feature = "native-inline-outpoint-index"))]
    Some(point)
}

fn own_resident_key(point: Cow<'_, OutPoint>) -> ResidentKey {
    #[cfg(feature = "native-inline-outpoint-index")]
    return *resident_key_ref(&point).expect("packed OutPoint width");
    #[cfg(not(feature = "native-inline-outpoint-index"))]
    match point {
        Cow::Owned(point) => point,
        Cow::Borrowed(point) => OutPoint::new_unchecked(Bytes::copy_from_slice(point.as_slice())),
    }
}

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveCellIndexLayout {
    #[default]
    Inline,
    InternedScripts,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
pub struct LiveCellIndexStatus {
    pub active: bool,
    pub entries: usize,
    pub accounted_bytes: usize,
    pub limit_bytes: usize,
    pub recovery_us: u64,
    #[serde(default)]
    pub recovery_workers: usize,
    pub hits: u64,
    pub absent: u64,
    pub fallbacks: u64,
    #[serde(default)]
    pub snapshot_contention_fallbacks: u64,
    #[serde(default)]
    pub layout: LiveCellIndexLayout,
    #[serde(default)]
    pub descriptors: usize,
    #[serde(default)]
    pub previous_header_lookups: u64,
    #[serde(default)]
    pub previous_header_bytes: usize,
}

#[derive(Default)]
struct InlineRecords {
    rows: HashMap<ResidentKey, LiveCell, RandomState>,
    payload_bytes: usize,
}

impl InlineRecords {
    fn payload(cell: &LiveCell) -> usize {
        // Include owned record buffers, the lazy digest, allocator metadata
        // and any separate key bytes. Bucket storage is accounted separately.
        cell.cell.as_slice().len() + ROW_PAYLOAD_OVERHEAD
    }

    fn bytes(&self) -> usize {
        self.payload_bytes + self.rows.allocation_size()
    }

    fn remove(&mut self, point: &ResidentKey) {
        if let Some(cell) = self.rows.remove(point) {
            self.payload_bytes -= Self::payload(&cell);
        }
    }

    fn insert(
        &mut self,
        inserted_bytes: usize,
        limit: usize,
        own: impl FnOnce() -> (ResidentKey, LiveCell),
    ) -> bool {
        let payload = self.payload_bytes.saturating_add(inserted_bytes);
        if payload.saturating_add(self.rows.allocation_size()) > limit {
            return false;
        }
        if self.rows.len() == self.rows.capacity() {
            // Reserving a single slot can double the table while the old
            // allocation is still live. Also cover the first allocation.
            let table = self.rows.allocation_size();
            let first = 8 * (size_of::<(ResidentKey, LiveCell)>() + 1) + 64;
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

enum Records {
    Inline(InlineRecords),
    Interned(InternedRecords),
}

#[cfg_attr(feature = "native-live-cell-parent-index", derive(Clone))]
enum ResidentCell {
    Inline(LiveCell),
    Interned(interned::Row),
}

impl ResidentCell {
    #[cfg(feature = "native-live-cell-parent-index")]
    fn retained_bytes(&self) -> usize {
        match self {
            Self::Inline(cell) => InlineRecords::payload(cell),
            Self::Interned(row) => row.retained_bytes(),
        }
    }

    fn decode(self) -> Result<LiveCell> {
        match self {
            Self::Inline(cell) => Ok(cell),
            Self::Interned(cell) => cell.into_cell(),
        }
    }
}

impl Records {
    fn new(layout: LiveCellIndexLayout) -> Self {
        match layout {
            LiveCellIndexLayout::Inline => Self::Inline(InlineRecords::default()),
            LiveCellIndexLayout::InternedScripts => Self::Interned(InternedRecords::default()),
        }
    }

    fn get(&self, point: &OutPoint) -> Option<ResidentCell> {
        let point = resident_key_ref(point)?;
        match self {
            Self::Inline(records) => records.rows.get(point).cloned().map(ResidentCell::Inline),
            Self::Interned(records) => records.get(point).map(ResidentCell::Interned),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Inline(records) => records.rows.len(),
            Self::Interned(records) => records.len(),
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Inline(records) => records.bytes(),
            Self::Interned(records) => records.bytes(),
        }
    }

    fn distinct_descriptors(&self) -> usize {
        match self {
            Self::Inline(_) => 0,
            Self::Interned(records) => records.distinct_descriptors(),
        }
    }

    fn remove(&mut self, point: &OutPoint) {
        let Some(point) = resident_key_ref(point) else {
            return;
        };
        match self {
            Self::Inline(records) => records.remove(point),
            Self::Interned(records) => records.remove(point),
        }
    }

    fn insert(
        &mut self,
        point: Cow<'_, OutPoint>,
        cell: LiveCell,
        limit: usize,
        detached: bool,
    ) -> bool {
        let own_point = || own_resident_key(point);
        match self {
            Self::Inline(records) => records.insert(InlineRecords::payload(&cell), limit, || {
                let point = own_point();
                if detached {
                    (point, cell)
                } else {
                    let record = LiveCell {
                        cell: cell.cell.detached(),
                        origin: stem_ledger::Origin {
                            context_id: Byte32::new_unchecked(Bytes::copy_from_slice(
                                cell.origin.context_id.as_slice(),
                            )),
                            ..cell.origin
                        },
                    };
                    (point, record)
                }
            }),
            Self::Interned(records) => records.insert(own_point(), &cell, limit),
        }
    }
}

#[cfg(feature = "native-live-cell-parent-index")]
type BeforeImage = ([u8; OutPoint::TOTAL_SIZE], Option<ResidentCell>);

#[cfg(feature = "native-live-cell-parent-index")]
struct Previous {
    header: Key,
    edits: Vec<BeforeImage>,
    bytes: usize,
}

#[cfg(feature = "native-live-cell-parent-index")]
impl Previous {
    fn capture(
        header: Key,
        records: &Records,
        changes: &LiveChanges,
        limit: usize,
    ) -> Option<Self> {
        let budget = limit.checked_sub(records.bytes())?;
        let row_size = size_of::<BeforeImage>();
        if changes.len().checked_mul(row_size)? > budget {
            return None;
        }
        let mut edits = Vec::new();
        edits.try_reserve_exact(changes.len()).ok()?;
        let mut bytes = edits.capacity().checked_mul(row_size)?;
        if bytes > budget {
            return None;
        }
        for point in changes.keys() {
            let old = records.get(point);
            // Also charge shared buffers in full. An overwritten row or its
            // descriptors might only be owned by this generation afterward.
            bytes = bytes.checked_add(old.as_ref().map_or(0, ResidentCell::retained_bytes))?;
            if bytes > budget {
                return None;
            }
            let key = point.as_slice().try_into().expect("packed OutPoint width");
            edits.push((key, old));
        }
        // The live map orders the output index numerically. This lookup
        // compares its fixed-width key byte-for-byte.
        edits.sort_unstable_by_key(|(point, _)| *point);
        Some(Self { header, edits, bytes })
    }

    fn lookup(&self, records: &Records, point: &OutPoint) -> Option<ResidentCell> {
        match self.edits.binary_search_by(|(key, _)| key.as_slice().cmp(point.as_slice())) {
            Ok(index) => self.edits[index].1.clone(),
            Err(_) => records.get(point),
        }
    }
}

struct State {
    header: Key,
    records: Option<Records>,
    #[cfg(feature = "native-live-cell-parent-index")]
    previous: Option<Previous>,
}

impl State {
    fn covers(&self, header: Key) -> bool {
        if self.header == header {
            return true;
        }
        #[cfg(feature = "native-live-cell-parent-index")]
        if self.previous.as_ref().is_some_and(|old| old.header == header) {
            return true;
        }
        false
    }
}

struct Index {
    state: RwLock<State>,
    #[cfg(feature = "jolteon-resident-cell-publication")]
    writer_status: std::sync::Mutex<Option<(Key, LiveCellIndexStatus)>>,
    limit_bytes: usize,
    layout: LiveCellIndexLayout,
    recovery_us: u64,
    recovery_workers: usize,
    hits: AtomicU64,
    absent: AtomicU64,
    fallbacks: AtomicU64,
    #[cfg(feature = "native-pinned-index-try-read")]
    snapshot_contention_fallbacks: AtomicU64,
    #[cfg(feature = "native-live-cell-parent-index")]
    previous_header_lookups: AtomicU64,
}

#[derive(Clone)]
pub(super) struct IndexView {
    index: Arc<Index>,
    header: Key,
    #[cfg(feature = "native-pinned-index-try-read")]
    allow_snapshot_fallback: bool,
}

struct RecoveryRow {
    id: Key,
    value: Key,
    point: OutPoint,
    cell: LiveCell,
}

impl RecoveryRow {
    fn verify(tree: &Patricia, id: Key, point: OutPoint, cell: LiveCell) -> Result<Self> {
        ensure!(record::cell_key(tree, &point) == id, "miskeyed committed Cell record");
        let value = record::cell_value(tree, &point, &cell);
        Ok(Self { id, value, point, cell })
    }
}

fn read_recovery_batch(
    cursor: &mut DBRawIterator<'_>,
    prefix: &[u8],
    tree: &Patricia,
    pool: &rayon::ThreadPool,
) -> Result<Vec<Result<RecoveryRow>>> {
    const MAX_BATCH_ROWS: usize = 512;
    const MAX_BATCH_BYTES: usize = 2 * (MAX_CELL_BYTES + OutPoint::TOTAL_SIZE + Origin::LEN);
    let mut batch = Vec::new();
    let mut bytes = 0;
    while batch.len() < MAX_BATCH_ROWS {
        let Some(key) = cursor.key().filter(|key| key.starts_with(prefix)) else {
            break;
        };
        let raw = cursor.value().context("missing committed Cell record")?;
        if !batch.is_empty() && raw.len() > MAX_BATCH_BYTES - bytes {
            break;
        }
        let id: Key = key[prefix.len()..].try_into()?;
        let (point, origin, body) = record::decode_cell_prefix(raw)?;
        ensure!(body.len() <= MAX_CELL_BYTES, "Cell exceeds byte limit");
        bytes += raw.len();
        batch.push((id, point, origin, Bytes::copy_from_slice(body)));
        cursor.next();
    }
    Ok(pool.install(|| {
        batch
            .into_par_iter()
            .map(|(id, point, origin, body)| {
                let cell = LiveCell { cell: Cell::from_bytes(body)?, origin };
                RecoveryRow::verify(tree, id, point, cell)
            })
            .collect()
    }))
}

impl IndexView {
    pub(super) fn recover(
        store: &Store,
        limit_bytes: usize,
        layout: LiveCellIndexLayout,
        recovery_workers: usize,
    ) -> Result<Self> {
        ensure!(
            (1..=64).contains(&recovery_workers),
            "Live Cell recovery workers must be between 1 and 64"
        );
        let started = Instant::now();
        let pool = (recovery_workers > 1)
            .then(|| rayon::ThreadPoolBuilder::new().num_threads(recovery_workers).build())
            .transpose()?;

        let snapshot = store.db.snapshot();
        let tip = snapshot.get(TIP)?.context("missing committed Stem tip")?;
        ensure!(tip.as_ref() == store.tip.encode(), "Live Cell index tip changed during recovery");
        let prefix = [STATE_RECORD, Namespace::Cells.tag()];
        let mut options = ReadOptions::default();
        options.fill_cache(false);
        let mut cursor = snapshot.get_raw_iter(&options);
        cursor.seek(&prefix);

        let mut batch = Vec::new().into_iter();
        let decoded = std::iter::from_fn(|| {
            if let Some(pool) = &pool {
                if let Some(row) = batch.next() {
                    return Some(row);
                }
                batch = match read_recovery_batch(&mut cursor, &prefix, &store.cells, pool) {
                    Ok(rows) => rows.into_iter(),
                    Err(error) => return Some(Err(error)),
                };
                return batch.next();
            }
            let key = cursor.key().filter(|key| key.starts_with(&prefix))?;
            let row = (|| {
                let id: Key = key[2..].try_into()?;
                let raw = cursor.value().context("missing committed Cell record")?;
                let (point, cell) = record::decode_cell(raw)?;
                RecoveryRow::verify(&store.cells, id, point, cell)
            })();
            cursor.next();
            Some(row)
        });
        let mut records = Some(Records::new(layout));
        let committed = decoded.map(|row| {
            let RecoveryRow { id, value, point, cell } = row?;
            if let Some(index) = &mut records
                && !index.insert(Cow::Owned(point), cell, limit_bytes, true)
            {
                records = None;
            }
            Ok((id, value))
        });
        let root = store.cells.root_from_sorted_records(committed)?;
        ensure!(
            root.as_slice() == store.tip.state.cell_root.as_slice(),
            "durable Live Cell records differ from authenticated Cell root"
        );
        let header = store.tip.id(&store.network).unpack();
        Ok(Self {
            index: Arc::new(Index {
                state: RwLock::new(State {
                    header,
                    records,
                    #[cfg(feature = "native-live-cell-parent-index")]
                    previous: None,
                }),
                #[cfg(feature = "jolteon-resident-cell-publication")]
                writer_status: Default::default(),
                limit_bytes,
                layout,
                recovery_us: started.elapsed().as_micros() as u64,
                recovery_workers,
                hits: AtomicU64::new(0),
                absent: AtomicU64::new(0),
                fallbacks: AtomicU64::new(0),
                #[cfg(feature = "native-pinned-index-try-read")]
                snapshot_contention_fallbacks: AtomicU64::new(0),
                #[cfg(feature = "native-live-cell-parent-index")]
                previous_header_lookups: AtomicU64::new(0),
            }),
            header,
            #[cfg(feature = "native-pinned-index-try-read")]
            allow_snapshot_fallback: false,
        })
    }

    #[cfg(feature = "native-pinned-index-try-read")]
    pub(super) fn allow_pinned_snapshot_fallback(&mut self) {
        self.allow_snapshot_fallback = true;
    }

    pub(super) fn at_header(&mut self, header: Key) {
        self.header = header;
    }

    pub(super) fn retire(self) -> (usize, LiveCellIndexLayout, usize) {
        // Snapshot replacement may still have pinned readers holding this
        // generation. Release its index before recovering a replacement.
        let mut state = self.index.state.write().expect("Live Cell index");
        let records = state.records.take();
        #[cfg(feature = "native-live-cell-parent-index")]
        let previous = state.previous.take();
        drop(state);
        drop(records);
        #[cfg(feature = "native-live-cell-parent-index")]
        drop(previous);
        (self.index.limit_bytes, self.index.layout, self.index.recovery_workers)
    }

    fn read_state(&self) -> Option<std::sync::RwLockReadGuard<'_, State>> {
        #[cfg(feature = "native-pinned-index-try-read")]
        if self.allow_snapshot_fallback {
            return match self.index.state.try_read() {
                Ok(state) => Some(state),
                Err(std::sync::TryLockError::WouldBlock) => None,
                Err(std::sync::TryLockError::Poisoned(_)) => panic!("Live Cell index"),
            };
        }
        Some(self.index.state.read().expect("Live Cell index"))
    }

    pub(super) fn resident(&self) -> bool {
        self.read_state().is_some_and(|state| state.covers(self.header) && state.records.is_some())
    }

    /// An inner None is authoritative absence at this exact indexed Header.
    pub(super) fn lookup(&self, point: &OutPoint) -> Result<Option<Option<LiveCell>>> {
        let Some(state) = self.read_state() else {
            self.index.fallbacks.fetch_add(1, Ordering::Relaxed);
            #[cfg(feature = "native-pinned-index-try-read")]
            self.index.snapshot_contention_fallbacks.fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        };
        if state.covers(self.header)
            && let Some(records) = &state.records
        {
            #[cfg(not(feature = "native-live-cell-parent-index"))]
            let cell = records.get(point);
            #[cfg(feature = "native-live-cell-parent-index")]
            let cell = if state.header == self.header {
                records.get(point)
            } else {
                self.index
                    .previous_header_lookups
                    .fetch_add(1, Ordering::Relaxed);
                state
                    .previous
                    .as_ref()
                    .expect("covered previous Header")
                    .lookup(records, point)
            };
            // Both kinds hold their own buffers. Reconstruct outside the lock
            // so a large Cell cannot delay publication of the next generation.
            drop(state);
            let cell = cell.map(ResidentCell::decode).transpose()?;
            let counter = if cell.is_some() { &self.index.hits } else { &self.index.absent };
            counter.fetch_add(1, Ordering::Relaxed);
            return Ok(Some(cell));
        }
        self.index.fallbacks.fetch_add(1, Ordering::Relaxed);
        Ok(None)
    }

    pub(super) fn advance(&mut self, header: Key, changes: &LiveChanges) {
        let mut state = self.index.state.write().expect("Live Cell index");
        #[cfg(feature = "jolteon-resident-cell-publication")]
        if state.header == header {
            // The certified writer may already have published this exact view.
            self.header = header;
            return;
        }
        let mut exhausted = state.header != self.header;
        #[cfg(feature = "native-live-cell-parent-index")]
        let mut previous = {
            // Readers hold only cloned rows, so retire the older journal
            // before reserving space for the next one.
            drop(state.previous.take());
            state
                .records
                .as_ref()
                .filter(|_| !exhausted)
                .and_then(|records| {
                    Previous::capture(state.header, records, changes, self.index.limit_bytes)
                })
        };
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
                let limit = self.index.limit_bytes;
                #[cfg(feature = "native-live-cell-parent-index")]
                let limit = limit - previous.as_ref().map_or(0, |old| old.bytes);
                if !records.insert(Cow::Borrowed(point), cell.clone(), limit, false) {
                    #[cfg(feature = "native-live-cell-parent-index")]
                    if previous.take().is_some()
                        && records.insert(
                            Cow::Borrowed(point),
                            cell.clone(),
                            self.index.limit_bytes,
                            false,
                        )
                    {
                        continue;
                    }
                    exhausted = true;
                    break;
                }
            }
        }
        let retired = if exhausted { state.records.take() } else { None };
        #[cfg(feature = "native-live-cell-parent-index")]
        if !exhausted {
            state.previous = previous;
        }
        state.header = header;
        self.header = header;
        drop(state);
        drop(retired);
    }

    #[cfg(all(test, feature = "native-pinned-index-try-read"))]
    pub(super) fn while_publishing_for_test<T>(&self, task: impl FnOnce() -> T) -> T {
        let _state = self.index.state.write().expect("Live Cell index");
        task()
    }

    pub(super) fn status(&self) -> LiveCellIndexStatus {
        let state = self.index.state.read().expect("Live Cell index");
        self.status_from(&state)
    }

    #[cfg(feature = "jolteon-resident-cell-publication")]
    pub(super) fn retain_writer_status(&self) {
        let state = self.index.state.read().expect("Live Cell index");
        let status = (state.header == self.header).then(|| (self.header, self.status_from(&state)));
        *self.index.writer_status.lock().expect("Live Cell index status") = status;
    }

    #[cfg(feature = "jolteon-resident-cell-publication")]
    pub(super) fn try_current_status(&self) -> Option<LiveCellIndexStatus> {
        match self.index.state.try_read() {
            Ok(state) if state.header == self.header => return Some(self.status_from(&state)),
            Ok(_) | Err(std::sync::TryLockError::WouldBlock) => {}
            Err(std::sync::TryLockError::Poisoned(_)) => panic!("Live Cell index"),
        }
        let status = *self.index.writer_status.lock().expect("Live Cell index status");
        status.filter(|(header, _)| *header == self.header).map(|(_, value)| value)
    }

    fn status_from(&self, state: &State) -> LiveCellIndexStatus {
        #[cfg(feature = "native-live-cell-parent-index")]
        let (previous_header_bytes, previous_header_lookups) = (
            state.previous.as_ref().map_or(0, |old| old.bytes),
            self.index.previous_header_lookups.load(Ordering::Relaxed),
        );
        #[cfg(not(feature = "native-live-cell-parent-index"))]
        let (previous_header_bytes, previous_header_lookups) = (0, 0);
        LiveCellIndexStatus {
            active: state.records.is_some(),
            entries: state.records.as_ref().map_or(0, Records::len),
            accounted_bytes: state.records.as_ref().map_or(0, Records::bytes) + previous_header_bytes,
            limit_bytes: self.index.limit_bytes,
            recovery_us: self.index.recovery_us,
            recovery_workers: self.index.recovery_workers,
            hits: self.index.hits.load(Ordering::Relaxed),
            absent: self.index.absent.load(Ordering::Relaxed),
            fallbacks: self.index.fallbacks.load(Ordering::Relaxed),
            #[cfg(feature = "native-pinned-index-try-read")]
            snapshot_contention_fallbacks: self.index.snapshot_contention_fallbacks.load(Ordering::Relaxed),
            #[cfg(not(feature = "native-pinned-index-try-read"))]
            snapshot_contention_fallbacks: 0,
            layout: self.index.layout,
            descriptors: state.records.as_ref().map_or(0, Records::distinct_descriptors),
            previous_header_bytes,
            previous_header_lookups,
        }
    }
}
