use anyhow::{Context, Result, ensure};
use ckb_db::internal::{ColumnFamily, DB, ReadOptions, WriteBatch, prelude::*};
use ckb_jsonrpc_types::{
    IndexerCell, IndexerCellsCapacity, IndexerOrder, IndexerPagination, IndexerRange,
    IndexerScriptType, IndexerSearchKey, IndexerSearchKeyFilter, IndexerSearchMode, IndexerTip,
    JsonBytes,
};
use ckb_types::{
    core::{HeaderView, cell::CellMeta},
    packed::{Byte32, OutPoint, Script},
    prelude::*,
};

pub const FAMILY: &str = "sidechain_rpc";
const INDEXED_TIP: &[u8] = b"tip";
const LOCK: u8 = b'l';
const TYPE: u8 = b't';
const NUMBER: u8 = b'n';
const POSITION: usize = 16;
const SCRIPT_FIELDS: usize = 33;
pub const MAX_PAGE: usize = 1000;
const MAX_VISITED: usize = 16_384;
const MAX_VISITED_DATA: u64 = 64 * 1024 * 1024;
const PAGE_BYTES: usize = 1024 * 1024;

pub struct Index<'a> {
    db: &'a DB,
    family: &'a ColumnFamily,
}

impl<'a> Index<'a> {
    pub fn new(db: &'a DB) -> Self {
        Self {
            db,
            family: db.cf_handle(FAMILY).expect("RPC index column family"),
        }
    }

    pub fn indexed_at(&self, header: &HeaderView) -> Result<bool> {
        Ok(self
            .db
            .get_cf(self.family, INDEXED_TIP)?
            .is_some_and(|hash| hash.as_ref() == header.hash().as_slice()))
    }

    pub fn clear(&self, batch: &mut WriteBatch) -> Result<()> {
        batch.delete_cf(self.family, INDEXED_TIP)?;
        batch.delete_range_cf(self.family, b"l", b"u")?;
        Ok(())
    }

    pub fn record_tip(&self, batch: &mut WriteBatch, header: &HeaderView) -> Result<()> {
        batch.put_cf(self.family, INDEXED_TIP, header.hash().as_slice())?;
        Ok(())
    }

    pub fn record_header(&self, batch: &mut WriteBatch, header: &HeaderView) -> Result<()> {
        let mut key = [NUMBER; 9];
        key[1..].copy_from_slice(&header.number().to_be_bytes());
        batch.put_cf(self.family, key, header.hash().as_slice())?;
        Ok(())
    }

    pub fn header_hash(&self, number: u64) -> Result<Option<Byte32>> {
        let mut key = [NUMBER; 9];
        key[1..].copy_from_slice(&number.to_be_bytes());
        self.db
            .get_cf(self.family, key)?
            .map(|bytes| Ok(Byte32::from_slice(&bytes)?))
            .transpose()
    }

    pub fn change_cell(&self, batch: &mut WriteBatch, cell: &CellMeta, add: bool) -> Result<()> {
        let info = cell
            .transaction_info
            .as_ref()
            .context("missing indexed Cell origin")?;
        let index = u32::try_from(
            info.index
                .checked_sub(1)
                .context("invalid Cell transaction index")?,
        )?;
        for (kind, script) in [
            (LOCK, Some(cell.cell_output.lock())),
            (TYPE, cell.cell_output.type_().to_opt()),
        ] {
            let Some(script) = script else { continue };
            let mut key = script_key(kind, &script);
            key.extend(info.block_number.to_be_bytes());
            key.extend(index.to_be_bytes());
            key.extend(u32::from(cell.out_point.index()).to_be_bytes());
            if add {
                batch.put_cf(self.family, &key, cell.out_point.as_slice())?;
            } else {
                batch.delete_cf(self.family, &key)?;
            }
        }
        Ok(())
    }

    pub fn cells(
        &self,
        search: IndexerSearchKey,
        order: IndexerOrder,
        limit: usize,
        after: Option<JsonBytes>,
        resolve: impl FnMut(&OutPoint) -> Result<CellMeta>,
    ) -> Result<IndexerPagination<IndexerCell>> {
        ensure!(
            (1..=MAX_PAGE).contains(&limit),
            "cell page limit must be between 1 and {MAX_PAGE}"
        );
        let mut items = Vec::with_capacity(limit);
        let mut cursor = Vec::new();
        let mut bytes = 0;
        let with_data = search.with_data.unwrap_or(true);
        self.visit(search, order, after, resolve, |key, cell, number, index| {
            // JSON hex encodes each data and script byte as two characters.
            let size = 512
                + cell.cell_output.as_slice().len() * 2
                + if with_data {
                    cell.data_bytes as usize * 2
                } else {
                    0
                };
            if !items.is_empty() && bytes + size > PAGE_BYTES {
                return Ok(false);
            }
            bytes += size;
            cursor = key.to_vec();
            items.push(IndexerCell {
                output: cell.cell_output.into(),
                output_data: with_data
                    .then(|| JsonBytes::from_bytes(cell.mem_cell_data.expect("indexed Cell data"))),
                out_point: cell.out_point.into(),
                block_number: number.into(),
                tx_index: index.into(),
            });
            Ok(items.len() < limit)
        })?;
        Ok(IndexerPagination::new(items, JsonBytes::from_vec(cursor)))
    }

    pub fn capacity(
        &self,
        search: IndexerSearchKey,
        header: &HeaderView,
        resolve: impl FnMut(&OutPoint) -> Result<CellMeta>,
    ) -> Result<IndexerCellsCapacity> {
        let mut capacity = 0u64;
        self.visit(search, IndexerOrder::Asc, None, resolve, |_, cell, _, _| {
            capacity = capacity
                .checked_add(cell.cell_output.capacity().into())
                .context("indexed capacity overflow")?;
            Ok(true)
        })?;
        Ok(IndexerCellsCapacity {
            capacity: capacity.into(),
            block_hash: header.hash().into(),
            block_number: header.number().into(),
        })
    }

    fn visit(
        &self,
        search: IndexerSearchKey,
        order: IndexerOrder,
        after: Option<JsonBytes>,
        mut resolve: impl FnMut(&OutPoint) -> Result<CellMeta>,
        mut emit: impl FnMut(&[u8], CellMeta, u64, u32) -> Result<bool>,
    ) -> Result<()> {
        let mode = search.script_search_mode.unwrap_or_default();
        ensure!(
            mode != IndexerSearchMode::Partial,
            "partial script searches require a rich indexer"
        );
        ensure!(
            !search.group_by_transaction.unwrap_or(false),
            "get_cells returns individual Cells"
        );
        let kind = match search.script_type {
            IndexerScriptType::Lock => LOCK,
            IndexerScriptType::Type => TYPE,
        };
        let prefix = script_key(kind, &search.script.into());
        let filter = Filter::new(search.filter.unwrap_or_default());
        let is_match = |key: &[u8]| {
            key.len() >= prefix.len() + POSITION
                && key.starts_with(&prefix)
                && (mode != IndexerSearchMode::Exact || key.len() == prefix.len() + POSITION)
        };
        let after = after
            .as_ref()
            .map(JsonBytes::as_bytes)
            .filter(|bytes| !bytes.is_empty());
        if let Some(after) = after {
            ensure!(
                is_match(after),
                "cursor is outside the requested script range"
            );
        }
        let descending = matches!(order, IndexerOrder::Desc);
        let read = ReadOptions::default();
        let mut iterator = self.db.get_raw_iter_cf(self.family, &read)?;
        if descending {
            iterator.seek_for_prev(after.unwrap_or(&upper_bound(&prefix)));
        } else {
            iterator.seek(after.unwrap_or(&prefix));
        }
        let mut visited = 0;
        let mut visited_data = 0;
        while let Some(key) = iterator.key().filter(|key| key.starts_with(&prefix)) {
            ensure!(visited < MAX_VISITED, "cell search exceeds its scan budget");
            visited += 1;
            if after != Some(key) && is_match(key) {
                let position = &key[key.len() - POSITION..];
                let number = u64::from_be_bytes(position[..8].try_into()?);
                if within(&filter.options.block_range, number) {
                    let point = OutPoint::from_slice(
                        iterator.value().context("missing indexed OutPoint")?,
                    )?;
                    let cell = resolve(&point)?;
                    visited_data += cell.data_bytes;
                    ensure!(
                        visited_data <= MAX_VISITED_DATA,
                        "cell search exceeds its data budget"
                    );
                    if filter.matches(&cell, kind) {
                        let index = u32::from_be_bytes(position[8..12].try_into()?);
                        if !emit(key, cell, number, index)? {
                            return Ok(());
                        }
                    }
                }
            }
            if descending {
                iterator.prev();
            } else {
                iterator.next();
            }
        }
        Ok(())
    }
}

pub fn tip(header: &HeaderView) -> IndexerTip {
    IndexerTip {
        block_hash: header.hash().into(),
        block_number: header.number().into(),
    }
}

struct Filter {
    options: IndexerSearchKeyFilter,
    script: Option<Vec<u8>>,
}

impl Filter {
    fn new(options: IndexerSearchKeyFilter) -> Self {
        let script = options
            .script
            .clone()
            .map(|script| script_key(0, &script.into()));
        Self { options, script }
    }

    fn matches(&self, cell: &CellMeta, kind: u8) -> bool {
        let other = if kind == LOCK {
            cell.cell_output.type_().to_opt()
        } else {
            Some(cell.cell_output.lock())
        };
        let data = cell.mem_cell_data.as_deref().expect("indexed Cell data");
        self.script.as_ref().is_none_or(|prefix| {
            other
                .as_ref()
                .is_some_and(|script| script_key(0, script).starts_with(prefix))
        }) && within(
            &self.options.script_len_range,
            other.map_or(0, |script| {
                (SCRIPT_FIELDS + script.args().raw_data().len()) as u64
            }),
        ) && within(&self.options.output_data_len_range, cell.data_bytes)
            && within(
                &self.options.output_capacity_range,
                cell.cell_output.capacity().into(),
            )
            && self.options.output_data.as_ref().is_none_or(|pattern| {
                let pattern = pattern.as_bytes();
                match self.options.output_data_filter_mode.unwrap_or_default() {
                    IndexerSearchMode::Prefix => data.starts_with(pattern),
                    IndexerSearchMode::Exact => data == pattern,
                    IndexerSearchMode::Partial => memchr::memmem::find(data, pattern).is_some(),
                }
            })
    }
}

fn within(range: &Option<IndexerRange>, value: u64) -> bool {
    range
        .as_ref()
        .is_none_or(|range| (range.start().value()..range.end().value()).contains(&value))
}

fn script_key(kind: u8, script: &Script) -> Vec<u8> {
    let args = script.args().raw_data();
    let mut key = Vec::with_capacity(1 + SCRIPT_FIELDS + args.len() + POSITION);
    key.push(kind);
    key.extend_from_slice(script.code_hash().as_slice());
    key.push(script.hash_type().into());
    key.extend_from_slice(&args);
    key
}

fn upper_bound(prefix: &[u8]) -> Vec<u8> {
    let mut bound = prefix.to_vec();
    while bound.last() == Some(&u8::MAX) {
        bound.pop();
    }
    // The key starts with an ASCII namespace, so there is always a successor.
    *bound.last_mut().expect("script namespace") += 1;
    bound
}
