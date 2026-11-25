//! Read-only wallet discovery over a committed RocksDB view.

use std::path::Path;

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{Options, ReadOnlyDB, prelude::*};
use ckb_types::packed::{Byte32, OutPoint};
use fern_ledger::{
    LiveCell,
    block::Header,
    patricia::{Key, Namespace},
    record,
};

use super::{STATE_RECORD, TIP, state_key};

pub enum ScriptQuery {
    Lock(Byte32),
    Type(Byte32),
}

pub struct CellPage {
    pub header: Header,
    pub cells: Vec<(OutPoint, LiveCell)>,
    pub next: Option<Key>,
}

pub fn find_cells(
    database: &Path,
    query: &ScriptQuery,
    after: Option<Key>,
    limit: usize,
    height: Option<u64>,
) -> Result<CellPage> {
    ensure!(
        (1..=256).contains(&limit),
        "Cell page limit must be between 1 and 256"
    );
    ensure!(
        after.is_none() || height.is_some(),
        "Cell pagination requires the prior page height"
    );
    let db = ReadOnlyDB::open(&Options::default(), database)?;
    let tip = db.get(TIP)?.context("missing Fern database tip")?;
    let tip = Header::decode(&tip)?;
    ensure!(
        height.is_none_or(|height| height == tip.context.height),
        "the Fern view advanced during Cell pagination"
    );

    let mut iterator = db.get_raw_iter(&Default::default());
    let start = state_key(STATE_RECORD, Namespace::Cells, &after.unwrap_or_default());
    iterator.seek(&start);
    if after.is_some() && iterator.key() == Some(start.as_slice()) {
        iterator.next();
    }
    let mut cells = Vec::new();
    let mut last = None;
    let mut scanned = 0;
    let mut returned = 0;
    while let Some(key) = iterator.key().filter(|key| key.starts_with(&start[..2])) {
        let raw = iterator
            .value()
            .context("missing Live Cell at the current iterator position")?;
        let (point, record) = record::decode_cell(raw)?;
        let matches = match query {
            ScriptQuery::Lock(hash) => record.cell.lock().calc_script_hash() == *hash,
            ScriptQuery::Type(hash) => record
                .cell
                .type_scripts()
                .any(|script| script.calc_script_hash() == *hash),
        };
        if matches {
            if returned + raw.len() > 4 * 1024 * 1024 {
                break;
            }
            returned += raw.len();
            cells.push((point, record));
        }
        last = Some(key[2..].try_into()?);
        scanned += 1;
        iterator.next();
        if cells.len() == limit || scanned == 4096 {
            break;
        }
    }
    let next = last.filter(|_| {
        iterator
            .key()
            .is_some_and(|key| key.starts_with(&start[..2]))
    });
    Ok(CellPage {
        header: tip,
        cells,
        next,
    })
}
