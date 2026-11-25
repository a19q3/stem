use std::{
    io::{Read, Write},
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{
    BlockBasedOptions, Cache, DB, DBVector, FlushOptions, Options, ReadOptions, WriteBatch,
    WriteOptions, prelude::*,
};
use ckb_hash::new_blake2b;
use ckb_types::{
    bytes::Bytes,
    core::{
        HeaderView, TransactionInfo, TransactionView,
        cell::{CellMeta, CellMetaBuilder, CellProvider, CellStatus},
    },
    packed::{Byte32, CellOutput, Header, OutPoint},
    prelude::*,
};
use lru::LruCache;
use serde::Serialize;

use crate::{
    bridge::{self, Bridge},
    genesis::{Genesis, SYSTEM_LOCK},
    governance, index,
};

const FORMAT: &[u8] = b"ckb-sidechain/v1";
const STORAGE_FORMAT: &[u8] = b"ckb-sidechain/v2";
const GENESIS: &[u8] = b"genesis";
const TIP: &[u8] = b"tip";
const APP_HASH: &[u8] = b"app_hash";
const WITHDRAWALS: &[u8] = b"withdrawals";
const GOVERNANCE: &[u8] = b"governance";
const GUARDIAN_GOVERNANCE: &[u8] = b"guardian_governance";
const BACKING_ANCHOR: &[u8] = b"guardian_l1_anchor";
const SNAPSHOT_END: &[u8] = b"z";
const ORDINARY_HISTORY: u8 = b'x';
const ORDINARY_HISTORY_END: [u8; 1] = [ORDINARY_HISTORY + 1];
const RECENT_STATUS: u8 = b'q';
const RECENT_BY_HEIGHT: u8 = b'r';
const RECENT_TRANSACTION_BLOCKS: u64 = 128;
const BLOCK_CACHE_BYTES: usize = 64 * 1024 * 1024;

pub struct Store {
    db: DB,
    pub tip: HeaderView,
    pub genesis_hash: Byte32,
    pub app_hash: Byte32,
    pub bridge: Option<Bridge>,
    pub next_withdrawal: u64,
    pub governance: Option<OutPoint>,
    pub guardian_governance: Option<OutPoint>,
    pub guardians: Option<bridge::Guardians>,
    pub rpc_index_enabled: bool,
    pub checkpoint_format: u8,
    pub compact_transaction_history: bool,
    shared_cells: Mutex<LruCache<OutPoint, CellMeta>>,
}

#[derive(Default, Serialize)]
pub struct StorageStats {
    live_cells: Records,
    transaction_archive: Records,
    transaction_replay_index: Records,
    recent_transaction_status: Records,
    recent_transaction_expiry: Records,
    headers: Records,
    header_origin_index: Records,
    rpc_index: Records,
    withdrawal_index: Records,
    metadata: Records,
}

#[derive(Default, Serialize)]
struct Records {
    count: u64,
    logical_bytes: u64,
}

impl Store {
    pub fn open(path: &Path, spec: &Genesis) -> Result<Self> {
        let (genesis, allocations) = spec.validate()?;
        let genesis_hash = genesis.hash();
        let db = open_db(path)?;
        let (tip, app_hash) = if let Some(marker) = db.get(GENESIS)? {
            let tip = Header::from_slice(&db.get(TIP)?.context("missing tip")?)?.into_view();
            if marker.as_ref() == [FORMAT, genesis_hash.as_slice()].concat() {
                migrate_header_origins(&db, &genesis_hash, tip.number())?;
            } else {
                ensure!(
                    marker.as_ref() == [STORAGE_FORMAT, genesis_hash.as_slice()].concat(),
                    "database genesis or format differs from the chain specification"
                );
            }
            let hash = Byte32::from_slice(
                &db.get(APP_HASH)?
                    .ok_or_else(|| anyhow::anyhow!("missing app hash"))?,
            )?;
            (tip, hash)
        } else {
            let mut batch = WriteBatch::default();
            batch.put(GENESIS, [STORAGE_FORMAT, genesis_hash.as_slice()].concat())?;
            batch.put(APP_HASH, genesis_hash.as_slice())?;
            if spec.checkpoint_interval > 0 {
                batch.put(SNAPSHOT_END, FORMAT)?;
            }
            if let Some(point) = spec.governance.clone().map(OutPoint::from) {
                batch.put(GOVERNANCE, point.as_slice())?;
            }
            if let Some(point) = spec.guardian_governance.clone().map(OutPoint::from) {
                batch.put(GUARDIAN_GOVERNANCE, point.as_slice())?;
            }
            record_header(&mut batch, &genesis)?;
            for (index, tx) in allocations.iter().enumerate() {
                record_transaction(&mut batch, b't', tx, &genesis)?;
                for cell in outputs(tx, &genesis, index) {
                    record_cell(&mut batch, &cell, &genesis)?;
                }
            }
            write_sync(&db, &batch)?;
            (genesis, genesis_hash.clone())
        };
        let next_withdrawal = db
            .get(WITHDRAWALS)?
            .map(|bytes| Ok::<_, anyhow::Error>(u64::from_le_bytes(bytes.as_ref().try_into()?)))
            .transpose()?
            .unwrap_or(0);
        let governance = db
            .get(GOVERNANCE)?
            .map(|raw| OutPoint::from_slice(&raw))
            .transpose()?;
        ensure!(
            governance.is_some() == spec.governance.is_some(),
            "inconsistent governance record"
        );
        let guardian_governance = db
            .get(GUARDIAN_GOVERNANCE)?
            .map(|raw| OutPoint::from_slice(&raw))
            .transpose()?;
        let backing_anchor = db.get(BACKING_ANCHOR)?;
        ensure!(
            guardian_governance.is_some() == spec.guardian_governance.is_some()
                && (backing_anchor.is_none() || guardian_governance.is_some()),
            "inconsistent bridge guardian records"
        );
        let mut store = Self {
            db,
            tip,
            genesis_hash,
            app_hash,
            bridge: spec.bridge.clone(),
            next_withdrawal,
            governance,
            guardian_governance,
            guardians: spec.bridge.as_ref().map(bridge::Guardians::fixed),
            rpc_index_enabled: false,
            checkpoint_format: spec.checkpoint_format(),
            compact_transaction_history: false,
            shared_cells: Mutex::new(LruCache::new(32)),
        };
        if let Some(point) = &store.guardian_governance {
            let CellStatus::Live(cell) = store.cell(point, true) else {
                anyhow::bail!("missing live guardian governance Cell");
            };
            governance::validate_cell(&cell.cell_output)?;
            let members = governance::decode_guardians(
                &cell
                    .mem_cell_data
                    .context("missing guardian governance data")?,
            )?;
            store.guardians = Some(bridge::Guardians {
                members,
                generation: Some(point.clone().into()),
                backing_anchor: backing_anchor
                    .map(|record| decode_backing_anchor(&record))
                    .transpose()?,
            });
        }
        Ok(store)
    }

    pub fn header(&self, hash: &Byte32) -> Option<HeaderView> {
        self.read(&key(b'h', hash.as_slice()))
            .map(|data| checked(Header::from_slice(&data)).into_view())
    }

    pub fn rpc_index(&self) -> Result<index::Index<'_>> {
        ensure!(
            self.rpc_index_enabled,
            "CKB RPC index is disabled on this replica"
        );
        Ok(index::Index::new(&self.db))
    }

    pub fn enable_rpc_index(&mut self) -> Result<()> {
        if self.db.cf_handle(index::FAMILY).is_none() {
            self.db.create_cf(index::FAMILY, &db_options())?;
        }
        let index = index::Index::new(&self.db);
        if !index.indexed_at(&self.tip)? {
            let mut batch = WriteBatch::default();
            index.clear(&mut batch)?;
            write_sync(&self.db, &batch)?;
            batch.clear()?;
            let mut read = ReadOptions::default();
            read.fill_cache(false);
            let mut iterator = self.db.get_raw_iter(&read);
            let mut pending = 0;
            for namespace in [b'h', b'c'] {
                iterator.seek([namespace]);
                while let Some(key) = iterator.key().filter(|key| key[0] == namespace) {
                    if namespace == b'h' {
                        let bytes = iterator.value().context("indexed header has no data")?;
                        index.record_header(&mut batch, &Header::from_slice(bytes)?.into_view())?;
                    } else {
                        let point = OutPoint::from_slice(&key[1..])?;
                        index.change_cell(&mut batch, &self.indexed_cell(&point)?, true)?;
                    }
                    pending += 1;
                    if pending == 4096 || batch.size_in_bytes() > 4 * 1024 * 1024 {
                        // Persist the derived data before writing its completion marker.
                        write_sync(&self.db, &batch)?;
                        batch.clear()?;
                        pending = 0;
                    }
                    iterator.next();
                }
                ensure!(
                    iterator.key().is_some(),
                    "incomplete canonical ledger during RPC index rebuild"
                );
            }
            index.record_tip(&mut batch, &self.tip)?;
            write_sync(&self.db, &batch)?;
        }
        self.rpc_index_enabled = true;
        Ok(())
    }

    pub fn indexed_cell(&self, point: &OutPoint) -> Result<CellMeta> {
        match self.cell(point, true) {
            CellStatus::Live(cell) => Ok(cell),
            _ => anyhow::bail!("Cell index references a missing live Cell"),
        }
    }

    pub fn transaction(&self, hash: &Byte32) -> Option<DBVector> {
        self.transaction_record(hash)
            .filter(|record| record.len() > 32)
    }

    fn transaction_record(&self, hash: &Byte32) -> Option<DBVector> {
        let record = self.read(&key(b't', hash.as_slice()));
        if (self.checkpoint_format == 3 && record.as_ref().is_some_and(|row| row.len() == 32))
            || (self.checkpoint_format == 4 && record.is_none())
        {
            return self
                .read(&key(ORDINARY_HISTORY, hash.as_slice()))
                .or(record);
        }
        record
    }

    pub fn transaction_or_origin(&self, hash: &Byte32) -> Option<Vec<u8>> {
        if let Some(record) = self.transaction_record(hash) {
            return Some(record.to_vec());
        }
        if self.checkpoint_format != 4 {
            return None;
        }
        if self.compact_transaction_history
            && let Some(record) = self.read(&key(RECENT_STATUS, hash.as_slice()))
        {
            return Some(record.to_vec());
        }
        let prefix = key(b'c', hash.as_slice());
        let mut cells = self.db.get_raw_iter(&ReadOptions::default());
        cells.seek(&prefix);
        if cells
            .key()
            .is_some_and(|record| record.starts_with(&prefix))
        {
            let cell = cells
                .value()
                .unwrap_or_else(|| fatal("missing stored cell value"));
            return Some(
                cell.get(..32)
                    .unwrap_or_else(|| fatal("truncated stored cell origin"))
                    .to_vec(),
            );
        }
        None
    }

    pub fn prune_ordinary_history(&mut self) -> Result<()> {
        ensure!(
            self.checkpoint_format >= 3,
            "transaction history pruning requires compact checkpoints"
        );
        let start = [ORDINARY_HISTORY];
        let has_history = {
            let mut records = self.db.get_raw_iter(&ReadOptions::default());
            records.seek(start);
            records.key().is_some_and(|key| key.starts_with(&start))
        };
        let mut batch = WriteBatch::default();
        if has_history {
            batch.delete_range(start, ORDINARY_HISTORY_END)?;
        }
        if self.checkpoint_format == 4 {
            let cutoff = self.tip.number().saturating_sub(RECENT_TRANSACTION_BLOCKS);
            let mut buckets = self.db.get_raw_iter(&ReadOptions::default());
            buckets.seek([RECENT_BY_HEIGHT]);
            while let Some(key) = buckets
                .key()
                .filter(|key| key.first() == Some(&RECENT_BY_HEIGHT))
            {
                let height = u64::from_be_bytes(key[1..].try_into()?);
                if height > cutoff {
                    break;
                }
                let hashes = buckets
                    .value()
                    .context("missing recent transaction bucket")?;
                erase_recent_bucket(&mut batch, key, hashes)?;
                buckets.next();
            }
        }
        if !batch.is_empty() {
            write_sync(&self.db, &batch)?;
        }
        self.compact_transaction_history = true;
        Ok(())
    }

    // Format 4 uses live inputs to protect ordinary transactions from replay.
    pub fn has_canonical_transaction(&self, hash: &Byte32) -> bool {
        checked(self.db.get_pinned(key(b't', hash.as_slice()))).is_some()
    }

    pub fn withdrawal(&self, nonce: u64) -> Option<OutPoint> {
        self.read(&key(b'w', &nonce.to_be_bytes()))
            .map(|record| checked(OutPoint::from_slice(&record)))
    }

    pub fn committee_updates(
        &self,
        txs: &[TransactionView],
    ) -> Result<Vec<tendermint_proto::v0_38::abci::ValidatorUpdate>> {
        let Some(point) = &self.governance else {
            return Ok(Vec::new());
        };
        let Some(tx) = txs
            .iter()
            .find(|tx| !bridge::is_mint(tx) && tx.input_pts_iter().any(|input| &input == point))
        else {
            return Ok(Vec::new());
        };
        let CellStatus::Live(previous) = self.cell(point, true) else {
            fatal("missing live governance Cell");
        };
        let old = governance::decode(
            previous
                .mem_cell_data
                .as_ref()
                .expect("governance cell data"),
        )?;
        let new = governance::decode(&governance::successor_data(tx)?)?;
        Ok(governance::updates(&old, &new))
    }

    fn read(&self, key: &[u8]) -> Option<DBVector> {
        // Operational database faults must never become consensus validation errors.
        checked(self.db.get(key))
    }

    pub fn commit(
        &mut self,
        header: HeaderView,
        txs: &[TransactionView],
        cells: impl IntoIterator<Item = (OutPoint, Option<CellMeta>)>,
    ) -> Result<Duration> {
        let app_hash = self.next_app_hash(&header, !txs.is_empty());
        self.commit_with_hash(header, txs, cells, app_hash)
    }

    pub fn commit_with_hash(
        &mut self,
        header: HeaderView,
        txs: &[TransactionView],
        cells: impl IntoIterator<Item = (OutPoint, Option<CellMeta>)>,
        app_hash: Byte32,
    ) -> Result<Duration> {
        let mut batch = WriteBatch::default();
        record_header(&mut batch, &header)?;
        batch.put(APP_HASH, app_hash.as_slice())?;
        let index = self.rpc_index_enabled.then(|| index::Index::new(&self.db));
        let mut next_withdrawal = self.next_withdrawal;
        let mut governance = self.governance.clone();
        let mut guardian_update = None;
        let mut backing_anchor = self
            .guardians
            .as_ref()
            .and_then(|guardians| guardians.backing_anchor.clone());
        let mut backing_changed = false;
        let mut recent = Vec::new();
        for tx in txs {
            let governance_change = governance::successor_point(tx, governance.as_ref());
            let guardian_change =
                governance::successor_point(tx, self.guardian_governance.as_ref());
            let mut burns = self
                .bridge
                .as_ref()
                .into_iter()
                .flat_map(|_| bridge::withdrawal_outputs(tx))
                .peekable();
            if self.checkpoint_format >= 3
                && governance_change.is_none()
                && guardian_change.is_none()
                && !bridge::is_mint(tx)
                && burns.peek().is_none()
            {
                let hash = tx.hash();
                if self.checkpoint_format == 3 {
                    batch.put(key(b't', hash.as_slice()), header.hash().as_slice())?;
                } else if self.compact_transaction_history {
                    batch.put(
                        key(RECENT_STATUS, hash.as_slice()),
                        header.hash().as_slice(),
                    )?;
                    recent.extend_from_slice(hash.as_slice());
                }
                if !self.compact_transaction_history {
                    record_transaction(&mut batch, ORDINARY_HISTORY, tx, &header)?;
                }
            } else {
                record_transaction(&mut batch, b't', tx, &header)?;
            }
            if let Some(next) = governance_change {
                batch.put(GOVERNANCE, next.as_slice())?;
                governance = Some(next);
            }
            if let Some(next) = guardian_change {
                let members = governance::decode_guardians(&governance::successor_data(tx)?)?;
                batch.put(GUARDIAN_GOVERNANCE, next.as_slice())?;
                guardian_update = Some((next, members));
            }
            if self.guardian_governance.is_some() && bridge::is_mint(tx) {
                let accepted = bridge::Certificate::from_mint(tx)?.anchor();
                if backing_anchor
                    .as_ref()
                    .is_none_or(|last| accepted.height > last.height)
                {
                    backing_anchor = Some(accepted);
                    backing_changed = true;
                }
            }
            if self.bridge.is_some() {
                for index in burns {
                    let point = OutPoint::new(tx.hash(), index.try_into()?);
                    batch.put(key(b'w', &next_withdrawal.to_be_bytes()), point.as_slice())?;
                    next_withdrawal = next_withdrawal
                        .checked_add(1)
                        .ok_or_else(|| anyhow::anyhow!("withdrawal counter overflow"))?;
                }
            }
        }
        if next_withdrawal != self.next_withdrawal {
            batch.put(WITHDRAWALS, next_withdrawal.to_le_bytes())?;
        }
        if backing_changed {
            let anchor = backing_anchor.as_ref().expect("updated L1 backing anchor");
            batch.put(
                BACKING_ANCHOR,
                [&anchor.height.to_le_bytes(), anchor.hash.as_bytes()].concat(),
            )?;
        }
        if !recent.is_empty() {
            batch.put(
                key(RECENT_BY_HEIGHT, &header.number().to_be_bytes()),
                recent,
            )?;
        }
        if self.checkpoint_format == 4
            && self.compact_transaction_history
            && let Some(expired) = header.number().checked_sub(RECENT_TRANSACTION_BLOCKS)
        {
            let key = key(RECENT_BY_HEIGHT, &expired.to_be_bytes());
            if let Some(hashes) = self.read(&key) {
                erase_recent_bucket(&mut batch, &key, &hashes)?;
            }
        }
        for (point, cell) in cells {
            if let Some(index) = &index {
                match &cell {
                    Some(cell) => index.change_cell(&mut batch, cell, true)?,
                    None => {
                        // Outputs created and consumed in one block never entered the index.
                        if let CellStatus::Live(previous) = self.cell(&point, true) {
                            index.change_cell(&mut batch, &previous, false)?;
                        }
                    }
                }
            }
            self.shared_cells
                .get_mut()
                .expect("cell cache lock")
                .pop(&point);
            if let Some(cell) = cell {
                record_cell(&mut batch, &cell, &header)?;
            } else {
                batch.delete(key(b'c', point.as_slice()))?;
            }
        }
        if let Some(index) = index {
            index.record_header(&mut batch, &header)?;
            index.record_tip(&mut batch, &header)?;
        }
        let start = Instant::now();
        write_sync(&self.db, &batch)?;
        self.tip = header;
        self.app_hash = app_hash;
        self.next_withdrawal = next_withdrawal;
        self.governance = governance;
        if let Some((next, members)) = guardian_update {
            let guardians = self
                .guardians
                .as_mut()
                .expect("guardian governance requires a bridge");
            guardians.members = members;
            guardians.generation = Some(next.clone().into());
            self.guardian_governance = Some(next);
        }
        if backing_changed {
            self.guardians
                .as_mut()
                .expect("bridge backing anchor requires guardians")
                .backing_anchor = backing_anchor;
        }
        Ok(start.elapsed())
    }

    pub fn next_app_hash(&self, header: &HeaderView, writes: bool) -> Byte32 {
        if writes {
            header.hash()
        } else {
            // CometBFT makes a proof block when the app hash changes. Empty blocks
            // advance the clock and CKB header chain without changing live-cell state.
            self.app_hash.clone()
        }
    }

    #[cfg(test)]
    pub fn export(&self, writer: &mut impl Write) -> Result<(Byte32, u64)> {
        self.export_with_cells(writer, |_, _, _| Ok(()))
    }

    pub fn export_with_cells(
        &self,
        writer: &mut impl Write,
        mut on_cell: impl FnMut(u64, &[u8], &[u8]) -> Result<()>,
    ) -> Result<(Byte32, u64)> {
        let mut read = ReadOptions::default();
        read.fill_cache(false);
        let mut iterator = self.db.get_raw_iter(&read);
        iterator.seek_to_first();
        let mut digest = new_blake2b();
        let mut total = 0u64;
        let mut found_end = false;
        let canonical_genesis = [FORMAT, self.genesis_hash.as_slice()].concat();
        while let Some(key) = iterator.key() {
            ensure!(!found_end, "records after archive sentinel");
            if local_history_namespace(self.checkpoint_format, key[0]) {
                iterator.seek([key[0] + 1]);
                continue;
            }
            if matches!((key.first(), key.len()), (Some(b'o'), 33)) {
                // This physical index is rebuilt from the authenticated archived headers.
                iterator.next();
                continue;
            }
            let value = if key == GENESIS {
                canonical_genesis.as_slice()
            } else {
                iterator
                    .value()
                    .context("snapshot iterator lost its value")?
            };
            let key_len = u32::try_from(key.len())?.to_le_bytes();
            let value_len = u32::try_from(value.len())?.to_le_bytes();
            if matches!((key.first(), key.len()), (Some(b'c'), 37)) {
                on_cell(total, key, value)?;
            }
            for part in [key_len.as_slice(), value_len.as_slice(), key, value] {
                writer.write_all(part)?;
                digest.update(part);
                total = total
                    .checked_add(part.len() as u64)
                    .context("archive size overflow")?;
            }
            found_end = key == SNAPSHOT_END && value == FORMAT;
            iterator.next();
        }
        ensure!(found_end, "did not reach the database archive sentinel");
        let mut root = [0; 32];
        digest.finalize(&mut root);
        Ok((root.into(), total))
    }

    pub fn storage_stats(&self) -> Result<StorageStats> {
        let mut stats = StorageStats::default();
        let mut read = ReadOptions::default();
        read.fill_cache(false);
        let mut iterator = self.db.get_raw_iter(&read);
        iterator.seek_to_first();
        while let Some(key) = iterator.key() {
            let value = iterator
                .value()
                .context("database iterator lost its value")?;
            let records = match (key.first().copied(), key.len()) {
                (Some(b'c'), 37) => &mut stats.live_cells,
                (Some(b't'), 33) if value.len() == 32 && self.checkpoint_format == 3 => {
                    &mut stats.transaction_replay_index
                }
                (Some(b't' | ORDINARY_HISTORY), 33) => &mut stats.transaction_archive,
                (Some(RECENT_STATUS), 33) => &mut stats.recent_transaction_status,
                (Some(RECENT_BY_HEIGHT), 9) => &mut stats.recent_transaction_expiry,
                (Some(b'h'), 33) => &mut stats.headers,
                (Some(b'o'), 33) => &mut stats.header_origin_index,
                (Some(b'w'), 9) => &mut stats.withdrawal_index,
                _ => &mut stats.metadata,
            };
            records.count += 1;
            records.logical_bytes += (key.len() + value.len()) as u64;
            iterator.next();
        }
        if let Some(family) = self.db.cf_handle(index::FAMILY) {
            let mut index = self.db.get_raw_iter_cf(family, &read)?;
            index.seek_to_first();
            while let Some(key) = index.key() {
                let value = index.value().context("RPC index iterator lost its value")?;
                stats.rpc_index.count += 1;
                stats.rpc_index.logical_bytes += (key.len() + value.len()) as u64;
                index.next();
            }
        }
        Ok(stats)
    }

    pub fn restore(
        path: &Path,
        spec: &Genesis,
        reader: &mut impl Read,
        mut on_cell: impl FnMut(u64, &[u8], &[u8]) -> Result<()>,
    ) -> Result<Self> {
        let format = spec.checkpoint_format();
        let db = open_db(path)?;
        let mut last = Vec::new();
        let mut batch = WriteBatch::default();
        let mut bytes = 0;
        let mut offset = 0;
        loop {
            let mut key_len = [0; 4];
            if reader.read(&mut key_len[..1])? == 0 {
                break;
            }
            reader.read_exact(&mut key_len[1..])?;
            let mut value_len = [0; 4];
            reader.read_exact(&mut value_len)?;
            let key_len = u32::from_le_bytes(key_len) as usize;
            let value_len = u32::from_le_bytes(value_len) as usize;
            ensure!(
                (1..=64).contains(&key_len) && value_len <= 16 * 1024 * 1024,
                "snapshot record exceeds bounds"
            );
            let mut key = vec![0; key_len];
            let mut value = vec![0; value_len];
            reader.read_exact(&mut key)?;
            reader.read_exact(&mut value)?;
            ensure!(
                key > last && key.as_slice() <= SNAPSHOT_END,
                "noncanonical archive key order"
            );
            ensure!(
                !local_history_namespace(format, key[0]),
                "compact checkpoint contains local transaction history"
            );
            ensure!(
                format != 4 || key[0] != b't' || value_len > 32,
                "format 4 checkpoint contains an ordinary transaction marker"
            );
            if matches!((key.first(), key.len()), (Some(b'c'), 37)) {
                on_cell(offset, &key, &value)?;
            }
            offset += 8 + (key_len + value_len) as u64;
            bytes += key_len + value_len;
            batch.put(&key, &value)?;
            last = key;
            if bytes >= 4 * 1024 * 1024 {
                db.write(&batch)?;
                batch = WriteBatch::default();
                bytes = 0;
            }
        }
        ensure!(
            last == SNAPSHOT_END,
            "snapshot is missing its terminal record"
        );
        db.write(&batch)?;
        ensure!(db.get(GENESIS)?.is_some(), "snapshot is missing genesis");
        flush_db(&db)?;
        drop(db);
        Self::open(path, spec)
    }

    pub fn flush(&self) -> Result<()> {
        flush_db(&self.db)
    }
}

impl CellProvider for Store {
    fn cell(&self, point: &OutPoint, _eager_load: bool) -> CellStatus {
        if let Some(cell) = self
            .shared_cells
            .lock()
            .expect("cell cache lock")
            .get(point)
        {
            return CellStatus::Live(cell.clone());
        }
        let Some(value) = self.read(&key(b'c', point.as_slice())) else {
            return CellStatus::Unknown;
        };
        // Layout is header hash, transaction index, Molecule CellOutput, raw cell data.
        if value.len() < 40 {
            fatal("truncated stored cell");
        }
        let block_hash = checked(Byte32::from_slice(&value[..32]));
        let index = u32::from_le_bytes(value[32..36].try_into().expect("fixed width")) as usize;
        let output_size =
            u32::from_le_bytes(value[36..40].try_into().expect("fixed width")) as usize;
        if !(4..=value.len() - 36).contains(&output_size) {
            fatal("invalid stored cell length");
        }
        let output = checked(CellOutput::from_slice(&value[36..36 + output_size]));
        let data = Bytes::copy_from_slice(&value[36 + output_size..]);
        let origin = self
            .read(&key(b'o', block_hash.as_slice()))
            .unwrap_or_else(|| fatal("missing cell origin"));
        if origin.len() != 16 {
            fatal("invalid cell origin index");
        }
        let height = u64::from_le_bytes(origin[..8].try_into().expect("fixed width"));
        let epoch = u64::from_le_bytes(origin[8..].try_into().expect("fixed width"));
        let info = TransactionInfo::new(
            height,
            ckb_types::core::EpochNumberWithFraction::from_full_value_unchecked(epoch),
            block_hash,
            index,
        );
        let cell = CellMetaBuilder::from_cell_output(output, data)
            .out_point(point.clone())
            .transaction_info(info)
            .build();
        // Small immutable code and dep-group Cells are used across transactions.
        // Their cached metadata includes the computed data hash.
        if cell.data_bytes <= 2_097_152
            && (cell.data_bytes >= 65_536 || cell.cell_output.lock() == *SYSTEM_LOCK)
        {
            self.shared_cells
                .lock()
                .expect("cell cache lock")
                .put(point.clone(), cell.clone());
        }
        CellStatus::Live(cell)
    }
}

pub fn outputs<'a>(
    tx: &'a TransactionView,
    header: &'a HeaderView,
    index: usize,
) -> impl Iterator<Item = CellMeta> + 'a {
    // Index zero is reserved for CKB cellbase. The sidechain has no cellbase.
    let info = TransactionInfo::new(header.number(), header.epoch(), header.hash(), index + 1);
    let hash = tx.hash();
    tx.outputs_with_data_iter()
        .enumerate()
        .map(move |(index, (output, data))| {
            CellMetaBuilder::from_cell_output(output, data)
                .out_point(OutPoint::new(hash.clone(), index as u32))
                .transaction_info(info.clone())
                .build()
        })
}

fn record_cell(batch: &mut WriteBatch, cell: &CellMeta, header: &HeaderView) -> Result<()> {
    let data = cell.mem_cell_data.as_ref().expect("new cell contains data");
    let index: u32 = cell
        .transaction_info
        .as_ref()
        .expect("new cell has location")
        .index
        .try_into()?;
    let mut bytes = Vec::with_capacity(36 + cell.cell_output.as_slice().len() + data.len());
    bytes.extend(header.hash().as_slice());
    bytes.extend(index.to_le_bytes());
    bytes.extend(cell.cell_output.as_slice());
    bytes.extend(data);
    batch.put(key(b'c', cell.out_point.as_slice()), bytes)?;
    Ok(())
}

fn record_header(batch: &mut WriteBatch, header: &HeaderView) -> Result<()> {
    batch.put(TIP, header.data().as_slice())?;
    batch.put(
        key(b'h', header.hash().as_slice()),
        header.data().as_slice(),
    )?;
    record_origin(batch, header)
}

fn record_origin(batch: &mut WriteBatch, header: &HeaderView) -> Result<()> {
    let mut value = [0; 16];
    value[..8].copy_from_slice(&header.number().to_le_bytes());
    value[8..].copy_from_slice(&header.epoch().full_value().to_le_bytes());
    batch.put(key(b'o', header.hash().as_slice()), value)?;
    Ok(())
}

fn migrate_header_origins(db: &DB, genesis: &Byte32, height: u64) -> Result<()> {
    let mut batch = WriteBatch::default();
    let mut migrated = 0u64;
    let mut read = ReadOptions::default();
    read.fill_cache(false);
    let mut iterator = db.get_raw_iter(&read);
    iterator.seek(b"h");
    let mut found_genesis = false;
    while let Some(key) = iterator.key() {
        if key.first() != Some(&b'h') {
            break;
        }
        let header =
            Header::from_slice(iterator.value().context("missing archived header")?)?.into_view();
        ensure!(
            key.get(1..) == Some(header.hash().as_slice()),
            "archived header hash is inconsistent"
        );
        found_genesis |= header.hash() == *genesis;
        record_origin(&mut batch, &header)?;
        migrated += 1;
        if migrated.is_multiple_of(4096) {
            db.write(&batch)?;
            batch = WriteBatch::default();
        }
        iterator.next();
    }
    ensure!(
        found_genesis && height.checked_add(1) == Some(migrated),
        "database header history is incomplete"
    );
    db.write(&batch)?;
    flush_db(db)?;
    let mut marker = WriteBatch::default();
    marker.put(GENESIS, [STORAGE_FORMAT, genesis.as_slice()].concat())?;
    write_sync(db, &marker)?;
    eprintln!("level=info event=storage_migrated schema=2 indexed_headers={migrated}");
    Ok(())
}

fn record_transaction(
    batch: &mut WriteBatch,
    namespace: u8,
    tx: &TransactionView,
    header: &HeaderView,
) -> Result<()> {
    let transaction = tx.data();
    let mut record = Vec::with_capacity(32 + transaction.as_slice().len());
    record.extend(header.hash().as_slice());
    record.extend(transaction.as_slice());
    batch.put(key(namespace, tx.hash().as_slice()), record)?;
    Ok(())
}

fn local_history_namespace(format: u8, namespace: u8) -> bool {
    (format >= 3 && namespace == ORDINARY_HISTORY)
        || (format == 4 && matches!(namespace, RECENT_STATUS | RECENT_BY_HEIGHT))
}

fn erase_recent_bucket(batch: &mut WriteBatch, bucket: &[u8], hashes: &[u8]) -> Result<()> {
    ensure!(
        !hashes.is_empty() && hashes.len().is_multiple_of(32),
        "invalid recent transaction bucket"
    );
    for hash in hashes.chunks_exact(32) {
        batch.delete(key(RECENT_STATUS, hash))?;
    }
    batch.delete(bucket)?;
    Ok(())
}

fn decode_backing_anchor(record: &[u8]) -> Result<bridge::BackingAnchor> {
    ensure!(record.len() == 40, "invalid committed L1 backing anchor");
    let hash: [u8; 32] = record[8..].try_into()?;
    Ok(bridge::BackingAnchor {
        height: u64::from_le_bytes(record[..8].try_into()?),
        hash: hash.into(),
    })
}

fn write_sync(db: &DB, batch: &WriteBatch) -> Result<()> {
    let mut options = WriteOptions::default();
    options.set_sync(true);
    db.write_opt(batch, &options)?;
    Ok(())
}

fn db_options() -> Options {
    let mut options = Options::default();
    options.create_if_missing(true);
    options.set_write_buffer_size(16 * 1024 * 1024);
    // Admission frequently tests archive keys which do not exist. Filter those
    // misses and keep decoded live-cell and history blocks in a bounded cache.
    let cache = Cache::new_hyper_clock_cache(BLOCK_CACHE_BYTES, 4096);
    let mut blocks = BlockBasedOptions::default();
    blocks.set_ribbon_filter(10.0);
    blocks.set_block_cache(&cache);
    blocks.set_cache_index_and_filter_blocks(true);
    options.set_block_based_table_factory(&blocks);
    options
}

fn open_db(path: &Path) -> Result<DB> {
    let options = db_options();
    if path.join("CURRENT").is_file()
        && DB::list_cf(&options, path)?
            .iter()
            .any(|name| name == index::FAMILY)
    {
        let families = [
            ckb_db::internal::ColumnFamilyDescriptor::new("default", db_options()),
            ckb_db::internal::ColumnFamilyDescriptor::new(index::FAMILY, db_options()),
        ];
        return Ok(DB::open_cf_descriptors(&options, path, families)?);
    }
    Ok(DB::open(&options, path)?)
}

fn flush_db(db: &DB) -> Result<()> {
    let mut options = FlushOptions::default();
    options.set_wait(true);
    db.flush_opt(&options)?;
    Ok(())
}

fn key(prefix: u8, suffix: &[u8]) -> Vec<u8> {
    let mut key = Vec::with_capacity(1 + suffix.len());
    key.push(prefix);
    key.extend(suffix);
    key
}

fn checked<T, E: std::fmt::Display>(result: std::result::Result<T, E>) -> T {
    result.unwrap_or_else(|error| fatal(error))
}

pub fn fatal(reason: impl std::fmt::Display) -> ! {
    eprintln!(
        "level=error event=sidechain_shutdown reason={reason:?}",
        reason = reason.to_string()
    );
    std::process::exit(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        dev::sign_fixture_lock_group, engine::Engine, genesis::script_consensus, tests::payments,
    };
    use ckb_crypto::secp::Privkey;
    use ckb_jsonrpc_types::{IndexerOrder, IndexerSearchKey};
    use ckb_types::packed::{CellInput, Transaction};

    #[test]
    fn format_four_rejects_replay_after_local_status_expiry() {
        let mut fixture = payments(1);
        let spec = &mut fixture.genesis;
        spec.checkpoint_interval = 4;
        spec.checkpoint_cell_proofs = true;
        spec.checkpoint_compact_transactions = true;
        spec.checkpoint_live_cell_replay = true;
        let archive_dir = tempfile::tempdir().unwrap();
        let compact_dir = tempfile::tempdir().unwrap();
        let mut archive = Store::open(archive_dir.path(), spec).unwrap();
        let mut compact = Store::open(compact_dir.path(), spec).unwrap();
        compact.prune_ordinary_history().unwrap();
        let engine = Engine::new(script_consensus().unwrap(), spec.epoch_length, 2).unwrap();
        let parent = Transaction::from_slice(fixture.transactions[0].as_bytes())
            .unwrap()
            .into_view();
        let child = sign_fixture_lock_group(
            &parent
                .as_advanced_builder()
                .set_inputs(vec![CellInput::new(OutPoint::new(parent.hash(), 0), 0)])
                .build(),
            &Privkey::from_slice(&[2; 32]),
        )
        .unwrap();
        for store in [&mut archive, &mut compact] {
            let block = engine
                .verify(
                    store,
                    1,
                    spec.timestamp_ms + 1,
                    vec![parent.data().as_bytes(), child.data().as_bytes()],
                )
                .unwrap();
            store
                .commit(block.header, &block.transactions, block.cells)
                .unwrap();
        }
        let origin = compact.tip.hash();
        assert!(archive.transaction(&parent.hash()).is_some());
        assert!(compact.transaction(&parent.hash()).is_none());
        assert!(!compact.has_canonical_transaction(&parent.hash()));
        assert_eq!(
            compact.transaction_or_origin(&parent.hash()).unwrap(),
            origin.as_slice()
        );
        assert_eq!(
            compact
                .storage_stats()
                .unwrap()
                .recent_transaction_status
                .count,
            2
        );
        assert_eq!(
            archive.export(&mut std::io::sink()).unwrap(),
            compact.export(&mut std::io::sink()).unwrap()
        );
        // The status of a same-block-spent output still survives an ordinary restart.
        drop(compact);
        let mut compact = Store::open(compact_dir.path(), spec).unwrap();
        compact.prune_ordinary_history().unwrap();
        assert!(compact.transaction_or_origin(&parent.hash()).is_some());
        for height in 2..=1 + RECENT_TRANSACTION_BLOCKS {
            let block = engine
                .verify(&compact, height, spec.timestamp_ms + height, vec![])
                .unwrap();
            if height == 1 + RECENT_TRANSACTION_BLOCKS {
                assert!(compact.transaction_or_origin(&parent.hash()).is_some());
            }
            compact.commit(block.header, &[], block.cells).unwrap();
        }
        assert!(compact.transaction_or_origin(&parent.hash()).is_none());
        assert_eq!(
            compact.transaction_or_origin(&child.hash()).unwrap(),
            origin.as_slice()
        );
        assert_eq!(
            compact
                .storage_stats()
                .unwrap()
                .recent_transaction_status
                .count,
            0
        );
        assert_eq!(
            compact
                .storage_stats()
                .unwrap()
                .recent_transaction_expiry
                .count,
            0
        );
        for store in [&archive, &compact] {
            for replay in [&parent, &child] {
                assert!(engine.check_tx(store, replay.data().as_bytes()).is_err());
            }
        }
    }

    #[test]
    fn migrates_a_legacy_live_cell_without_changing_the_checkpoint_image() {
        let mut fixture = payments(1);
        fixture.genesis.checkpoint_interval = 4;
        let spec = &fixture.genesis;
        let path = tempfile::tempdir().unwrap();
        let mut store = Store::open(path.path(), spec).unwrap();
        let engine = Engine::new(script_consensus().unwrap(), spec.epoch_length, 1).unwrap();
        let block = engine
            .verify(
                &store,
                1,
                spec.timestamp_ms,
                vec![fixture.transactions[0].clone().into_bytes()],
            )
            .unwrap();
        let point = OutPoint::new(block.transactions[0].hash(), 0);
        store
            .commit(block.header, &block.transactions, block.cells)
            .unwrap();
        let old_cell = store.cell(&point, true);
        let old_root = store.export(&mut std::io::sink()).unwrap();
        let mut legacy = WriteBatch::default();
        legacy
            .delete(key(b'o', store.tip.hash().as_slice()))
            .unwrap();
        legacy
            .delete(key(b'o', store.genesis_hash.as_slice()))
            .unwrap();
        legacy
            .put(GENESIS, [FORMAT, store.genesis_hash.as_slice()].concat())
            .unwrap();
        write_sync(&store.db, &legacy).unwrap();
        drop(store);

        let migrated = Store::open(path.path(), spec).unwrap();
        assert_eq!(migrated.cell(&point, true), old_cell);
        assert_eq!(migrated.export(&mut std::io::sink()).unwrap(), old_root);
        assert_eq!(
            migrated.storage_stats().unwrap().header_origin_index.count,
            2
        );
    }

    #[test]
    fn rebuilds_stale_wallet_index_and_preserves_canonical_snapshots() {
        let mut fixture = payments(3);
        fixture.genesis.checkpoint_interval = 4;
        let spec = &fixture.genesis;
        let path = tempfile::tempdir().unwrap();
        let engine = Engine::new(script_consensus().unwrap(), spec.epoch_length, 1).unwrap();
        let mut store = Store::open(path.path(), spec).unwrap();
        let funding =
            ckb_types::packed::Transaction::from_slice(spec.allocations.last().unwrap().as_bytes())
                .unwrap()
                .into_view();
        let lock = funding.output(0).unwrap().lock();
        let search = || IndexerSearchKey {
            script: lock.clone().into(),
            with_data: Some(false),
            ..Default::default()
        };
        let points = |store: &Store| {
            store
                .rpc_index()
                .unwrap()
                .cells(search(), IndexerOrder::Asc, 10, None, |point| {
                    store.indexed_cell(point)
                })
                .unwrap()
                .objects
                .into_iter()
                .map(|cell| cell.out_point.index.value())
                .collect::<Vec<_>>()
        };
        let commit = |store: &mut Store, height, tx: usize| {
            let block = engine
                .verify(
                    store,
                    height,
                    spec.timestamp_ms + height,
                    vec![fixture.transactions[tx].clone().into_bytes()],
                )
                .unwrap();
            store
                .commit(block.header, &block.transactions, block.cells)
                .unwrap();
        };

        store.enable_rpc_index().unwrap();
        assert_eq!(points(&store), [0, 1, 2]);
        commit(&mut store, 1, 0);
        assert_eq!(points(&store), [1, 2]);
        drop(store);

        let mut store = Store::open(path.path(), spec).unwrap();
        assert!(store.rpc_index().is_err());
        commit(&mut store, 2, 1);
        let checkpoint = store.export(&mut std::io::sink()).unwrap();
        store.enable_rpc_index().unwrap();
        assert_eq!(points(&store), [2]);
        let index = store.rpc_index().unwrap();
        assert_eq!(
            index.header_hash(0).unwrap(),
            Some(store.genesis_hash.clone())
        );
        assert_eq!(index.header_hash(2).unwrap(), Some(store.tip.hash()));
        assert_eq!(store.export(&mut std::io::sink()).unwrap(), checkpoint);
    }
}
