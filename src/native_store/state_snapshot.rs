//! Export and restore canonical state records using an externally authenticated
//! Stem AppHash. The Patricia nodes are reconstructed at the destination.

use std::sync::Arc;

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{DB, Direction, IteratorMode, prelude::*};
use ckb_types::{packed::Byte32, prelude::*};
use stem_ledger::{
    block::Header,
    history::History,
    patricia::Namespace,
    record, snapshot::StateSnapshot,
};

use super::{
    Batch, HEADER, HEADER_ID, HISTORY, STATE_NODE, STATE_RECORD, Store, TIP, index_key, put_nodes,
    put_tip, state_key, sync_with_consensus,
};

pub struct SnapshotReader {
    db: Arc<DB>,
    previous_headers: u64,
}

impl SnapshotReader {
    pub fn read(&self) -> Result<StateSnapshot> {
        let snapshot = self.db.snapshot();
        let header = Header::decode(&snapshot.get(TIP)?.context("missing snapshot tip")?)?;
        let frontier = snapshot
            .get(HISTORY)?
            .context("missing snapshot frontier")?
            .to_vec();
        let mut records = Vec::new();
        let mut itr = snapshot.get_raw_iter(&Default::default());
        itr.seek(&[STATE_RECORD]);
        while itr.valid() {
            let key = itr.key().context("invalid snapshot record cursor")?;
            let value = itr
                .value()
                .context("invalid snapshot record value cursor")?;
            if key.first() != Some(&STATE_RECORD) {
                break;
            }
            let key: [u8; 34] = key.try_into()?;
            let namespace = match key[1] {
                0 => Namespace::Cells,
                1 => Namespace::System,
                _ => anyhow::bail!("invalid state namespace"),
            };
            records.push((namespace, key[2..].try_into()?, value.to_vec()));
            itr.next();
        }
        let first = header.context.height.saturating_sub(self.previous_headers);
        let headers = (first..=header.context.height)
            .map(|height| {
                let key = index_key(HEADER, &height.to_be_bytes());
                Header::decode(
                    &snapshot
                        .get(key)?
                        .context("missing snapshot Header Dep history")?,
                )
            })
            .collect::<Result<_>>()?;
        Ok(StateSnapshot {
            header,
            frontier,
            records,
            headers,
        })
    }
}

impl Store {
    pub fn snapshot_reader(&self) -> SnapshotReader {
        SnapshotReader {
            db: Arc::clone(&self.db),
            previous_headers: self.previous_headers(),
        }
    }

    pub fn export_state(&self) -> Result<StateSnapshot> {
        self.snapshot_reader().read()
    }

    /// The caller authenticates the AppHash against a CometBFT H+1 commit or
    /// an already verified L1 checkpoint. Installation is for a fresh replica.
    pub fn install_state(&mut self, image: StateSnapshot, trusted_app_hash: &Byte32) -> Result<()> {
        self.install_certified_state(image, trusted_app_hash, &[])
    }

    pub(crate) fn install_certified_state(
        &mut self,
        image: StateSnapshot,
        trusted_app_hash: &Byte32,
        consensus: &[(Vec<u8>, Vec<u8>)],
    ) -> Result<()> {
        ensure!(!self.research_jmt, "snapshot installation is outside the JMT comparison");
        ensure!(
            self.branch.is_empty()
                && self.tip.context.height == 0
                && image.header.context.height != 0,
            "snapshot install requires a fresh replica and non-genesis state"
        );
        ensure!(
            image.header.app_hash(&self.network) == *trusted_app_hash,
            "snapshot Header differs from authenticated AppHash"
        );
        let height = image.header.context.height;
        let frontier = History::decode(self.network.clone(), &image.frontier)?;
        ensure!(
            frontier.count() == height && frontier.root() == image.header.state.history_root,
            "snapshot frontier differs from authenticated history"
        );
        let first_height = self.first_required_header(height);
        ensure!(
            image
                .headers
                .first()
                .is_some_and(|first| first.context.height == first_height)
                && image.headers.last() == Some(&image.header),
            "snapshot Header suffix is incomplete"
        );
        if first_height == 0 {
            ensure!(
                image.headers[0].id(&self.network) == self.genesis_id,
                "snapshot genesis differs from local spec"
            );
        }
        for pair in image.headers.windows(2) {
            let parent = &pair[0];
            let child = &pair[1];
            ensure!(
                parent.context.height.checked_add(1) == Some(child.context.height)
                    && parent.id(&self.network) == child.context.parent
                    && parent.context.timestamp_ms <= child.context.timestamp_ms,
                "snapshot Header history is discontinuous"
            );
        }

        let mut batch = Batch::default();
        // A fresh replica contains only its local genesis state. The batch
        // replaces that state and its history indices together.
        for prefix in [STATE_NODE, STATE_RECORD, HEADER, HEADER_ID] {
            for (key, _) in self
                .db
                .iterator(IteratorMode::From(&[prefix], Direction::Forward))
            {
                if key.first() != Some(&prefix) {
                    break;
                }
                batch.delete(key);
            }
        }
        let mut cells = self
            .cells
            .edit(self.cells.empty_root(), |_| Ok(None::<Vec<u8>>));
        let mut system = self
            .system
            .edit(self.system.empty_root(), |_| Ok(None::<Vec<u8>>));
        let mut cell_records = Vec::new();
        let mut system_records = Vec::new();
        let mut previous = None;
        for (namespace, key, data) in image.records {
            let ordered = (namespace.tag(), key);
            ensure!(
                previous.is_none_or(|prior| prior < ordered),
                "unordered or duplicate snapshot state key"
            );
            previous = Some(ordered);
            match namespace {
                Namespace::Cells => {
                    let (point, cell) = record::decode_cell(&data)?;
                    ensure!(
                        record::cell_key(&self.cells, &point) == key
                            && !point.is_null()
                            && cell.origin.height <= height
                            && cell.origin.timestamp_ms <= image.header.context.timestamp_ms,
                        "snapshot Live Cell key or creation context is invalid"
                    );
                    self.capacity.verify(&cell.cell)?;
                    if let Some(index) = cell.origin.height.checked_sub(first_height) {
                        let context = &image
                            .headers
                            .get(usize::try_from(index)?)
                            .context("missing Cell creation Header")?
                            .context;
                        ensure!(
                            cell.origin.context_id == context.id(&self.network)
                                && cell.origin.timestamp_ms == context.timestamp_ms,
                            "snapshot Cell origin differs from matching Header"
                        );
                    }
                    cell_records.push((key, Some(self.cells.value_hash(&data))));
                }
                Namespace::System => {
                    system_records.push((key, Some(self.system.value_hash(&data))));
                }
            }
            batch.put(state_key(STATE_RECORD, namespace, &key), data);
        }
        cells.set_batch(&cell_records, |_, _, _| {})?;
        system.set_batch(&system_records, |_, _, _| {})?;
        ensure!(
            cells.root().pack() == image.header.state.cell_root
                && system.root().pack() == image.header.state.system_root,
            "snapshot records differ from authenticated state roots"
        );
        put_nodes(&mut batch, Namespace::Cells, cells.into_writes());
        put_nodes(&mut batch, Namespace::System, system.into_writes());
        for header in image.headers {
            let index = header.context.height.to_be_bytes();
            batch.put(index_key(HEADER, &index), header.encode());
            batch.put(
                index_key(HEADER_ID, header.id(&self.network).as_slice()),
                index,
            );
        }
        put_tip(&mut batch, &self.network, &image.header, &frontier);
        sync_with_consensus(
            &self.db,
            &batch,
            None,
            consensus,
            &[],
            self.metrics
                .as_deref()
                .map(|m| (m, super::Purpose::Restore)),
        )?;
        self.tip = image.header;
        #[cfg(feature = "native-hot-cell-node-cache")]
        {
            self.cell_node_header = self.tip.id(&self.network).unpack();
        }
        self.stake_inputs = Arc::default();
        self.live_cache.clear();
        self.history = frontier;
        if let Some(index) = self.live_index.take() {
            let (limit, layout, workers) = index.retire();
            self.live_index = Some(super::IndexView::recover(self, limit, layout, workers)?);
        }
        if let Some(index) = self.cell_nodes.take() {
            let limit = index.retire();
            self.cell_nodes = Some(Arc::new(super::NodeIndex::recover(self, limit)?));
        }
        Ok(())
    }

    fn first_required_header(&self, height: u64) -> u64 {
        height.saturating_sub(self.previous_headers())
    }

    fn previous_headers(&self) -> u64 {
        let staking = self
            .staking
            .as_ref()
            .map(|stake| stake.window())
            .unwrap_or_default();
        self.limits.header_dep_age.max(staking).saturating_sub(1)
    }
}
