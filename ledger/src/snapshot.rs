//! The existing state snapshot is a Strand sequence of an execution header,
//! frontier, header suffix and namespace-keyed records. Stream the vectors so
//! an archive does not need a second buffer spanning the whole snapshot.
use std::io::{Read, Write};

use anyhow::{Context, Result, ensure};

use crate::{
    block::Header,
    patricia::{Key, Namespace},
    strand::prelude::Reader,
    wire,
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

const HEADER_BYTES: usize = stem_stake::finality::NATIVE_HEADER_BYTES;

impl StateSnapshot {
    pub fn encode(&self, mut out: impl Write) -> Result<()> {
        out.write_all(&self.header.encode())?;
        sized_write(&mut out, &self.frontier)?;
        wire::SnapshotHeaderVec::write_count(self.headers.len(), &mut out)?;
        for header in &self.headers {
            out.write_all(&header.encode())?;
        }
        wire::SnapshotRecordVec::write_count(self.records.len(), &mut out)?;
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
            let location = wire::SnapshotRecordKeyReader::from_slice(&prefix)?;
            let namespace = match location.namespace().as_slice()[0] {
                0 => Namespace::Cells,
                1 => Namespace::System,
                _ => anyhow::bail!("invalid snapshot state namespace"),
            };
            let key = location.key().as_slice().try_into()?;
            let value = sized_read(&mut input, &mut remaining, crate::MAX_CELL_BYTES + 84)?;
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
    wire::Bytes::write_count(bytes.len(), out)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_snapshot_matches_legacy_file_and_generated_schema() -> Result<()> {
        let header = Header::decode(&[0; HEADER_BYTES])?;
        let snapshot = StateSnapshot {
            header: header.clone(),
            frontier: vec![1, 2, 3],
            headers: vec![header],
            records: vec![
                (Namespace::Cells, [5; 32], vec![]),
                (Namespace::System, [7; 32], vec![9; 257]),
            ],
        };
        let mut old = vec![0; HEADER_BYTES];
        old.extend_from_slice(&[3, 0, 0, 0, 1, 2, 3, 1, 0, 0, 0]);
        old.extend_from_slice(&[0; HEADER_BYTES]);
        old.extend_from_slice(&2u32.to_le_bytes());
        for (namespace, key, bytes) in &snapshot.records {
            old.push(namespace.tag());
            old.extend_from_slice(key);
            old.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            old.extend_from_slice(bytes);
        }

        let mut encoded = Vec::new();
        snapshot.encode(&mut encoded)?;
        assert_eq!(encoded, old);
        let typed = wire::StateSnapshotReader::from_slice(&old)?;
        assert_eq!(typed.frontier().raw_data(), [1, 2, 3]);
        assert_eq!(typed.headers().len(), 1);
        let mut records = typed.records().iter();
        assert!(records.next().unwrap().value().raw_data().is_empty());
        assert_eq!(records.next().unwrap().location().key().as_slice(), [7; 32]);
        let rebuilt = StateSnapshot::decode(old.as_slice(), old.len() as u64)?;
        assert_eq!(rebuilt.records, snapshot.records);
        assert_eq!(rebuilt.headers, snapshot.headers);

        let mut trailing = old.clone();
        trailing.push(0);
        for invalid in [&old[..old.len() - 1], trailing.as_slice()] {
            assert!(wire::StateSnapshot::from_slice(invalid).is_err());
            assert!(StateSnapshot::decode(invalid, invalid.len() as u64).is_err());
        }
        Ok(())
    }
}
