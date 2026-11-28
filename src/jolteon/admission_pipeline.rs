//! Bounded, client-only admission experiment. Only Replica updates the mempool.
use std::{collections::HashMap, sync::Arc, thread, time::Instant};

use anyhow::{Context, Result};
use bytes::Bytes;
use ckb_types::prelude::*;
use stem_ledger::Transaction;
use stem_stake::Hash32;
use tokio::sync::{mpsc, oneshot};

use super::{AdmissionPipelineMetrics, Replica};
use crate::native_store::{PinnedView, Store};

const MAX_SUBMISSIONS: usize = 128;
const MAX_BYTES: usize = 4 * 1024 * 1024;

type Reply = (oneshot::Sender<Vec<u8>>, Instant);

struct Pending {
    raw: Bytes,
    replies: Vec<Reply>,
}

struct Job {
    id: Hash32,
    tip: Hash32,
    view: Arc<PinnedView>,
    raw: Bytes,
}

pub(super) struct Completed {
    id: Hash32,
    tip: Hash32,
    checked: Result<Option<Transaction>>,
    verification_us: u64,
}

pub(super) struct Pipeline {
    jobs: mpsc::Sender<Job>,
    pending: HashMap<Hash32, Pending>,
    view: Option<(Hash32, Arc<PinnedView>)>,
    bytes: usize,
    replies: usize,
    metrics: AdmissionPipelineMetrics,
}

impl Pipeline {
    pub(super) fn start() -> Result<(Self, mpsc::Receiver<Completed>)> {
        let (jobs, mut work) = mpsc::channel::<Job>(MAX_SUBMISSIONS);
        let (send, done) = mpsc::channel(MAX_SUBMISSIONS);
        thread::Builder::new()
            .name("stem-admission".into())
            .spawn(move || {
                while let Some(job) = work.blocking_recv() {
                    let started = Instant::now();
                    let checked = job.view.check(job.raw);
                    let verification_us =
                        started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
                    if send
                        .blocking_send(Completed {
                            id: job.id,
                            tip: job.tip,
                            checked,
                            verification_us,
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
                pending: HashMap::new(),
                view: None,
                bytes: 0,
                replies: 0,
                metrics: AdmissionPipelineMetrics::default(),
            },
            done,
        ))
    }

    fn has_room(&self, bytes: usize) -> bool {
        self.replies < MAX_SUBMISSIONS && self.bytes.saturating_add(bytes) <= MAX_BYTES
    }

    fn queue(&mut self, store: &Store, id: Hash32, raw: Bytes, reply: Reply) -> Option<Reply> {
        let new_bytes = if self.pending.contains_key(&id) {
            0
        } else {
            raw.len()
        };
        if !self.has_room(new_bytes) {
            self.metrics.full = self.metrics.full.saturating_add(1);
            return Some(reply);
        }
        if let Some(pending) = self.pending.get_mut(&id) {
            pending.replies.push(reply);
            self.replies += 1;
            self.metrics.coalesced = self.metrics.coalesced.saturating_add(1);
            return None;
        }
        let tip = store.tip().id(store.network()).unpack();
        if self.view.as_ref().is_none_or(|(old, _)| *old != tip) {
            self.view = Some((tip, Arc::new(store.pinned_view())));
        }
        let view = Arc::clone(&self.view.as_ref().expect("current admission view").1);
        if self
            .jobs
            .try_send(Job {
                id,
                tip,
                view,
                raw: raw.clone(),
            })
            .is_err()
        {
            self.metrics.full = self.metrics.full.saturating_add(1);
            return Some(reply);
        }
        self.bytes += raw.len();
        self.replies += 1;
        self.pending.insert(
            id,
            Pending {
                raw,
                replies: vec![reply],
            },
        );
        self.metrics.dispatched = self.metrics.dispatched.saturating_add(1);
        None
    }

    pub(super) fn status(&self) -> AdmissionPipelineMetrics {
        AdmissionPipelineMetrics {
            pending_replies: self.replies,
            pending_bytes: self.bytes,
            ..self.metrics
        }
    }
}

impl Replica {
    pub(super) fn queue_client_admission(
        &mut self,
        raw: Bytes,
        received: Instant,
        reply: oneshot::Sender<Vec<u8>>,
    ) -> Result<()> {
        let started = Instant::now();
        self.admission
            .client_queue
            .observe(started.duration_since(received));
        self.admission.client_requests = self.admission.client_requests.saturating_add(1);

        let id = ckb_hash::blake2b_256(&raw);
        if self.mempool.contains(&id) {
            let response = self.admitted_client(id, raw);
            return self.admission_reply((reply, started), Ok(response));
        }
        if let Err(error) = self.admission_capacity(raw.len(), true) {
            return self.admission_reply((reply, started), Err(error));
        }
        if let Some(reply) = self.pipeline.queue(&self.store, id, raw, (reply, started)) {
            self.admission_reply(
                reply,
                Err(anyhow::anyhow!("admission pipeline capacity exceeded")),
            )?;
        }
        Ok(())
    }

    pub(super) fn complete_client_admission(&mut self, completed: Completed) -> Result<()> {
        let Completed {
            id,
            tip,
            checked,
            verification_us,
        } = completed;
        let pending = self
            .pipeline
            .pending
            .remove(&id)
            .context("unexpected admission completion")?;
        self.pipeline.bytes -= pending.raw.len();
        self.pipeline.replies -= pending.replies.len();
        self.record_admission_check(true, verification_us);
        if matches!(checked, Ok(None)) {
            self.pipeline.metrics.snapshot_misses =
                self.pipeline.metrics.snapshot_misses.saturating_add(1);
        }
        let result = self.finish_client_admission(id, tip, checked, pending.raw);
        let result =
            result.unwrap_or_else(|error| serde_json::json!({"error": format!("{error:#}")}));
        let bytes = serde_json::to_vec(&result)?;
        for (reply, started) in pending.replies {
            self.admission.client_service.observe(started.elapsed());
            let _ = reply.send(bytes.clone());
        }
        Ok(())
    }

    fn finish_client_admission(
        &mut self,
        id: Hash32,
        tip: Hash32,
        checked: Result<Option<Transaction>>,
        raw: Bytes,
    ) -> Result<serde_json::Value> {
        if !self.mempool.contains(&id) {
            self.admission_capacity(raw.len(), true)?;
            let current_tip: Hash32 = self.store.tip().id(self.store.network()).unpack();
            let tx = if tip == current_tip && !matches!(checked, Ok(None)) {
                checked?.context("missing transaction from admission snapshot")?
            } else {
                self.pipeline.metrics.rechecked = self.pipeline.metrics.rechecked.saturating_add(1);
                let tx = self.decode_admission(&raw, id, true)?;
                self.validate_admission(&tx, true, Some(id))?;
                tx
            };
            self.mempool.insert(id, tx)?;
        }
        Ok(self.admitted_client(id, raw))
    }

    fn admission_reply(
        &mut self,
        (reply, started): Reply,
        result: Result<serde_json::Value>,
    ) -> Result<()> {
        let result =
            result.unwrap_or_else(|error| serde_json::json!({"error": format!("{error:#}")}));
        let bytes = serde_json::to_vec(&result)?;
        self.admission.client_service.observe(started.elapsed());
        let _ = reply.send(bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_bytes_and_waiting_duplicates_have_independent_limits() -> Result<()> {
        let (mut pipeline, _done) = Pipeline::start()?;
        assert!(pipeline.has_room(MAX_BYTES));
        assert!(!pipeline.has_room(MAX_BYTES + 1));
        pipeline.bytes = MAX_BYTES;
        assert!(pipeline.has_room(0));
        assert!(!pipeline.has_room(1));
        pipeline.replies = MAX_SUBMISSIONS;
        assert!(!pipeline.has_room(0));
        Ok(())
    }
}
