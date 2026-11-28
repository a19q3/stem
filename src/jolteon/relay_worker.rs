//! Bounded background validation for authenticated relay frames.
use std::{
    collections::HashSet,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use ckb_types::prelude::*;
use stem_ledger::Transaction;
use stem_stake::Hash32;
use tokio::sync::mpsc;

use super::{LatencyHistogram, Replica};
use crate::native_store::{PinnedView, Store};

const MAX_JOBS: usize = 128;
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, serde::Deserialize, serde::Serialize)]
pub struct Metrics {
    pub queued: u64,
    pub completed: u64,
    pub coalesced: u64,
    pub inline_full: u64,
    pub snapshot_misses: u64,
    pub rechecked: u64,
    pub redundant_completions: u64,
    pub obsolete_epoch: u64,
    pub capacity_rejections: u64,
    pub admitted: u64,
    pub pending_jobs: usize,
    pub pending_bytes: usize,
    pub wait: LatencyHistogram,
    pub service: LatencyHistogram,
}

struct Job {
    id: Hash32,
    epoch: u64,
    header: Hash32,
    snapshot: Arc<PinnedView>,
    tx: Transaction,
    queued: Instant,
}

pub(super) struct Completed {
    job: Job,
    checked: Result<Option<()>>,
    wait: Duration,
    service: Duration,
}

pub(super) enum Schedule {
    Queued,
    Inline(Transaction),
}

pub(super) struct Worker {
    jobs: mpsc::Sender<Job>,
    pending: HashSet<Hash32>,
    bytes: usize,
    snapshot: Option<(Hash32, Arc<PinnedView>)>,
    metrics: Metrics,
}

impl Worker {
    pub(super) fn start() -> Result<(Self, mpsc::Receiver<Completed>)> {
        let (jobs, mut work) = mpsc::channel::<Job>(MAX_JOBS);
        let (send, done) = mpsc::channel(MAX_JOBS);
        thread::Builder::new()
            .name("stem-relay".into())
            .spawn(move || {
                while let Some(job) = work.blocking_recv() {
                    let start = Instant::now();
                    let checked = job.snapshot.check_relay(&job.tx);
                    let wait = start.duration_since(job.queued);
                    let service = start.elapsed();
                    if send
                        .blocking_send(Completed {
                            job,
                            checked,
                            wait,
                            service,
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok((
            Self {
                jobs,
                pending: HashSet::new(),
                bytes: 0,
                snapshot: None,
                metrics: Metrics::default(),
            },
            done,
        ))
    }

    pub(super) fn coalesce(&mut self, id: &Hash32) -> bool {
        if !self.pending.contains(id) {
            return false;
        }
        self.metrics.coalesced = self.metrics.coalesced.saturating_add(1);
        true
    }

    fn has_room(&self, bytes: usize) -> bool {
        self.pending.len() < MAX_JOBS && self.bytes.saturating_add(bytes) <= MAX_BYTES
    }

    pub(super) fn schedule(
        &mut self,
        store: &Store,
        epoch: u64,
        id: Hash32,
        tx: Transaction,
    ) -> Result<Schedule> {
        let bytes = tx.as_slice().len();
        if !self.has_room(bytes) {
            self.metrics.inline_full = self.metrics.inline_full.saturating_add(1);
            return Ok(Schedule::Inline(tx));
        }
        let header: Hash32 = store.tip().id(store.network()).unpack();
        if self
            .snapshot
            .as_ref()
            .is_none_or(|(bound, _)| *bound != header)
        {
            self.snapshot = Some((header, Arc::new(store.pinned_view())));
        }
        let snapshot = Arc::clone(&self.snapshot.as_ref().expect("relay snapshot").1);
        self.jobs
            .try_send(Job {
                id,
                epoch,
                header,
                snapshot,
                tx,
                queued: Instant::now(),
            })
            .map_err(|_| anyhow::anyhow!("relay worker queue lost its bounded slot"))?;
        self.pending.insert(id);
        self.bytes += bytes;
        self.metrics.queued = self.metrics.queued.saturating_add(1);
        Ok(Schedule::Queued)
    }

    pub(super) fn status(&self) -> Metrics {
        Metrics {
            pending_jobs: self.pending.len(),
            pending_bytes: self.bytes,
            ..self.metrics
        }
    }
}

impl Replica {
    pub(super) fn complete_relay(&mut self, done: Completed) -> Result<()> {
        let Completed {
            job,
            checked,
            wait,
            service,
        } = done;
        let worker = self
            .relay_worker
            .as_mut()
            .context("unexpected relay result")?;
        ensure!(worker.pending.remove(&job.id), "untracked relay result");
        let bytes = job.tx.as_slice().len();
        worker.bytes -= bytes;
        let metrics = &mut worker.metrics;
        metrics.completed = metrics.completed.saturating_add(1);
        metrics.wait.observe(wait);
        metrics.service.observe(service);
        let missed = matches!(checked, Ok(None));
        if missed {
            metrics.snapshot_misses = metrics.snapshot_misses.saturating_add(1);
        }
        if job.epoch != self.committee.epoch() {
            metrics.obsolete_epoch = metrics.obsolete_epoch.saturating_add(1);
        } else if self.mempool.contains(&job.id) {
            metrics.redundant_completions = metrics.redundant_completions.saturating_add(1);
        }
        if !missed {
            self.record_admission_check(false, service.as_micros().try_into().unwrap_or(u64::MAX));
        }
        if job.epoch != self.committee.epoch() || self.mempool.contains(&job.id) {
            return Ok(());
        }
        if self.admission_capacity(bytes, false).is_err() {
            self.relay_worker_metrics().capacity_rejections = self
                .relay_worker_metrics()
                .capacity_rejections
                .saturating_add(1);
            self.rejected = self.rejected.saturating_add(1);
            return Ok(());
        }
        let current: Hash32 = self.store.tip().id(self.store.network()).unpack();
        let valid = if missed || current != job.header {
            let metrics = self.relay_worker_metrics();
            metrics.rechecked = metrics.rechecked.saturating_add(1);
            self.validate_admission(&job.tx, false, None)
        } else {
            checked.map(|_| ())
        };
        if valid
            .and_then(|()| self.mempool.insert(job.id, job.tx))
            .is_err()
        {
            self.rejected = self.rejected.saturating_add(1);
        } else {
            let metrics = self.relay_worker_metrics();
            metrics.admitted = metrics.admitted.saturating_add(1);
        }
        Ok(())
    }

    fn relay_worker_metrics(&mut self) -> &mut Metrics {
        &mut self.relay_worker.as_mut().expect("relay worker").metrics
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outstanding_jobs_and_bytes_are_bounded_independently() -> Result<()> {
        let (mut worker, _done) = Worker::start()?;
        assert!(worker.has_room(MAX_BYTES));
        assert!(!worker.has_room(MAX_BYTES + 1));
        worker.pending.extend((0..MAX_JOBS).map(|i| [i as u8; 32]));
        assert!(!worker.has_room(1));
        assert!(worker.coalesce(&[0; 32]));
        assert!(!worker.coalesce(&[255; 32]));
        assert_eq!(worker.status().coalesced, 1);
        worker.pending.clear();
        worker.bytes = MAX_BYTES;
        assert!(!worker.has_room(1));
        Ok(())
    }
}
