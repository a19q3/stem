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
#[cfg(feature = "jolteon-pre-quorum-proposal")]
use super::BranchExecution;
#[cfg(feature = "jolteon-shared-proposal-snapshot")]
use super::mempool::ProposalSnapshot;
use crate::native_store::{PinnedView, Prepared, ProposalScan, StateReadFault};
#[cfg(feature = "jolteon-cancel-stale-execution")]
use crate::native_store::{PreparationCancellation, PreparationCancelled};

#[derive(Clone, Copy)]
struct Stamp {
    epoch: u64,
    view: u64,
    base: Point,
    base_header: Hash32,
    parent: Point,
    #[cfg(feature = "jolteon-pre-quorum-proposal")]
    prior_high: Option<Point>,
}

impl Stamp {
    #[cfg(feature = "jolteon-pre-quorum-proposal")]
    fn awaiting_quorum(&self, epoch: u64, view: u64, base: Point, high: Point) -> bool {
        self.prior_high == Some(high)
            && self.epoch == epoch
            && self.base == base
            && self.parent.view == view
            && view.checked_add(1) == Some(self.view)
    }

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
        if cfg!(feature = "jolteon-exact-prefix-pipeline") {
            if current.base.view <= self.base.view || current.base.header != current.base_header {
                return None;
            }
            let index = branch.iter().position(|point| *point == current.base)?;
            return Some(&branch[index + 1..]);
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
    #[cfg(feature = "jolteon-shared-proposal-snapshot")]
    transactions: ProposalSnapshot,
    #[cfg(not(feature = "jolteon-shared-proposal-snapshot"))]
    transactions: Vec<Transaction>,
    #[cfg(not(feature = "jolteon-shared-proposal-snapshot"))]
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

#[cfg(feature = "jolteon-cancel-stale-execution")]
struct Active {
    stamp: Stamp,
    branch: Vec<Point>,
    committing: bool,
    cancellation: PreparationCancellation,
}

pub(super) struct Pipeline {
    send: mpsc::Sender<Job>,
    running: bool,
    ready: Option<Completion>,
    #[cfg(feature = "jolteon-cancel-stale-execution")]
    active: Option<Active>,
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
                    let (execution, work, scan) = workers.install(|| {
                        let executing = Instant::now();
                        #[cfg(feature = "jolteon-shared-proposal-snapshot")]
                        let (transactions, mut scan) = job.transactions.candidates();
                        #[cfg(not(feature = "jolteon-shared-proposal-snapshot"))]
                        let (transactions, mut scan) = (job.transactions, job.scan);
                        let result = job.state.build_proposal(
                            &job.ancestors,
                            transactions,
                            job.time,
                            MAX_PROPOSAL_BYTES,
                            &mut scan,
                            job.commit.is_some(),
                        );
                        (result, (executing, executing.elapsed()), scan)
                    });
                    #[cfg(feature = "jolteon-shared-proposal-snapshot")]
                    drop(job.transactions);
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
                #[cfg(feature = "jolteon-cancel-stale-execution")]
                active: None,
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
            #[cfg(feature = "jolteon-pre-quorum-proposal")]
            prior_high: None,
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
        self.schedule_proposal_at(
            self.proposal_stamp(), branch, ancestors, parent_header, time, commit,
        )
    }

    fn schedule_proposal_at(
        &mut self,
        stamp: Stamp,
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
        let state = self.store.pinned_view();
        let capture = Instant::now();
        let snapshot_entries = self.mempool.len() as u64;
        #[cfg(feature = "jolteon-shared-proposal-snapshot")]
        let transactions = self.mempool.proposal_snapshot();
        #[cfg(not(feature = "jolteon-shared-proposal-snapshot"))]
        let (transactions, scan) = {
            let (transactions, scan) = self.mempool.proposal_candidates();
            (transactions.cloned().collect(), scan)
        };
        let capture = capture.elapsed();
        let committing = commit.is_some();
        #[cfg(feature = "jolteon-cancel-stale-execution")]
        let active = Active {
            stamp,
            branch: branch.clone(),
            committing,
            cancellation: PreparationCancellation::default(),
        };
        #[cfg(feature = "jolteon-cancel-stale-execution")]
        let state = state.with_cancellation(active.cancellation.clone());
        let job = Job {
            stamp,
            branch,
            parent_header,
            time,
            state,
            ancestors,
            transactions,
            #[cfg(not(feature = "jolteon-shared-proposal-snapshot"))]
            scan,
            commit,
        };
        self.proposal_pipeline
            .send
            .try_send(job)
            .map_err(|_| anyhow::anyhow!("proposal construction worker stopped"))?;
        self.proposal_pipeline.running = true;
        #[cfg(feature = "jolteon-cancel-stale-execution")]
        {
            self.proposal_pipeline.active = Some(active);
        }
        let jobs = &mut self.execution.proposal_jobs;
        jobs.started = jobs.started.saturating_add(1);
        jobs.snapshot_capture.observe(capture);
        jobs.snapshot_entries = jobs.snapshot_entries.saturating_add(snapshot_entries);
        if committing {
            jobs.commit_overlap_started = jobs.commit_overlap_started.saturating_add(1);
        }
        #[cfg(feature = "jolteon-pre-quorum-proposal")]
        if stamp.prior_high.is_some() {
            jobs.pre_quorum_started = jobs.pre_quorum_started.saturating_add(1);
        }
        Ok(())
    }

    #[cfg(feature = "jolteon-pre-quorum-proposal")]
    pub(super) fn prepare_next_view(
        &mut self,
        voted: Point,
        branch: Vec<Point>,
        execution: BranchExecution,
    ) -> Result<()> {
        let Some(view) = voted.view.checked_add(1) else {
            return Ok(());
        };
        if self.proposal_pipeline.running
            || self.proposal_pipeline.ready.is_some()
            || self.mempool.len() == 0
            || self.view != voted.view
            || self.high.point.view.checked_add(1) != Some(voted.view)
            || self.candidates[&voted].proposal.parent.point != self.high.point
            || branch.len() >= super::MAX_BRANCH
            || branch.len() != execution.batches.len()
            || Some(self.committee.leader(view)?) != self.member
        {
            return Ok(());
        }
        let stamp = Stamp {
            view,
            parent: voted,
            prior_high: Some(self.high.point),
            ..self.proposal_stamp()
        };
        if !self.waiting_for_proposal_quorum(&stamp) {
            return Ok(());
        }
        let parent = execution.batches.last().context("missing voted execution")?.header();
        if execution.pending.as_ref().is_some_and(|change| {
            parent.context.height.saturating_add(1) >= change.activation_height
        }) {
            return Ok(());
        }
        let Ok(time) = super::now_ms() else {
            return Ok(());
        };
        if time <= parent.context.timestamp_ms {
            return Ok(());
        }
        self.schedule_proposal_at(stamp, branch, execution.batches, voted.header, time, None)
    }

    #[cfg(feature = "jolteon-pre-quorum-proposal")]
    fn waiting_for_proposal_quorum(&self, stamp: &Stamp) -> bool {
        stamp.awaiting_quorum(self.committee.epoch(), self.view, self.finalized, self.high.point)
            && self.pre_quorum_eligible(stamp)
    }

    #[cfg(feature = "jolteon-pre-quorum-proposal")]
    fn pre_quorum_eligible(&self, stamp: &Stamp) -> bool {
        self.handoff_tip.is_none()
            && !self.recovery.active
            && !self.preferred.contains_key(&stamp.view)
            && self.since.elapsed() < self.timeout * (1 << self.backoff)
    }

    #[cfg(feature = "jolteon-pre-quorum-proposal")]
    fn keep_pre_quorum_proposal(&self, stamp: &Stamp, branch: &[Point]) -> bool {
        self.pre_quorum_eligible(stamp)
            && (stamp.awaiting_quorum(self.committee.epoch(), self.view, self.finalized, self.high.point)
                || stamp.remaining(&self.proposal_stamp(), branch, false).is_some())
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
            #[cfg(feature = "jolteon-pre-quorum-proposal")]
            if self.waiting_for_proposal_quorum(&done.stamp) {
                return false;
            }
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
        #[cfg(feature = "jolteon-pre-quorum-proposal")]
        if self.proposal_pipeline.ready.as_ref().is_some_and(|done| {
            self.waiting_for_proposal_quorum(&done.stamp)
        }) {
            return Ok(true);
        }
        let Some(done) = self.proposal_pipeline.ready.take() else {
            return Ok(false);
        };
        self.finish_proposal(done)
    }

    #[cfg(feature = "jolteon-cancel-stale-execution")]
    pub(super) fn cancel_stale_proposal(&mut self) {
        #[cfg(feature = "jolteon-pre-quorum-proposal")]
        if self.proposal_pipeline.ready.as_ref().is_some_and(|done| {
            done.stamp.prior_high.is_some() && !self.keep_pre_quorum_proposal(&done.stamp, &done.branch)
        }) {
            let stale = self.proposal_pipeline.ready.take().expect("stale proposal");
            self.discard_proposal(stale.elapsed);
        }
        let Some(active) = self.proposal_pipeline.active.as_ref() else {
            return;
        };
        if active.cancellation.is_cancelled() {
            return;
        }
        let stamp = active.stamp;
        let obsolete = || stamp.epoch != self.committee.epoch()
            || stamp.view != self.view
            || stamp.parent != self.high.point
            || self.since.elapsed() >= self.timeout * (1 << self.backoff)
            || (stamp.base != self.finalized
                && stamp
                    .remaining(&self.proposal_stamp(), &active.branch, active.committing)
                    .is_none());
        #[cfg(feature = "jolteon-pre-quorum-proposal")]
        let obsolete = if stamp.prior_high.is_some() {
            !self.keep_pre_quorum_proposal(&stamp, &active.branch)
        } else {
            obsolete()
        };
        #[cfg(not(feature = "jolteon-pre-quorum-proposal"))]
        let obsolete = obsolete();
        if obsolete && active.cancellation.cancel() {
            let jobs = &mut self.execution.proposal_jobs;
            jobs.cancel_requested = jobs.cancel_requested.saturating_add(1);
        }
    }

    pub(super) fn complete_proposal(&mut self, done: Completion) -> Result<()> {
        ensure!(self.proposal_pipeline.running, "unexpected proposal completion");
        self.proposal_pipeline.running = false;
        #[cfg(feature = "jolteon-cancel-stale-execution")]
        {
            self.proposal_pipeline.active = None;
        }
        self.execution.proposal.observe(done.elapsed);
        self.execution.proposal_scan.record(done.scan);
        #[cfg(feature = "jolteon-cancel-stale-execution")]
        if matches!(&done.execution, Err(error) if error.is::<PreparationCancelled>()) {
            let jobs = &mut self.execution.proposal_jobs;
            jobs.cancelled = jobs.cancelled.saturating_add(1);
            jobs.cancelled_us = jobs.cancelled_us.saturating_add(done.elapsed.as_micros() as u64);
            return Ok(());
        }
        if let Ok(Some((_, execution))) = &done.execution {
            self.execution
                .proposal_commitments
                .record(execution.commitment_timing());
        }
        #[cfg(feature = "jolteon-pre-quorum-proposal")]
        if done.stamp.prior_high.is_some() {
            // The next tick processes finality before attempting publication.
            if !matches!(&done.execution, Ok(None))
                && self.keep_pre_quorum_proposal(&done.stamp, &done.branch)
            {
                self.proposal_pipeline.ready = Some(done);
            } else {
                self.discard_proposal(done.elapsed);
            }
            return Ok(());
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
        #[cfg(feature = "jolteon-pre-quorum-proposal")]
        let pre_quorum_overlap = done.stamp.prior_high.map(|_| {
            self.since.saturating_duration_since(done.work.0).min(done.work.1)
        });
        let rebased_blocks = remaining.map_or(0, |suffix| done.branch.len() - suffix.len());
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
        #[cfg(feature = "jolteon-pre-quorum-proposal")]
        if let Some(overlap) = pre_quorum_overlap {
            jobs.pre_quorum_published = jobs.pre_quorum_published.saturating_add(1);
            jobs.pre_quorum_overlap_us =
                jobs.pre_quorum_overlap_us.saturating_add(overlap.as_micros() as u64);
        }
        if !overlap.is_zero() {
            jobs.commit_overlap_published = jobs.commit_overlap_published.saturating_add(1);
            jobs.commit_overlap_us = jobs.commit_overlap_us.saturating_add(overlap.as_micros() as u64);
        }
        if rebased_blocks != 0 {
            jobs.rebased = jobs.rebased.saturating_add(1);
            jobs.rebased_blocks = jobs.rebased_blocks.saturating_add(rebased_blocks as u64);
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
        let stamp = Stamp {
            epoch: 1, view: 6, base, base_header: base.header, parent,
            #[cfg(feature = "jolteon-pre-quorum-proposal")]
            prior_high: None,
        };
        let branch = [first, parent];
        assert_eq!(stamp.remaining(&stamp, &branch, false), Some(branch.as_slice()));

        let mut current = Stamp { base: first, base_header: first.header, ..stamp };
        assert_eq!(stamp.remaining(&current, &branch, true), Some(&branch[1..]));
        let exact_prefix = cfg!(feature = "jolteon-exact-prefix-pipeline");
        assert_eq!(stamp.remaining(&current, &branch, false), exact_prefix.then_some(&branch[1..]));
        current.base_header = parent.header;
        assert!(stamp.remaining(&current, &branch, true).is_none());
        current.base = parent;
        assert_eq!(stamp.remaining(&current, &branch, true), exact_prefix.then_some(&branch[2..]));
        assert_eq!(stamp.remaining(&current, &branch, false), exact_prefix.then_some(&branch[2..]));
        current.base = point(4, 14);
        current.base_header = current.base.header;
        assert!(stamp.remaining(&current, &branch, true).is_none());
        current.base = first;
        current.base_header = first.header;
        current.epoch += 1;
        assert!(stamp.remaining(&current, &branch, true).is_none());
        current.epoch -= 1;
        current.view += 1;
        assert!(stamp.remaining(&current, &branch, true).is_none());
    }

    #[cfg(feature = "jolteon-pre-quorum-proposal")]
    #[test]
    fn pre_quorum_preparation_needs_the_exact_voted_parent() {
        let point = |view, byte| Point { view, parent_view: view - 1, header: [byte; 32] };
        let base = point(4, 4);
        let high = point(5, 5);
        let voted = point(6, 6);
        let fork = point(6, 16);
        let before = Stamp {
            epoch: 2, view: 6, base, base_header: base.header, parent: high, prior_high: None,
        };
        let job = Stamp { view: 7, parent: voted, prior_high: Some(high), ..before };
        let branch = [high, voted];
        assert!(job.awaiting_quorum(before.epoch, before.view, base, high));
        assert!(job.remaining(&before, &branch, false).is_none());
        assert!(!job.awaiting_quorum(before.epoch + 1, before.view, base, high));
        assert!(!job.awaiting_quorum(before.epoch, before.view, high, high));
        assert!(!job.awaiting_quorum(before.epoch, before.view, base, point(5, 15)));

        let timeout = Stamp { view: 7, ..before };
        assert!(!job.awaiting_quorum(timeout.epoch, timeout.view, base, high));
        assert!(job.remaining(&timeout, &branch, false).is_none());
        let changed = Stamp { parent: fork, ..timeout };
        assert!(job.remaining(&changed, &branch, false).is_none());

        let certified = Stamp { parent: voted, ..timeout };
        assert_eq!(job.remaining(&certified, &branch, false), Some(branch.as_slice()));
        let committed = Stamp { base: high, base_header: high.header, ..certified };
        assert_eq!(job.remaining(&committed, &branch, false), Some(&branch[1..]));
    }
}
