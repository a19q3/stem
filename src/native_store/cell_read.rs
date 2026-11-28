//! Keep the owned allocation returned by a RocksDB point read.

use anyhow::Result;
use bytes::Bytes;
use ckb_db::internal::DBVector;
use ckb_types::packed::OutPoint;
use stem_ledger::{LiveCell, record};

struct OwnedPointRead(DBVector);

impl AsRef<[u8]> for OwnedPointRead {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

// SAFETY: this private wrapper accepts only owned RocksDB get results.
// rocksdb_get allocates a copy with malloc; DBVector holds its pointer
// and length, with no database, snapshot, or owning-thread reference.
// DBVector's destructor calls rocksdb_free (free). It can release that
// allocation on a different thread. The wrapper exposes immutable bytes.
unsafe impl Send for OwnedPointRead {}

pub(super) fn decode_owned(bytes: DBVector) -> Result<(OutPoint, LiveCell)> {
    record::decode_cell_bytes(Bytes::from_owner(OwnedPointRead(bytes)))
}

#[cfg(test)]
mod tests {
    use ckb_db::internal::{DB, Options, prelude::*};
    use ckb_types::{packed::Script, prelude::*};
    use stem_ledger::{Cell, Origin};

    use super::*;

    #[test]
    fn owned_row_keeps_the_cell_allocation_after_closing_the_database() -> Result<()> {
        let home = tempfile::tempdir()?;
        let mut options = Options::default();
        options.create_if_missing(true);
        let db = DB::open(&options, home.path())?;
        let point = OutPoint::new([7; 32].pack(), 3);
        let expected = LiveCell {
            cell: Cell::new(
                20_000_000_000,
                Script::default(),
                vec![],
                vec![5; 16_384].into(),
            )?,
            origin: Origin {
                height: 7,
                timestamp_ms: 2_048,
                context_id: [9; 32].pack(),
            },
        };
        let canonical = record::encode_cell(&point, &expected);
        db.put(b"record", &canonical)?;
        let raw = db.get(b"record")?.expect("stored record");
        let (_, _, body) = record::decode_cell_prefix(&raw)?;
        let address = body.as_ptr() as usize;
        let (actual_point, actual) = decode_owned(raw)?;
        drop(db);
        std::thread::spawn(move || {
            assert_eq!(actual.cell.as_slice().as_ptr() as usize, address);
            assert_eq!(actual_point, point);
            assert_eq!(actual.origin, expected.origin);
            assert_eq!(record::encode_cell(&actual_point, &actual), canonical);
            assert_eq!(actual.cell.content_hash(), expected.cell.content_hash());
        })
        .join()
        .expect("owned record uses a thread-independent allocation");
        Ok(())
    }
}
