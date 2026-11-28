//! Write a canonical Live Cell record to the native database batch.

use anyhow::Result;
use ckb_db::internal::WriteBatch;
use ckb_types::packed::OutPoint;
use stem_ledger::{LiveCell, record};

#[cfg(feature = "native-vectored-cell-writes")]
use ckb_db::internal::{Handle, ffi};

#[cfg(feature = "native-vectored-cell-writes")]
const MIN_VECTORED_CELL_BYTES: usize = 4 * 1024;

pub(super) fn put_record(
    batch: &mut WriteBatch,
    key: &[u8; 34],
    point: &OutPoint,
    cell: &LiveCell,
    scratch: &mut Vec<u8>,
) -> Result<()> {
    #[cfg(feature = "native-vectored-cell-writes")]
    if cell.cell.as_slice().len() >= MIN_VECTORED_CELL_BYTES {
        record::with_cell_parts(point, cell, |value_parts| {
            let keys = [key.as_ptr().cast()];
            let key_sizes = [key.len()];
            let values = value_parts.map(|part| part.as_ptr().cast());
            let value_sizes = value_parts.map(|part| part.len());
            // SAFETY: The batch is uniquely borrowed. Each pointer and length
            // describes a live slice, including the encoded origin. RocksDB
            // synchronously appends the concatenated parts to its own buffer.
            unsafe {
                ffi::rocksdb_writebatch_putv(
                    batch.handle(),
                    1,
                    keys.as_ptr(),
                    key_sizes.as_ptr(),
                    3,
                    values.as_ptr(),
                    value_sizes.as_ptr(),
                );
            }
        });
        return Ok(());
    }

    record::encode_cell_into(point, cell, scratch);
    batch.put(key, scratch.as_slice())?;
    Ok(())
}

#[cfg(all(test, feature = "native-vectored-cell-writes"))]
mod tests {
    use bytes::Bytes;
    use ckb_db::internal::{DB, Options, WriteOptions, prelude::*};
    use ckb_types::{packed::Script, prelude::*};
    use stem_ledger::{Cell, MAX_CELL_BYTES, Origin};

    use super::*;

    fn batch_bytes(batch: &WriteBatch) -> &[u8] {
        let mut length = 0;
        // SAFETY: RocksDB exposes the batch buffer until its next mutation.
        // The returned slice keeps the batch immutably borrowed.
        unsafe {
            let data = ffi::rocksdb_writebatch_data(batch.handle(), &mut length);
            std::slice::from_raw_parts(data.cast(), length)
        }
    }

    #[test]
    fn vectored_cell_records_match_contiguous_batches_and_durable_rows() -> Result<()> {
        let lock = Script::default();
        let capacity = 1_000_000_000_000_000;
        let overhead = Cell::new(capacity, lock.clone(), vec![], Bytes::new())?
            .as_slice()
            .len();
        let mut control = WriteBatch::default();
        let mut candidate = WriteBatch::default();
        let mut scratch = Vec::new();
        let mut expected = Vec::new();
        for (index, size) in [
            overhead,
            MIN_VECTORED_CELL_BYTES - 1,
            MIN_VECTORED_CELL_BYTES,
            MIN_VECTORED_CELL_BYTES + 1,
            MAX_CELL_BYTES,
        ]
        .into_iter()
        .enumerate()
        {
            let point = OutPoint::new([4; 32].pack(), index as u32);
            let cell = LiveCell {
                cell: Cell::new(
                    capacity,
                    lock.clone(),
                    vec![],
                    Bytes::from(vec![index as u8; size - overhead]),
                )?,
                origin: Origin {
                    height: 11,
                    timestamp_ms: 19,
                    context_id: [23; 32].pack(),
                },
            };
            assert_eq!(cell.cell.as_slice().len(), size);
            let key = [index as u8; 34];
            let encoded = record::encode_cell(&point, &cell);
            control.put(key, &encoded)?;
            put_record(&mut candidate, &key, &point, &cell, &mut scratch)?;
            expected.push((key, encoded));
        }
        assert_eq!(candidate.len(), expected.len());
        assert_eq!(batch_bytes(&candidate), batch_bytes(&control));
        assert!(scratch.len() < MIN_VECTORED_CELL_BYTES + 128);

        let home = tempfile::tempdir()?;
        let mut options = Options::default();
        options.create_if_missing(true);
        let db = DB::open(&options, home.path())?;
        let mut sync = WriteOptions::default();
        sync.set_sync(true);
        db.write_opt(&candidate, &sync)?;
        drop(db);
        let reopened = DB::open(&options, home.path())?;
        for (key, expected) in expected {
            assert_eq!(reopened.get(key)?.as_deref(), Some(expected.as_slice()));
        }
        Ok(())
    }
}
