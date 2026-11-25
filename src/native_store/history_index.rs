//! Rebuildable MMR proof index over committed Headers in an archive node.

use super::{HEADER, TIP, index_key};
use anyhow::{Context, Result, ensure};
use ckb_db::internal::{DB, Options, ReadOnlyDB, WriteBatch, WriteOptions, prelude::*};
use ckb_types::{packed::Byte32, prelude::*};
use std::path::Path;
use stem_ledger::block::Header;
use stem_stake::{
    Hash32,
    history::{self, Frontier},
};

const MARKER: &[u8] = b"stem/history-index";
const PROGRESS: &[u8] = b"stem/history-progress";

pub struct HistoryIndex {
    archive: ReadOnlyDB,
    db: DB,
    network: Byte32,
    frontier: Frontier,
}

impl HistoryIndex {
    pub fn open(archive: &Path, index: &Path, network: Byte32) -> Result<Self> {
        let archive = ReadOnlyDB::open(&Options::default(), archive)?;
        let genesis = Self::load_header(&archive, 0)?;
        let mut options = Options::default();
        options.create_if_missing(true);
        let db = DB::open(&options, index)?;
        let id = genesis.id(&network);
        let frontier = if let Some(marker) = db.get(MARKER)? {
            ensure!(
                marker.as_ref() == id.as_slice(),
                "history index belongs to a different genesis"
            );
            let encoded = db
                .get(PROGRESS)?
                .context("missing history index frontier")?;
            Frontier::decode(network.unpack(), &encoded)?
        } else {
            let frontier = Frontier::empty(network.unpack());
            let mut batch = WriteBatch::default();
            batch.put(MARKER, id.as_slice())?;
            batch.put(PROGRESS, &frontier.encode())?;
            Self::commit(&db, &batch)?;
            frontier
        };
        Ok(Self {
            archive,
            db,
            network,
            frontier,
        })
    }

    fn load_header(archive: &ReadOnlyDB, height: u64) -> Result<Header> {
        let encoded = archive
            .get(index_key(HEADER, &height.to_be_bytes()))?
            .with_context(|| format!("archive is missing the Stem Header at height {height}"))?;
        let header = Header::decode(&encoded)?;
        ensure!(
            header.context.height == height,
            "archive Header height mismatch"
        );
        Ok(header)
    }

    pub fn header(&self, height: u64) -> Result<Header> {
        Self::load_header(&self.archive, height)
    }

    pub fn sync(&mut self, target: u64) -> Result<()> {
        let tip = Header::decode(&self.archive.get(TIP)?.context("missing archive tip")?)?;
        ensure!(
            target <= tip.context.height,
            "target is ahead of the archive tip"
        );
        let mut parent = if let Some(height) = self.frontier.count().checked_sub(1) {
            self.header(height)?.id(&self.network)
        } else {
            Byte32::default()
        };
        while self.frontier.count() < target {
            let end = target.min(self.frontier.count().saturating_add(256));
            let mut frontier = self.frontier.clone();
            let mut batch = WriteBatch::default();
            while frontier.count() < end {
                let height = frontier.count();
                let header = self.header(height)?;
                let root: Hash32 = header.state.history_root.unpack();
                ensure!(
                    header.context.parent == parent && root == frontier.root(),
                    "archive Header does not extend the indexed history at height {height}"
                );
                parent = header.id(&self.network);
                let mut nodes = Vec::new();
                frontier.append_indexed(height, &parent.unpack(), |start, level, hash| {
                    nodes.push((start, level, hash));
                })?;
                for (start, level, hash) in nodes {
                    batch.put(&node_key(start, level), &hash)?;
                }
            }
            batch.put(PROGRESS, &frontier.encode())?;
            Self::commit(&self.db, &batch)?;
            self.frontier = frontier;
        }
        let at_target = self.header(target)?;
        let root: Hash32 = at_target.state.history_root.unpack();
        ensure!(
            self.frontier_at(target)?.root() == root,
            "indexed history does not match the target Stem Header"
        );
        Ok(())
    }

    fn commit(db: &DB, batch: &WriteBatch) -> Result<()> {
        let mut options = WriteOptions::default();
        options.set_sync(true);
        Ok(db.write_opt(batch, &options)?)
    }

    fn node(&self, start: u64, level: u32) -> Result<Hash32> {
        let data = self
            .db
            .get(node_key(start, level))?
            .context("history index node is missing")?;
        Ok(data.as_ref().try_into()?)
    }

    pub fn frontier_at(&self, height: u64) -> Result<Frontier> {
        ensure!(
            height <= self.frontier.count(),
            "index has not reached the requested height"
        );
        Frontier::from_index(self.network.unpack(), height, |start, level| {
            self.node(start, level)
        })
    }

    pub fn inclusion(&self, height: u64, tip: u64) -> Result<Vec<Hash32>> {
        ensure!(
            tip <= self.frontier.count(),
            "index has not reached the requested height"
        );
        history::inclusion(height, tip, |start, level| self.node(start, level))
    }

    pub fn extension(&self, from: u64, to: u64) -> Result<Vec<Hash32>> {
        ensure!(
            to <= self.frontier.count(),
            "index has not reached the requested height"
        );
        history::extension(from, to, |start, level| self.node(start, level))
    }
}

fn node_key(start: u64, level: u32) -> [u8; 10] {
    let mut key = [b'm'; 10];
    key[1..9].copy_from_slice(&start.to_be_bytes());
    key[9] = level as u8;
    key
}
