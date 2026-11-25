use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    thread,
};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ckb_types::{packed::Byte32, prelude::Entity};
use tendermint_proto::v0_38::abci::{
    self, response_apply_snapshot_chunk::Result as Applied,
    response_offer_snapshot::Result as Offer,
};

use crate::native_store::{SnapshotReader, StateSnapshot, Store};

const FORMAT: u32 = 1;
const CHUNK: u64 = 1024 * 1024;
const KEEP: usize = 4;

pub struct Snapshots {
    interval: u64,
    maximum: u64,
    directory: PathBuf,
    captures: mpsc::SyncSender<()>,
    published: Arc<Mutex<BTreeMap<u64, Saved>>>,
    receiver: Mutex<Option<Incoming>>,
}

#[derive(Clone)]
struct Saved {
    path: PathBuf,
    metadata: abci::Snapshot,
    size: u64,
}

struct Incoming {
    file: File,
    snapshot: abci::Snapshot,
    app_hash: Byte32,
    size: u64,
    received: Vec<bool>,
    missing: u32,
}

impl Snapshots {
    pub fn open(
        directory: &Path,
        reader: SnapshotReader,
        interval: u64,
        maximum: u64,
    ) -> Result<Self> {
        ensure!(
            maximum >= CHUNK && maximum.div_ceil(CHUNK) <= u64::from(u32::MAX),
            "invalid snapshot byte limit"
        );
        fs::create_dir_all(directory)?;
        let published = Arc::new(Mutex::new(BTreeMap::new()));
        let (captures, ready) = mpsc::sync_channel(1);
        let archive = Arc::clone(&published);
        let target = directory.to_owned();
        thread::Builder::new()
            .name("fern-snapshot".into())
            .spawn(move || {
                if let Err(error) = discover(&target, &archive, maximum) {
                    eprintln!("level=warn event=snapshot_discovery_failed error={error:#}");
                }
                while ready.recv().is_ok() {
                    if let Err(error) = publish(&target, &archive, &reader, maximum) {
                        eprintln!("level=warn event=snapshot_publish_failed error={error:#}");
                    }
                }
            })?;
        Ok(Self {
            interval,
            maximum,
            directory: directory.to_owned(),
            captures,
            published,
            receiver: Mutex::new(None),
        })
    }

    pub fn committed(&self, height: u64) {
        if self.interval != 0 && height.is_multiple_of(self.interval) {
            let _ = self.captures.try_send(());
        }
    }

    pub fn list(&self, committed: u64) -> abci::ResponseListSnapshots {
        let snapshots = self
            .published
            .lock()
            .expect("snapshot archive lock")
            .values()
            .rev()
            .filter(|entry| {
                entry
                    .metadata
                    .height
                    .checked_add(2)
                    .is_some_and(|safe| safe <= committed)
            })
            .map(|entry| entry.metadata.clone())
            .collect();
        abci::ResponseListSnapshots { snapshots }
    }

    pub fn chunk(
        &self,
        request: abci::RequestLoadSnapshotChunk,
        committed: u64,
    ) -> Result<abci::ResponseLoadSnapshotChunk> {
        ensure!(
            request.format == FORMAT
                && request
                    .height
                    .checked_add(2)
                    .is_some_and(|safe| safe <= committed),
            "requested snapshot is not available"
        );
        let saved = self
            .published
            .lock()
            .expect("snapshot archive lock")
            .get(&request.height)
            .cloned()
            .context("requested snapshot is not stored")?;
        let (offset, length) = extent(request.chunk, saved.size)?;
        let mut file = File::open(saved.path)?;
        file.seek(SeekFrom::Start(offset))?;
        let mut chunk = vec![0; length];
        file.read_exact(&mut chunk)?;
        Ok(abci::ResponseLoadSnapshotChunk {
            chunk: Bytes::from(chunk),
        })
    }

    pub fn offer(
        &self,
        request: abci::RequestOfferSnapshot,
        store_height: u64,
    ) -> Result<abci::ResponseOfferSnapshot> {
        let snapshot = request.snapshot.context("missing offered snapshot")?;
        if snapshot.format != FORMAT {
            return Ok(abci::ResponseOfferSnapshot {
                result: Offer::RejectFormat as i32,
            });
        }
        ensure!(
            store_height == 0 && snapshot.height > 0 && snapshot.hash.len() == 32,
            "invalid snapshot offer"
        );
        let size = u64::from_le_bytes(snapshot.metadata.as_ref().try_into()?);
        ensure!(
            size != 0 && size <= self.maximum && size.div_ceil(CHUNK) == u64::from(snapshot.chunks),
            "snapshot offer exceeds byte budget or has inconsistent chunk count"
        );
        let app_hash = Byte32::from_slice(&request.app_hash)?;
        let file = tempfile::tempfile_in(&self.directory)?;
        let received = vec![false; snapshot.chunks as usize];
        *self.receiver.lock().expect("snapshot receiver lock") = Some(Incoming {
            file,
            app_hash,
            size,
            missing: snapshot.chunks,
            snapshot,
            received,
        });
        Ok(abci::ResponseOfferSnapshot {
            result: Offer::Accept as i32,
        })
    }

    pub fn apply(
        &self,
        request: abci::RequestApplySnapshotChunk,
        store: &mut Store,
    ) -> Result<abci::ResponseApplySnapshotChunk> {
        let mut guard = self.receiver.lock().expect("snapshot receiver lock");
        let candidate = guard.as_mut().context("no snapshot is being retrieved")?;
        let (offset, length) = extent(request.index, candidate.size)?;
        if request.chunk.len() != length {
            return Ok(abci::ResponseApplySnapshotChunk {
                result: Applied::Retry as i32,
                refetch_chunks: vec![request.index],
                reject_senders: (request.sender != "")
                    .then_some(request.sender)
                    .into_iter()
                    .collect(),
            });
        }
        if !candidate.received[request.index as usize] {
            candidate.file.seek(SeekFrom::Start(offset))?;
            candidate.file.write_all(&request.chunk)?;
            candidate.received[request.index as usize] = true;
            candidate.missing -= 1;
        }
        if candidate.missing == 0 {
            let mut candidate = guard.take().expect("complete snapshot");
            if file_hash(&mut candidate.file)? != candidate.snapshot.hash.as_ref() {
                return Ok(abci::ResponseApplySnapshotChunk {
                    result: Applied::RejectSnapshot as i32,
                    ..Default::default()
                });
            }
            candidate.file.seek(SeekFrom::Start(0))?;
            let state = StateSnapshot::decode(&mut candidate.file, candidate.size)?;
            ensure!(
                state.header.context.height == candidate.snapshot.height,
                "snapshot height differs from offer"
            );
            store.install_state(state, &candidate.app_hash)?;
            eprintln!(
                "level=info event=snapshot_installed height={}",
                candidate.snapshot.height
            );
        }
        Ok(abci::ResponseApplySnapshotChunk {
            result: Applied::Accept as i32,
            ..Default::default()
        })
    }
}

fn extent(index: u32, size: u64) -> Result<(u64, usize)> {
    let offset = u64::from(index) * CHUNK;
    ensure!(offset < size, "snapshot chunk index exceeds size");
    Ok((offset, usize::try_from((size - offset).min(CHUNK))?))
}

fn file_hash(file: &mut File) -> Result<[u8; 32]> {
    file.seek(SeekFrom::Start(0))?;
    let mut hash = ckb_hash::new_blake2b();
    hash.update(b"Fern state snapshot\0");
    let mut chunk = vec![0; CHUNK as usize];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        hash.update(&chunk[..read]);
    }
    let mut digest = [0; 32];
    hash.finalize(&mut digest);
    Ok(digest)
}

fn describe(path: PathBuf, height: u64, maximum: u64) -> Result<Saved> {
    let mut file = File::open(&path)?;
    let size = file.metadata()?.len();
    ensure!(
        size != 0 && size <= maximum,
        "snapshot exceeds local byte limit"
    );
    let metadata = abci::Snapshot {
        height,
        format: FORMAT,
        chunks: u32::try_from(size.div_ceil(CHUNK))?,
        hash: Bytes::copy_from_slice(&file_hash(&mut file)?),
        metadata: Bytes::copy_from_slice(&size.to_le_bytes()),
    };
    Ok(Saved {
        path,
        metadata,
        size,
    })
}

fn discover(directory: &Path, archive: &Mutex<BTreeMap<u64, Saved>>, maximum: u64) -> Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(height) = name
            .to_str()
            .and_then(|name| name.strip_suffix(".snapshot"))
            .and_then(|name| name.parse::<u64>().ok())
        else {
            continue;
        };
        if let Ok(saved) = describe(entry.path(), height, maximum) {
            archive
                .lock()
                .expect("snapshot archive lock")
                .insert(height, saved);
        }
    }
    prune(archive)
}

fn publish(
    directory: &Path,
    archive: &Mutex<BTreeMap<u64, Saved>>,
    reader: &SnapshotReader,
    maximum: u64,
) -> Result<()> {
    let snapshot = reader.read()?;
    let height = snapshot.header.context.height;
    let path = directory.join(format!("{height}.snapshot"));
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    snapshot.encode(&mut temporary)?;
    ensure!(
        temporary.as_file().metadata()?.len() <= maximum,
        "snapshot exceeds local byte limit"
    );
    temporary.as_file().sync_all()?;
    temporary.persist(&path)?;
    let saved = describe(path, height, maximum)?;
    archive
        .lock()
        .expect("snapshot archive lock")
        .insert(height, saved);
    prune(archive)?;
    eprintln!("level=info event=snapshot_published height={height}");
    Ok(())
}

fn prune(archive: &Mutex<BTreeMap<u64, Saved>>) -> Result<()> {
    loop {
        let oldest = {
            let mut entries = archive.lock().expect("snapshot archive lock");
            if entries.len() <= KEEP {
                return Ok(());
            }
            entries.pop_first().expect("snapshot archive is nonempty").1
        };
        fs::remove_file(oldest.path)?;
    }
}
