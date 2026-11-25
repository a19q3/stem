//! Export and restore canonical state records using an externally authenticated
//! Stem AppHash. The Patricia nodes are reconstructed at the destination.

use std::{
    io::{Read, Write},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{DB, Direction, IteratorMode, prelude::*};
use ckb_types::{packed::Byte32, prelude::*};
use stem_ledger::{
    block::Header,
    history::History,
    patricia::{Key, Namespace},
    record,
};

use super::{
    Batch, HEADER, HEADER_ID, HISTORY, STATE_NODE, STATE_RECORD, Store, TIP, index_key, put_nodes,
    put_tip, state_key, sync,
};

#[derive(Clone, Debug)]
pub struct StateSnapshot {
    pub header: Header,
    pub frontier: Vec<u8>,
    /// Canonical Cell and protocol records. Keys are in database order.
    pub records: Vec<(Namespace, Key, Vec<u8>)>,
    /// A suffix of native Headers including the checkpoint, in ascending order.
    pub headers: Vec<Header>,
}

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
        ensure!(
            self.tip.context.height == 0 && image.header.context.height != 0,
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
        let mut cells = self.cells.edit(self.cells.empty_root(), |_| Ok(None));
        let mut system = self.system.edit(self.system.empty_root(), |_| Ok(None));
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
                    cells.set(key, Some(self.cells.value_hash(&data)))?;
                }
                Namespace::System => {
                    system.set(key, Some(self.system.value_hash(&data)))?;
                }
            }
            batch.put(state_key(STATE_RECORD, namespace, &key), data);
        }
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
        sync(&self.db, &batch)?;
        self.tip = image.header;
        self.history = frontier;
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

const HEADER_BYTES: usize = 208;

impl StateSnapshot {
    pub fn encode(&self, mut out: impl Write) -> Result<()> {
        out.write_all(&self.header.encode())?;
        sized_write(&mut out, &self.frontier)?;
        out.write_all(&u32::try_from(self.headers.len())?.to_le_bytes())?;
        for header in &self.headers {
            out.write_all(&header.encode())?;
        }
        out.write_all(&u32::try_from(self.records.len())?.to_le_bytes())?;
        for (namespace, key, record) in &self.records {
            out.write_all(&[namespace.tag()])?;
            out.write_all(key)?;
            sized_write(&mut out, record)?;
        }
        Ok(())
    }

    pub fn decode(mut input: impl Read, length: u64) -> Result<Self> {
        let mut remaining = length;
        let mut encoded = [0; HEADER_BYTES];
        snapshot_read(&mut input, &mut remaining, &mut encoded)?;
        let header = Header::decode(&encoded)?;
        let frontier = sized_read(&mut input, &mut remaining, 8 + 64 * 32)?;
        let mut count = [0; 4];
        snapshot_read(&mut input, &mut remaining, &mut count)?;
        let headers_count = u32::from_le_bytes(count);
        ensure!(
            u64::from(headers_count) <= remaining / HEADER_BYTES as u64,
            "invalid snapshot Header count"
        );
        let mut headers = Vec::new();
        for _ in 0..headers_count {
            snapshot_read(&mut input, &mut remaining, &mut encoded)?;
            headers.push(Header::decode(&encoded)?);
        }
        snapshot_read(&mut input, &mut remaining, &mut count)?;
        let records_count = u32::from_le_bytes(count);
        ensure!(
            u64::from(records_count) <= remaining / 37,
            "invalid snapshot record count"
        );
        let mut records = Vec::new();
        for _ in 0..records_count {
            let mut prefix = [0; 33];
            snapshot_read(&mut input, &mut remaining, &mut prefix)?;
            let namespace = match prefix[0] {
                0 => Namespace::Cells,
                1 => Namespace::System,
                _ => anyhow::bail!("invalid snapshot state namespace"),
            };
            let key = prefix[1..].try_into()?;
            let value = sized_read(&mut input, &mut remaining, stem_ledger::MAX_CELL_BYTES + 84)?;
            records.push((namespace, key, value));
        }
        ensure!(remaining == 0, "trailing snapshot data");
        Ok(Self {
            header,
            frontier,
            records,
            headers,
        })
    }
}

fn sized_write(out: &mut impl Write, bytes: &[u8]) -> Result<()> {
    out.write_all(&u32::try_from(bytes.len())?.to_le_bytes())?;
    out.write_all(bytes)?;
    Ok(())
}

fn sized_read(input: &mut impl Read, remaining: &mut u64, limit: usize) -> Result<Vec<u8>> {
    let mut size = [0; 4];
    *remaining = remaining
        .checked_sub(4)
        .context("truncated snapshot length")?;
    input.read_exact(&mut size)?;
    let size = u32::from_le_bytes(size) as usize;
    ensure!(size <= limit, "snapshot record exceeds limit");
    *remaining = remaining
        .checked_sub(size as u64)
        .context("truncated snapshot record")?;
    let mut bytes = vec![0; size];
    input.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn snapshot_read(input: &mut impl Read, remaining: &mut u64, bytes: &mut [u8]) -> Result<()> {
    *remaining = remaining
        .checked_sub(bytes.len() as u64)
        .context("truncated snapshot")?;
    input.read_exact(bytes)?;
    Ok(())
}
