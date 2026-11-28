//! Bounded Leader construction. Only the Replica may publish the result.
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use ckb_types::prelude::*;
use stem_ledger::Transaction;
use stem_stake::{Hash32, jolteon::Point};
use tokio::sync::mpsc;

use super::{CommitWindow, MAX_PROPOSAL_BYTES, Replica, can_vote_at, commit_overlap};
use crate::native_store::{PinnedView, Prepared, ProposalScan, StateReadFault};

struct Stamp {
    epoch: u64,
    view: u64,
    base: Point,
    base_header: Hash32,
    parent: Point,
}

impl Stamp {
    fn remaining<'a>(
        &self,
        current: &Self,
        branch: &'a [Point],
        committing: bool,
    ) -> Option<&'a [Point]> {
        if self.epoch != current.epoch
            || self.view != current.view
            || self.parent != current.parent
        {
            return None;
        }
        if self.base == current.base {
            return (self.base_header == current.base_header).then_some(branch);
        }
        let (first, suffix) = branch.split_first()?;
        (committing && *first == current.base && first.header == current.base_header)
            .then_some(suffix)
    }
}

struct Job {
    stamp: Stamp,
    branch: Vec<Point>,
    parent_header: Hash32,
    time: u64,
    state: PinnedView,
    ancestors: Vec<Arc<Prepared>>,
    transactions: Vec<Transaction>,
    scan: ProposalScan,
    commit: Option<CommitWindow>,
}

pub(super) struct Completion {
    stamp: Stamp,
    branch: Vec<Point>,
    parent_header: Hash32,
    time: u64,
    execution: Result<Option<(Vec<Transaction>, Prepared)>>,
    scan: ProposalScan,
    work: (Instant, Duration),
    elapsed: Duration,
    commit: Option<CommitWindow>,
}

pub(super) struct Pipeline {
    send: mpsc::Sender<Job>,
    running: bool,
    ready: Option<Completion>,
}

impl Pipeline {
    pub(super) fn start(
        workers: Arc<rayon::ThreadPool>,
    ) -> Result<(Self, mpsc::Receiver<Completion>)> {
        let (send, mut jobs) = mpsc::channel::<Job>(1);
        let (done, results) = mpsc::channel(1);
        thread::Builder::new()
            .name("stem-proposal".into())
            .spawn(move || {
                while let Some(job) = jobs.blocking_recv() {
                    let started = Instant::now();
                    let mut scan = job.scan;
                    let (execution, work) = workers.install(|| {
                        let executing = Instant::now();
                        let result = job.state.build_proposal(
                            &job.ancestors,
                            job.transactions,
                            job.time,
                            MAX_PROPOSAL_BYTES,
                            &mut scan,
                            job.commit.is_some(),
                        );
                        (result, (executing, executing.elapsed()))
                    });
                    let result = Completion {
                        stamp: job.stamp,
                        branch: job.branch,
                        parent_header: job.parent_header,
                        time: job.time,
                        execution,
                        scan,
                        work,
                        elapsed: started.elapsed(),
                        commit: job.commit,
                    };
                    if done.blocking_send(result).is_err() {
                        break;
                    }
                }
            })?;
        Ok((
            Self {
                send,
                running: false,
                ready: None,
            },
            results,
        ))
    }

    pub(super) fn running(&self) -> bool {
        self.running
    }
}

impl Replica {
    fn proposal_stamp(&self) -> Stamp {
        Stamp {
            epoch: self.committee.epoch(),
            view: self.view,
            base: self.finalized,
            base_header: self.store.tip().id(self.store.network()).unpack(),
            parent: self.high.point,
        }
    }

    pub(super) fn schedule_proposal(
        &mut self,
        branch: Vec<Point>,
        ancestors: Vec<Arc<Prepared>>,
        parent_header: Hash32,
        time: u64,
        commit: Option<CommitWindow>,
    ) -> Result<()> {
        ensure!(
            !self.proposal_pipeline.running && self.proposal_pipeline.ready.is_none(),
            "proposal construction already running"
        );
        let stamp = self.proposal_stamp();
        let state = self.store.pinned_view();
        let (transactions, scan) = self.mempool.proposal_candidates();
        let committing = commit.is_some();
        let job = Job {
            stamp,
            branch,
            parent_header,
            time,
            state,
            ancestors,
            transactions: transactions.cloned().collect(),
            scan,
            commit,
        };
        self.proposal_pipeline
            .send
            .try_send(job)
            .map_err(|_| anyhow::anyhow!("proposal construction worker stopped"))?;
        self.proposal_pipeline.running = true;
        let jobs = &mut self.execution.proposal_jobs;
        jobs.started = jobs.started.saturating_add(1);
        if committing {
            jobs.commit_overlap_started = jobs.commit_overlap_started.saturating_add(1);
        }
        Ok(())
    }

    #[cfg(feature = "jolteon-cross-block-pipeline")]
    pub(super) fn prepare_commit_proposal(
        &mut self,
        branch: &[Point],
        ancestors: &[Arc<Prepared>],
        commit: CommitWindow,
    ) -> Result<()> {
        let current = self.proposal_stamp();
        if self.proposal_pipeline.ready.as_ref().is_some_and(|done| {
            done.stamp
                .remaining(&current, &done.branch, done.commit.is_some())
                .is_none()
        }) {
            let stale = self.proposal_pipeline.ready.take().expect("stale proposal");
            self.discard_proposal(stale.elapsed);
        }
        if self.proposal_pipeline.running
            || self.proposal_pipeline.ready.is_some()
            || self.mempool.len() == 0
            || self.proposal_parent()?.is_none()
            || self.since.elapsed() >= self.timeout * (1 << self.backoff)
        {
            return Ok(());
        }
        let parent = ancestors.last().context("missing certified proposal parent")?;
        let Ok(time) = super::now_ms() else {
            return Ok(());
        };
        if time <= parent.header().context.timestamp_ms {
            return Ok(());
        }
        let parent_header = parent.header().id(self.store.network()).unpack();
        self.schedule_proposal(branch.to_vec(), ancestors.to_vec(), parent_header, time, Some(commit))
    }

    pub(super) fn publish_paced_proposal(&mut self) -> Result<bool> {
        let Some(done) = self.proposal_pipeline.ready.take() else {
            return Ok(false);
        };
        self.finish_proposal(done)
    }

    pub(super) fn complete_proposal(&mut self, done: Completion) -> Result<()> {
        ensure!(self.proposal_pipeline.running, "unexpected proposal completion");
        self.proposal_pipeline.running = false;
        self.execution.proposal.observe(done.elapsed);
        self.execution.proposal_scan.record(done.scan);
        if let Ok(Some((_, execution))) = &done.execution {
            self.execution
                .proposal_commitments
                .record(execution.commitment_timing());
        }
        self.finish_proposal(done).map(|_| ())
    }

    fn discard_proposal(&mut self, elapsed: Duration) {
        let jobs = &mut self.execution.proposal_jobs;
        jobs.stale = jobs.stale.saturating_add(1);
        jobs.stale_us = jobs.stale_us.saturating_add(elapsed.as_micros() as u64);
    }

    fn finish_proposal(&mut self, done: Completion) -> Result<bool> {
        let current = self.proposal_stamp();
        let remaining = done.stamp.remaining(&current, &done.branch, done.commit.is_some());
        let parent = if remaining.is_some() && !matches!(done.execution, Ok(None)) {
            self.proposal_parent()?
        } else {
            None
        };
        let parent = match parent {
            Some((parent, timeout))
                if self.branch(parent.point)?.as_deref() == remaining
                    && self.since.elapsed() < self.timeout * (1 << self.backoff)
                    && can_vote_at(done.time) =>
            {
                Some((parent, timeout))
            }
            _ => None,
        };
        let Some((parent, timeout)) = parent else {
            self.discard_proposal(done.elapsed);
            return Ok(false);
        };
        if self.since.elapsed() < self.proposal_interval {
            self.proposal_pipeline.ready = Some(done);
            return Ok(true);
        }
        let overlap = commit_overlap(done.commit.as_ref(), done.work);
        let rebased = done.stamp.base != current.base;
        let (transactions, execution) = done
            .execution
            .map_err(|error| {
                if error.is::<StateReadFault>() {
                    error.context(super::StorageFault)
                } else {
                    error
                }
            })?
            .context("missing pinned proposal state")?;
        let actual_parent: Hash32 = execution.header().context.parent.unpack();
        ensure!(
            actual_parent == done.parent_header
                && execution.header().context.timestamp_ms == done.time,
            "prepared proposal differs from its pinned parent or time"
        );
        self.publish_proposal(
            parent,
            timeout,
            execution.header().clone(),
            transactions,
            Some(Arc::new(execution)),
        )?;
        let jobs = &mut self.execution.proposal_jobs;
        jobs.published = jobs.published.saturating_add(1);
        if !overlap.is_zero() {
            jobs.commit_overlap_published = jobs.commit_overlap_published.saturating_add(1);
            jobs.commit_overlap_us = jobs.commit_overlap_us.saturating_add(overlap.as_micros() as u64);
        }
        if rebased {
            jobs.rebased = jobs.rebased.saturating_add(1);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_binding_accepts_only_its_scheduled_commit_prefix() {
        let point = |view, value| Point { view, parent_view: view - 1, header: [value; 32] };
        let base = point(3, 3);
        let first = point(4, 4);
        let parent = point(5, 5);
        let stamp = Stamp { epoch: 1, view: 6, base, base_header: base.header, parent };
        let branch = [first, parent];
        assert_eq!(stamp.remaining(&stamp, &branch, false), Some(branch.as_slice()));

        let mut current = Stamp { base: first, base_header: first.header, ..stamp };
        assert_eq!(stamp.remaining(&current, &branch, true), Some(&branch[1..]));
        assert!(stamp.remaining(&current, &branch, false).is_none());
        current.base_header = parent.header;
        assert!(stamp.remaining(&current, &branch, true).is_none());
        current.base = parent;
        assert!(stamp.remaining(&current, &branch, true).is_none());
        current.base = first;
        current.base_header = first.header;
        current.epoch += 1;
        assert!(stamp.remaining(&current, &branch, true).is_none());
        current.epoch -= 1;
        current.view += 1;
        assert!(stamp.remaining(&current, &branch, true).is_none());
    }
}
