//! Certified writes own the database writer until durability is acknowledged.
use std::{
    ops::Range,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use stem_stake::jolteon::Point;
use tokio::sync::{mpsc, oneshot};

use super::{CommitWindow, Incoming, Replica, can_vote_at, transport::Inbox, wire};
use crate::native_store::Prepared;

struct Write {
    persist: Box<dyn FnOnce() -> Result<()> + Send>,
    done: oneshot::Sender<Result<Duration>>,
    window: Option<CommitWindow>,
}

pub(super) struct Writer(mpsc::Sender<Write>);

impl Writer {
    pub(super) fn start() -> Result<Self> {
        let (sender, mut jobs) = mpsc::channel::<Write>(1);
        thread::Builder::new().name("stem-commit".into()).spawn(move || {
            while let Some(job) = jobs.blocking_recv() {
                let start = Instant::now();
                let result = (job.persist)();
                let elapsed = start.elapsed();
                if let Some(window) = job.window {
                    let _ = window.set((start, elapsed));
                }
                let result = result.map(|()| elapsed);
                if job.done.send(result).is_err() {
                    break;
                }
            }
        })?;
        Ok(Self(sender))
    }

    fn submit(
        &self,
        persist: impl FnOnce() -> Result<()> + Send + 'static,
        window: Option<CommitWindow>,
    ) -> Result<oneshot::Receiver<Result<Duration>>> {
        let (done, completion) = oneshot::channel();
        self.0
            .try_send(Write {
                persist: Box::new(persist),
                done,
                window,
            })
            .map_err(|_| anyhow!("certified writer is unavailable"))?;
        Ok(completion)
    }
}

impl Replica {
    pub(super) async fn write_certified(
        &mut self,
        inbox: &mut Inbox,
        prepared: Arc<Prepared>,
        records: Vec<(Vec<u8>, Vec<u8>)>,
        retired: Vec<Range<Vec<u8>>>,
        descendants: Option<(&[Point], &[Arc<Prepared>])>,
    ) -> Result<()> {
        let write = self
            .store
            .certified_write(Arc::clone(&prepared), records, retired)?;
        let window = (cfg!(feature = "jolteon-cross-block-pipeline") && descendants.is_some())
            .then(CommitWindow::default);
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
        loop {
            tokio::select! {
                biased;
                result = &mut persistence => {
                    let elapsed = result.context("certified writer stopped")??;
                    self.execution.commit_jobs.persistence.observe(elapsed);
                    break;
                }
                event = inbox.recv_live(), if inbox.can_defer() => {
                    let event = event.context("replica transport stopped during commit")?;
                    if event.reply.is_some() && event.frame.kind == wire::STATUS {
                        // STATUS reads only the last installed in-memory state.
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
        }
        self.store.install_durable_tip(&prepared);
        let metrics = &mut self.execution.commit_jobs;
        metrics.acknowledged = metrics.acknowledged.saturating_add(1);
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
        let Ok(proposal) = wire::canonical_bytes::<wire::Proposal>(frame.body.clone()) else {
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
