//! Certified writes own the database writer until durability is acknowledged.
use std::{
    ops::Range,
    sync::Arc,
    thread,
    time::Instant,
};

use anyhow::{Context, Result, anyhow};
use stem_stake::jolteon::Point;
use tokio::sync::{mpsc, oneshot};

use super::{CommitWindow, Incoming, Replica, can_vote_at, record_commit_span, transport::Inbox, wire};
use crate::native_store::{CertifiedWriteCache, CertifiedWriteTiming, ConsensusRecord, Prepared};
#[cfg(feature = "jolteon-prebuilt-state-write")]
use {
    crate::native_store::Store,
    std::sync::{
        Weak,
        atomic::{AtomicBool, Ordering},
    },
};

type Persist = Box<dyn FnOnce(&mut CertifiedWriteCache) -> Result<CertifiedWriteTiming> + Send>;

struct Write {
    persist: Persist,
    done: oneshot::Sender<Result<CertifiedWriteTiming>>,
    window: Option<CommitWindow>,
}

enum Job {
    Commit(Write),
    #[cfg(feature = "jolteon-prebuilt-state-write")]
    Prebuild {
        build: Box<dyn FnOnce(&mut CertifiedWriteCache) + Send>,
        pending: Arc<AtomicBool>,
    },
}

pub(super) struct Writer {
    sender: mpsc::Sender<Job>,
    #[cfg(feature = "jolteon-prebuilt-state-write")]
    prebuilding: Arc<AtomicBool>,
    #[cfg(feature = "jolteon-prebuilt-state-write")]
    last_prebuilt: Weak<Prepared>,
}

impl Writer {
    pub(super) fn start() -> Result<Self> {
        // Always leave room for the certified write even when the one
        // optional prebuild is queued.
        let capacity = 1 + usize::from(cfg!(feature = "jolteon-prebuilt-state-write"));
        let (sender, mut jobs) = mpsc::channel::<Job>(capacity);
        thread::Builder::new().name("stem-commit".into()).spawn(move || {
            let mut cache = CertifiedWriteCache::default();
            while let Some(job) = jobs.blocking_recv() {
                let job = match job {
                    Job::Commit(write) => write,
                    #[cfg(feature = "jolteon-prebuilt-state-write")]
                    Job::Prebuild { build, pending } => {
                        // At most one prebuild is outstanding. A queued
                        // certified write takes precedence over starting it.
                        if jobs.is_empty() {
                            build(&mut cache);
                        }
                        pending.store(false, Ordering::Release);
                        continue;
                    }
                };
                let start = Instant::now();
                let result = (job.persist)(&mut cache);
                let elapsed = start.elapsed();
                if let Some(window) = job.window {
                    record_commit_span(&window, (start, elapsed));
                }
                if job.done.send(result).is_err() {
                    break;
                }
            }
        })?;
        Ok(Self {
            sender,
            #[cfg(feature = "jolteon-prebuilt-state-write")]
            prebuilding: Arc::default(),
            #[cfg(feature = "jolteon-prebuilt-state-write")]
            last_prebuilt: Weak::new(),
        })
    }

    #[cfg(feature = "jolteon-prebuilt-state-write")]
    fn prebuild(&mut self, store: &Store, prepared: &Arc<Prepared>) -> bool {
        let candidate = Arc::downgrade(prepared);
        if self.last_prebuilt.ptr_eq(&candidate) || self.prebuilding.load(Ordering::Acquire) {
            return false;
        }
        let Some(build) = store.prebuild_certified_state(Arc::clone(prepared)) else {
            return false;
        };
        self.prebuilding.store(true, Ordering::Relaxed);
        let job = Job::Prebuild {
            build: Box::new(build),
            pending: Arc::clone(&self.prebuilding),
        };
        if self.sender.try_send(job).is_err() {
            self.prebuilding.store(false, Ordering::Relaxed);
            return false;
        }
        self.last_prebuilt = candidate;
        true
    }

    fn submit(
        &self,
        persist: impl FnOnce(&mut CertifiedWriteCache) -> Result<CertifiedWriteTiming> + Send + 'static,
        window: Option<CommitWindow>,
    ) -> Result<oneshot::Receiver<Result<CertifiedWriteTiming>>> {
        let (done, completion) = oneshot::channel();
        self.sender
            .try_send(Job::Commit(Write {
                persist: Box::new(persist),
                done,
                window,
            }))
            .map_err(|_| anyhow!("certified writer is unavailable"))?;
        Ok(completion)
    }
}

impl Replica {
    #[cfg(feature = "jolteon-prebuilt-state-write")]
    pub(super) fn prebuild_certified_state(&mut self) {
        if let Some(prepared) = self
            .candidates
            .get(&self.high.point)
            .and_then(|entry| entry.execution.as_ref())
            && self.commit_writer.prebuild(&self.store, prepared)
        {
            let metrics = &mut self.execution.commit_jobs;
            metrics.state_prebuilds = metrics.state_prebuilds.saturating_add(1);
        }
    }

    pub(super) async fn write_certified(
        &mut self,
        inbox: &mut Inbox,
        prepared: Arc<Prepared>,
        records: Vec<ConsensusRecord>,
        retired: Vec<Range<Vec<u8>>>,
        descendants: Option<(&[Point], &[Arc<Prepared>])>,
        window: Option<CommitWindow>,
    ) -> Result<()> {
        let write = self
            .store
            .certified_write(Arc::clone(&prepared), records, retired)?;
        let mut persistence = self.commit_writer.submit(write, window.clone())?;
        if let Some((branch, ancestors)) = descendants {
            self.prepare_commit_descendant(branch, ancestors, window.clone())?;
            // Construction can start against the known certified branch.
            // The Replica processes the result after the write is acknowledged.
            #[cfg(feature = "jolteon-cross-block-pipeline")]
            self.prepare_commit_proposal(
                branch,
                ancestors,
                window.clone().expect("known commit window"),
            )?;
        }
        let node_publication_done = loop {
            tokio::select! {
                biased;
                result = &mut persistence => {
                    let timing = result.context("certified writer stopped")??;
                    let metrics = &mut self.execution.commit_jobs;
                    metrics.persistence.observe(timing.persistence);
                    #[cfg(feature = "jolteon-prebuilt-state-write")]
                    if let Some(elapsed) = timing.reused_state_build {
                        metrics.reused_state_build.observe(elapsed);
                    }
                    #[cfg(feature = "jolteon-reuse-write-buffer")]
                    if timing.reused_write_buffer {
                        metrics.reused_write_buffers = metrics.reused_write_buffers.saturating_add(1);
                    }
                    if let Some(elapsed) = timing.cell_node_publication {
                        metrics.cell_node_publication.observe(elapsed);
                    }
                    break timing.cell_node_publication.is_some();
                }
                event = inbox.recv_live(), if inbox.can_defer() => {
                    let event = event.context("replica transport stopped during commit")?;
                    if event.reply.is_some() && event.frame.kind == wire::STATUS {
                        // The consensus status reports the last installed tip.
                        self.handle(event)?;
                        let metrics = &mut self.execution.commit_jobs;
                        metrics.status_replies = metrics.status_replies.saturating_add(1);
                    } else if descendants.is_some() && self.cache_commit_descendant(&event) {
                        let (branch, ancestors) = descendants.expect("known commit branch");
                        self.prepare_commit_descendant(branch, ancestors, window.clone())?;
                    } else {
                        // In particular, keep journal writes and finality reads
                        // behind the same ordered acknowledgement.
                        inbox.defer(event);
                        let metrics = &mut self.execution.commit_jobs;
                        metrics.deferred_events = metrics.deferred_events.saturating_add(1);
                    }
                }
            }
        };
        self.store.install_durable_tip(&prepared, node_publication_done);
        let metrics = &mut self.execution.commit_jobs;
        metrics.acknowledged = metrics.acknowledged.saturating_add(1);
        if window.is_some() {
            metrics.overlap_eligible_writes = metrics.overlap_eligible_writes.saturating_add(1);
        }
        Ok(())
    }

    fn cache_commit_descendant(&mut self, event: &Incoming) -> bool {
        let frame = &event.frame;
        if event.reply.is_some()
            || frame.kind != wire::PROPOSAL
            || frame.epoch != self.committee.epoch()
            || self.high.point.view.checked_add(1) != Some(self.view)
        {
            return false;
        }
        let Some(peer) = self.directory.sender(event) else {
            return false;
        };
        let Ok(proposal) = wire::ProposalBody::decode(frame.body.clone()) else {
            return false;
        };
        if proposal.view != self.view
            || proposal.seal
            || !proposal.timeout.is_empty()
            || proposal.parent.get(..Point::SIZE) != Some(self.high.point.encode().as_slice())
        {
            return false;
        }
        // An exact already-journaled parent needs no quorum or timeout write.
        // Cache the verified proposal without entering a vote or changing view.
        let result = self
            .check_proposal(proposal)
            .and_then(|(point, checked, _)| self.cache_proposal(point, checked, peer));
        if result.is_err() {
            self.rejected = self.rejected.saturating_add(1);
        }
        if let Some(received) = event.control_received_at {
            self.execution.control_queue.observe(received.elapsed());
        }
        true
    }

    fn prepare_commit_descendant(
        &mut self,
        branch: &[Point],
        ancestors: &[Arc<Prepared>],
        commit: Option<CommitWindow>,
    ) -> Result<()> {
        let Some(target) = self.preferred.get(&self.view) else {
            return Ok(());
        };
        let candidate = &self.candidates[target];
        let proposal = &candidate.proposal;
        let time = proposal.header.context.timestamp_ms;
        let parent = ancestors.last().context("missing commit descendant parent")?;
        if candidate.execution.is_some()
            || candidate.execution_error.is_some()
            || proposal.wire.seal
            || proposal.parent.point != self.high.point
            || time <= parent.header().context.timestamp_ms
            || !can_vote_at(time)
        {
            return Ok(());
        }
        let mut path = branch.to_vec();
        path.push(*target);
        if self.candidate_pipeline.schedule(
            &self.store,
            self.committee.epoch(),
            self.finalized,
            &path,
            proposal,
            ancestors.to_vec(),
            commit,
        )? {
            let jobs = &mut self.execution.candidate_jobs;
            jobs.started = jobs.started.saturating_add(1);
            let commits = &mut self.execution.commit_jobs;
            commits.descendants = commits.descendants.saturating_add(1);
        }
        Ok(())
    }
}

#[cfg(all(test, feature = "jolteon-exact-prefix-pipeline"))]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn overlap_counts_only_the_individual_certified_writes() {
        let timeline = CommitWindow::default();
        let start = Instant::now();
        let ms = Duration::from_millis;
        record_commit_span(&timeline, (start, ms(5)));
        record_commit_span(&timeline, (start + ms(10), ms(7)));
        assert_eq!(
            super::super::commit_overlap(Some(&timeline), (start + ms(2), ms(11))),
            ms(6),
        );
        assert_eq!(
            super::super::commit_overlap(Some(&timeline), (start + ms(6), ms(3))),
            Duration::ZERO,
        );
    }
}
