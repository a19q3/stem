//! Bounded local timing evidence. The consensus loop never waits for the trace.
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, RecvTimeoutError, SyncSender},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::Result;
use serde::Serialize;
use stem_stake::jolteon::Point;

const CAPACITY: usize = 4096;
const MAX_EVENTS: u64 = 262_144;

#[derive(Clone, Copy)]
pub(super) struct Tag {
    pub epoch: u64,
    pub point: Point,
    pub kind: u32,
    pub peer: u16,
}

#[derive(Clone, Copy)]
pub(super) struct Reception {
    pub read: Instant,
    pub queued: Instant,
    pub handling: Instant,
    pub bytes: usize,
}

impl Reception {
    pub(super) fn record(self, trace: &Trace, tag: Tag) {
        trace.wire(tag, "rx_read_complete", self.read, self.bytes);
        trace.wire(tag, "rx_inbox_enqueue", self.queued, self.bytes);
        trace.wire(tag, "rx_handler_begin", self.handling, self.bytes);
    }
}

struct Event {
    time: Instant,
    epoch: u64,
    point: Point,
    stage: &'static str,
    kind: Option<u32>,
    peer: Option<u16>,
    bytes: Option<usize>,
}

#[derive(Default)]
struct Counters {
    submitted: AtomicU64,
    written: AtomicU64,
    dropped: AtomicU64,
}

#[derive(Serialize)]
pub(super) struct Status {
    submitted: u64,
    written: u64,
    dropped: u64,
}

pub(super) struct Trace {
    events: SyncSender<Event>,
    counters: Arc<Counters>,
}

impl Trace {
    pub(super) fn open(path: &Path, replica: u16) -> Result<Self> {
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        let mut output = BufWriter::with_capacity(64 * 1024, file);
        let started = Instant::now();
        let unix_ns = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos() as u64;
        let bracket_ns = started.elapsed().as_nanos() as u64;
        let counters = Arc::new(Counters::default());
        let writer = Arc::clone(&counters);
        let (events, mut_events) = mpsc::sync_channel::<Event>(CAPACITY);
        writeln!(
            output,
            "{}",
            serde_json::json!({"schema":1,"replica":replica,"unix_ns":unix_ns,"clock_bracket_ns":bracket_ns})
        )?;
        output.flush()?;
        thread::Builder::new()
            .name("stem-qc-trace".into())
            .spawn(move || -> std::io::Result<()> {
                let mut last_flush = Instant::now();
                loop {
                    match mut_events.recv_timeout(Duration::from_millis(100)) {
                        Ok(event) => {
                            writeln!(
                                output,
                                "{}",
                                serde_json::json!({
                                    "t_ns": event.time.saturating_duration_since(started).as_nanos() as u64,
                                    "epoch": event.epoch,
                                    "view": event.point.view,
                                    "parent_view": event.point.parent_view,
                                    "header": hex::encode(event.point.header),
                                    "stage": event.stage,
                                    "kind": event.kind,
                                    "peer": event.peer,
                                    "bytes": event.bytes,
                                })
                            )?;
                            writer.written.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => return output.flush(),
                    }
                    if last_flush.elapsed() >= Duration::from_millis(100) {
                        output.flush()?;
                        last_flush = Instant::now();
                    }
                }
            })?;
        Ok(Self { events, counters })
    }

    fn record(&self, event: Event) {
        let previous = self.counters.submitted.fetch_add(1, Ordering::Relaxed);
        if previous >= MAX_EVENTS || self.events.try_send(event).is_err() {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(super) fn local(
        &self,
        epoch: u64,
        point: Point,
        stage: &'static str,
        time: Instant,
        peer: Option<u16>,
    ) {
        self.record(Event {
            time,
            epoch,
            point,
            stage,
            peer,
            kind: None,
            bytes: None,
        });
    }

    pub(super) fn wire(&self, tag: Tag, stage: &'static str, time: Instant, bytes: usize) {
        self.record(Event {
            time,
            epoch: tag.epoch,
            point: tag.point,
            stage,
            peer: Some(tag.peer),
            kind: Some(tag.kind),
            bytes: Some(bytes),
        });
    }

    pub(super) fn status(&self) -> Status {
        Status {
            submitted: self.counters.submitted.load(Ordering::Relaxed),
            written: self.counters.written.load(Ordering::Relaxed),
            dropped: self.counters.dropped.load(Ordering::Relaxed),
        }
    }
}
