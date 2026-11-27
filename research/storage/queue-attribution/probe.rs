//! Research-only span recorder for the replica's single event loop.
use std::{
    fs::OpenOptions,
    io::{self, BufWriter, Write},
    time::Instant,
};

pub(super) struct Probe {
    origin: Instant,
    sink: BufWriter<std::fs::File>,
}

impl Probe {
    pub(super) fn open() -> anyhow::Result<Self> {
        let origin = Instant::now();
        let path = std::env::var_os("STEM_QUEUE_TRACE")
            .ok_or_else(|| anyhow::anyhow!("set STEM_QUEUE_TRACE to a fresh per-node path"))?;
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Self {
            origin,
            sink: BufWriter::with_capacity(64 * 1024, file),
        })
    }

    pub(super) fn span(&mut self, kind: u8, start: Instant, end: Instant) -> io::Result<()> {
        let mut record = [0; 17];
        record[0] = kind;
        record[1..9].copy_from_slice(&((start - self.origin).as_nanos() as u64).to_le_bytes());
        record[9..].copy_from_slice(&((end - self.origin).as_nanos() as u64).to_le_bytes());
        self.sink.write_all(&record)
    }

    pub(super) fn flush(&mut self) -> io::Result<()> {
        self.sink.flush()
    }
}
