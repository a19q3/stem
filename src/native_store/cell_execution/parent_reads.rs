//! Successful parent reads available to the same transaction's ordered retry.

use ckb_types::packed::OutPoint;
use smallvec::SmallVec;
use stem_ledger::LiveCell;

use crate::native_store::LiveCache;

const MAX_RECORDS: usize = 8;
const MAX_RECORD_BYTES: usize = 64 * 1024;
type Entry = (OutPoint, Option<LiveCell>);

#[derive(Default)]
pub(super) struct ParentReads {
    records: SmallVec<[Entry; 2]>,
    bytes: usize,
    pub(super) hits: u64,
}

impl ParentReads {
    pub(super) fn len(&self) -> usize {
        self.records.len()
    }

    pub(super) fn remember(&mut self, point: &OutPoint, row: &Option<LiveCell>) {
        let bytes = row.as_ref().map_or(0, LiveCache::record_bytes);
        if self.records.len() == MAX_RECORDS
            || bytes > MAX_RECORD_BYTES - self.bytes
            || self.records.iter().any(|(key, _)| key == point)
        {
            return;
        }
        self.bytes += bytes;
        self.records.push((point.clone(), row.clone()));
    }

    pub(super) fn replay(&mut self, point: &OutPoint) -> Option<Option<LiveCell>> {
        let (_, row) = self.records.iter().find(|(key, _)| key == point)?;
        self.hits += 1;
        Some(row.clone())
    }
}
