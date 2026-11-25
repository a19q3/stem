use std::{
    fs::{self, File},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ckb_hash::new_blake2b;
use ckb_types::{
    core::HeaderView,
    packed::{Byte32, Header, OutPoint},
    prelude::*,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::{NamedTempFile, TempDir};
use tendermint_proto::v0_38::abci::Snapshot;

use crate::{
    cell_proof::{self, MAX_IMAGE},
    engine::Engine,
    genesis::Genesis,
    store::Store,
};

const CHUNK: u64 = 1024 * 1024;
const V1_METADATA_SIZE: usize = 40 + Header::TOTAL_SIZE;
const ACTIVE: &str = "active-store";

#[derive(Clone)]
struct Manifest {
    header: HeaderView,
    size: u64,
    digest: Byte32,
    cells_root: Option<cell_proof::Hash>,
    format: u8,
}

impl Manifest {
    fn hash(&self) -> Byte32 {
        if let Some(root) = self.cells_root {
            let mut hash = Sha256::new();
            hash.update(match self.format {
                2 => b"CKB sidechain authenticated database snapshot v2\0",
                3 => b"CKB sidechain authenticated database snapshot v3\0",
                4 => b"CKB sidechain authenticated database snapshot v4\0",
                _ => unreachable!("checkpoint format with a Cell proof root"),
            });
            hash.update(self.header.number().to_le_bytes());
            hash.update(self.header.hash().as_slice());
            hash.update(self.size.to_le_bytes());
            hash.update(self.digest.as_slice());
            hash.update(root);
            let hash: [u8; 32] = hash.finalize().into();
            return hash.into();
        }
        let mut hasher = new_blake2b();
        hasher.update(b"CKB sidechain authenticated database snapshot v1\0");
        hasher.update(self.header.hash().as_slice());
        hasher.update(&self.size.to_le_bytes());
        hasher.update(self.digest.as_slice());
        let mut hash = [0u8; 32];
        hasher.finalize(&mut hash);
        hash.into()
    }

    fn encode(&self) -> Bytes {
        let mut encoded = Vec::new();
        encoded.extend(self.digest.as_slice());
        encoded.extend(self.size.to_le_bytes());
        encoded.extend(self.header.data().as_slice());
        if let Some(root) = self.cells_root {
            encoded.extend(root);
        }
        if self.format >= 3 {
            encoded.push(self.format);
        }
        encoded.into()
    }

    fn decode(encoded: &[u8]) -> Result<Self> {
        let (root_end, format) = match encoded.len() {
            V1_METADATA_SIZE => (V1_METADATA_SIZE, 1),
            length if length == V1_METADATA_SIZE + 32 => (length, 2),
            length if length == V1_METADATA_SIZE + 33 && matches!(encoded.last(), Some(3 | 4)) => {
                (length - 1, encoded[length - 1])
            }
            _ => anyhow::bail!("invalid snapshot manifest size or format"),
        };
        let digest = Byte32::from_slice(&encoded[..32])?;
        let size = u64::from_le_bytes(encoded[32..40].try_into()?);
        let header = Header::from_slice(&encoded[40..V1_METADATA_SIZE])?.into_view();
        let cells_root = encoded
            .get(V1_METADATA_SIZE..root_end)
            .filter(|bytes| !bytes.is_empty())
            .map(<[u8; 32]>::try_from)
            .transpose()?;
        ensure!(
            (1..=MAX_IMAGE).contains(&size),
            "snapshot size outside allowed bounds"
        );
        Ok(Self {
            header,
            size,
            digest,
            cells_root,
            format,
        })
    }

    fn format(&self) -> u32 {
        u32::from(self.format)
    }

    fn chunks(&self) -> u32 {
        self.size.div_ceil(CHUNK) as u32
    }

    fn chunk_len(&self, index: u32) -> Option<usize> {
        let offset = u64::from(index) * CHUNK;
        (offset < self.size).then(|| (self.size - offset).min(CHUNK) as usize)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            height: self.header.number(),
            format: self.format(),
            chunks: self.chunks(),
            hash: Bytes::copy_from_slice(self.hash().as_slice()),
            metadata: self.encode(),
        }
    }
}

pub struct Archive {
    directory: PathBuf,
    home: PathBuf,
}

pub struct Captured {
    manifest: Manifest,
    image: Option<NamedTempFile>,
    index: Option<cell_proof::Index>,
}

pub struct Incoming {
    manifest: Manifest,
    image: NamedTempFile,
    next_chunk: u32,
}

impl Archive {
    pub fn open(home: &Path) -> Result<Self> {
        let directory = home.join("snapshots");
        fs::create_dir_all(&directory)?;
        Ok(Self {
            directory,
            home: home.into(),
        })
    }

    pub fn active_store(home: &Path) -> Result<PathBuf> {
        let path = home.join(ACTIVE);
        match fs::read_to_string(path) {
            Ok(selected) => {
                ensure!(
                    selected.starts_with("synced-")
                        && selected.len() <= 40
                        && selected
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || c == b'-'),
                    "invalid active database path"
                );
                let selected = home.join(selected);
                ensure!(
                    fs::symlink_metadata(&selected)?.is_dir(),
                    "active database is missing"
                );
                Ok(selected)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(home.join("cells")),
            Err(error) => Err(error.into()),
        }
    }

    pub fn capture(&self, state: &Store, header: HeaderView) -> Result<Captured> {
        let staged = (|| -> Result<_> {
            let mut image = NamedTempFile::new_in(&self.directory)?;
            let mut writer = CappedWriter {
                output: BufWriter::new(image.as_file_mut()),
                remaining: MAX_IMAGE,
            };
            let exported = export(state, &mut writer)?;
            writer.flush()?;
            drop(writer);
            Ok((Some(image), exported))
        })();
        let (image, (digest, size, cell_tree)) = match staged {
            Ok(staged) => staged,
            Err(error) => {
                // A replica can still commit the checkpoint if the archive
                // directory fills while the database remains writable.
                eprintln!(
                    "level=warn event=snapshot_stage_failed reason={:?}",
                    error.to_string()
                );
                (None, export(state, &mut std::io::sink())?)
            }
        };
        let (cells_root, index) =
            cell_tree.map_or((None, None), |(root, index)| (Some(root), Some(index)));
        Ok(Captured {
            manifest: Manifest {
                header,
                digest,
                size,
                cells_root,
                format: state.checkpoint_format,
            },
            image,
            index,
        })
    }

    pub fn publish(&self, captured: Captured) -> Result<()> {
        let manifest = captured.manifest;
        if manifest.size > MAX_IMAGE {
            return Ok(());
        }
        let Some(image) = captured.image else {
            return Ok(());
        };
        let height = manifest.header.number();
        if let (Some(index), Some(root)) = (captured.index, manifest.cells_root) {
            let mut file = NamedTempFile::new_in(&self.directory)?;
            let mut writer = BufWriter::new(file.as_file_mut());
            index.write(&root, &mut writer)?;
            writer.flush()?;
            drop(writer);
            file.as_file().sync_all()?;
            file.persist(self.cells(height))?;
        }
        image.as_file().sync_all()?;
        image.persist(self.data(height))?;
        let mut file = NamedTempFile::new_in(&self.directory)?;
        file.write_all(&manifest.encode())?;
        file.as_file().sync_all()?;
        file.persist(self.metadata(height))?;
        File::open(&self.directory)?.sync_all()?;
        for old in self.heights()?.into_iter().skip(2) {
            fs::remove_file(self.metadata(old))?;
            fs::remove_file(self.data(old))?;
            match fs::remove_file(self.cells(old)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub fn list(&self, tip: u64) -> Result<Vec<Snapshot>> {
        self.heights()?
            .into_iter()
            .filter(|height| *height <= tip)
            .take(2)
            .map(|height| {
                let manifest = self.manifest(height)?;
                Ok(manifest.snapshot())
            })
            .collect()
    }

    pub fn chunk(&self, height: u64, format: u32, index: u32) -> Result<Bytes> {
        let manifest = self.manifest(height)?;
        ensure!(format == manifest.format(), "unsupported snapshot format");
        let len = manifest
            .chunk_len(index)
            .context("snapshot chunk out of bounds")?;
        let mut file = File::open(self.data(height))?;
        file.seek(SeekFrom::Start(u64::from(index) * CHUNK))?;
        let mut chunk = vec![0; len];
        file.read_exact(&mut chunk)?;
        Ok(chunk.into())
    }

    pub fn cell_proof(&self, height: u64, point: &OutPoint) -> Result<Value> {
        let manifest = self
            .manifest(height)
            .context("checkpoint archive is unavailable")?;
        let root = manifest
            .cells_root
            .context("checkpoint does not commit Live Cell proofs")?;
        let index = File::open(self.cells(height))?;
        let image = File::open(self.data(height))?;
        let Some(membership) = cell_proof::prove(&index, &image, manifest.size, &root, point)?
        else {
            return Ok(Value::Null);
        };
        Ok(json!({
            "format": manifest.format(),
            "height": height,
            "ckb_header_hash": hex::encode(manifest.header.hash().as_slice()),
            "image_size": manifest.size,
            "image_digest": hex::encode(manifest.digest.as_slice()),
            "cells_root": hex::encode(root),
            "membership": membership,
        }))
    }

    pub fn incoming(
        &self,
        spec: &Genesis,
        offer: Snapshot,
        trusted_app_hash: &[u8],
    ) -> Result<Incoming> {
        ensure!(
            offer.format == u32::from(spec.checkpoint_format()) && spec.checkpoint(offer.height),
            "unsupported checkpoint format or height"
        );
        let manifest = Manifest::decode(&offer.metadata)?;
        ensure!(
            offer.format == manifest.format()
                && manifest.header.number() == offer.height
                && manifest.chunks() == offer.chunks,
            "snapshot height or chunk count mismatch"
        );
        let hash = manifest.hash();
        ensure!(
            hash.as_slice() == trusted_app_hash && hash.as_slice() == offer.hash,
            "snapshot manifest is unauthenticated"
        );
        let image = NamedTempFile::new_in(&self.directory)?;
        Ok(Incoming {
            manifest,
            image,
            next_chunk: 0,
        })
    }

    pub fn install(
        &self,
        incoming: Incoming,
        spec: &Genesis,
        engine: &Engine,
        rpc_index: bool,
        compact_transaction_history: bool,
    ) -> Result<Store> {
        let manifest = incoming.manifest;
        let mut file = BufReader::new(incoming.image.reopen()?);
        let mut hash = new_blake2b();
        let copied = std::io::copy(
            &mut file,
            &mut DigestWriter(&mut |input| hash.update(input)),
        )?;
        let mut digest = [0; 32];
        hash.finalize(&mut digest);
        ensure!(
            digest == manifest.digest.as_slice() && copied == manifest.size,
            "invalid certified snapshot digest"
        );
        file.seek(SeekFrom::Start(0))?;
        let temp = tempfile::Builder::new()
            .prefix("synced-")
            .tempdir_in(&self.home)?;
        let mut cells = manifest.cells_root.map(|_| cell_proof::Builder::default());
        let mut store = Store::restore(temp.path(), spec, &mut file, |offset, key, value| {
            if let Some(builder) = cells.as_mut() {
                builder.add(offset, key, value)?;
            }
            Ok(())
        })?;
        let index = if let Some(cells) = cells {
            let (root, index) = cells.finish();
            ensure!(
                Some(root) == manifest.cells_root,
                "restored Cells differ from certified checkpoint root"
            );
            Some(index)
        } else {
            None
        };
        let header = manifest.header.clone();
        let block = engine.verify(&store, header.number(), header.timestamp(), Vec::new())?;
        ensure!(
            block.header == header,
            "checkpoint header differs from restored state"
        );
        store.commit_with_hash(header, &[], block.cells, manifest.hash())?;
        if compact_transaction_history {
            store.prune_ordinary_history()?;
        }
        if rpc_index {
            store.enable_rpc_index()?;
        }
        store.flush()?;
        self.activate(temp)?;
        if index.is_some()
            && let Err(error) = self.publish(Captured {
                manifest,
                image: Some(incoming.image),
                index,
            })
        {
            eprintln!(
                "level=warn event=snapshot_publish_failed reason={:?}",
                error.to_string()
            );
        }
        Ok(store)
    }

    fn activate(&self, temporary: TempDir) -> Result<()> {
        let name = temporary
            .path()
            .file_name()
            .context("missing staged database name")?
            .to_owned();
        let mut active = NamedTempFile::new_in(&self.home)?;
        active.write_all(name.as_encoded_bytes())?;
        active.as_file().sync_all()?;
        // Persist the database directory before making it discoverable at startup.
        File::open(temporary.path())?.sync_all()?;
        File::open(&self.home)?.sync_all()?;
        let _database = temporary.keep();
        active.persist(self.home.join(ACTIVE))?;
        File::open(&self.home)?.sync_all()?;
        Ok(())
    }

    fn heights(&self) -> Result<Vec<u64>> {
        let mut heights = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            if let Some(height) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".meta"))
                .and_then(|height| height.parse::<u64>().ok())
            {
                heights.push(height);
            }
        }
        heights.sort_unstable_by(|a, b| b.cmp(a));
        Ok(heights)
    }

    fn manifest(&self, height: u64) -> Result<Manifest> {
        Manifest::decode(&fs::read(self.metadata(height))?)
    }

    fn data(&self, height: u64) -> PathBuf {
        self.directory.join(format!("{height}.bin"))
    }

    fn metadata(&self, height: u64) -> PathBuf {
        self.directory.join(format!("{height}.meta"))
    }

    fn cells(&self, height: u64) -> PathBuf {
        self.directory.join(format!("{height}.cells"))
    }
}

type CellTree = Option<(cell_proof::Hash, cell_proof::Index)>;

fn export(state: &Store, writer: &mut impl Write) -> Result<(Byte32, u64, CellTree)> {
    let mut cells = (state.checkpoint_format >= 2).then(cell_proof::Builder::default);
    let (digest, size) = state.export_with_cells(writer, |offset, key, value| {
        if let Some(builder) = cells.as_mut() {
            builder.add(offset, key, value)?;
        }
        Ok(())
    })?;
    Ok((digest, size, cells.map(cell_proof::Builder::finish)))
}

impl Captured {
    pub fn app_hash(&self) -> Byte32 {
        self.manifest.hash()
    }

    pub fn image_size(&self) -> u64 {
        self.manifest.size
    }
}

impl Incoming {
    pub fn height(&self) -> u64 {
        self.manifest.header.number()
    }

    pub fn apply(&mut self, index: u32, chunk: &[u8]) -> Result<bool> {
        ensure!(
            index == self.next_chunk,
            "snapshot chunks must arrive in order"
        );
        ensure!(
            self.manifest.chunk_len(index) == Some(chunk.len()),
            "invalid snapshot chunk size"
        );
        self.image.write_all(chunk)?;
        self.next_chunk += 1;
        Ok(self.next_chunk == self.manifest.chunks())
    }
}

struct DigestWriter<'a>(&'a mut dyn FnMut(&[u8]));

struct CappedWriter<W> {
    output: W,
    remaining: u64,
}

impl<W: Write> Write for CappedWriter<W> {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        let available = (input.len() as u64).min(self.remaining) as usize;
        self.output.write_all(&input[..available])?;
        self.remaining -= available as u64;
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.output.flush()
    }
}

impl Write for DigestWriter<'_> {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        self.0(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{genesis::script_consensus, tests::payments};

    #[test]
    fn rebuilds_certified_image_and_rejects_a_corrupt_chunk_before_installing() {
        let mut fixture = payments(1);
        let spec = &mut fixture.genesis;
        spec.checkpoint_interval = 4;
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        let origin = Archive::open(source.path()).unwrap();
        let receiver = Archive::open(destination.path()).unwrap();
        let engine = Engine::new(script_consensus().unwrap(), spec.epoch_length, 2).unwrap();
        let mut state = Store::open(&source.path().join("cells"), spec).unwrap();
        for height in 1..4 {
            let block = engine
                .verify(&state, height, spec.timestamp_ms + height, Vec::new())
                .unwrap();
            state.commit(block.header, &[], block.cells).unwrap();
        }
        let block = engine
            .verify(&state, 4, spec.timestamp_ms + 4, Vec::new())
            .unwrap();
        let captured = origin.capture(&state, block.header.clone()).unwrap();
        let root = captured.app_hash();
        state
            .commit_with_hash(block.header, &[], block.cells, root.clone())
            .unwrap();
        origin.publish(captured).unwrap();
        let snapshot = origin.list(4).unwrap().remove(0);
        assert!(receiver.incoming(spec, snapshot.clone(), &[0; 32]).is_err());
        let mut corrupt = receiver
            .incoming(spec, snapshot.clone(), root.as_slice())
            .unwrap();
        let mut valid = receiver
            .incoming(spec, snapshot.clone(), root.as_slice())
            .unwrap();
        for index in 0..snapshot.chunks {
            let bytes = origin.chunk(4, 1, index).unwrap();
            let mut changed = bytes.to_vec();
            changed[0] ^= 1;
            assert_eq!(
                valid.apply(index, &bytes).unwrap(),
                index + 1 == snapshot.chunks
            );
            assert_eq!(
                corrupt.apply(index, &changed).unwrap(),
                index + 1 == snapshot.chunks
            );
        }
        assert!(
            receiver
                .install(corrupt, spec, &engine, true, false)
                .is_err()
        );
        assert!(!destination.path().join(ACTIVE).exists());
        let restored = receiver.install(valid, spec, &engine, true, false).unwrap();
        assert_eq!(restored.tip, state.tip);
        assert_eq!(restored.app_hash, root);
        assert_eq!(
            restored.rpc_index().unwrap().header_hash(4).unwrap(),
            Some(restored.tip.hash())
        );
        assert_eq!(
            restored.export(&mut std::io::sink()).unwrap(),
            state.export(&mut std::io::sink()).unwrap()
        );
        drop(restored);
        let recovered =
            Store::open(&Archive::active_store(destination.path()).unwrap(), spec).unwrap();
        assert_eq!(recovered.tip.number(), 4);
        assert_eq!(recovered.app_hash, root);
    }
}
