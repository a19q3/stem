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

use super::{MAX_PROPOSAL_BYTES, Replica, can_vote_at};
use crate::native_store::{PinnedView, Prepared, ProposalScan, StateReadFault};

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    epoch: u64,
    view: u64,
    base: Point,
    base_header: Hash32,
    parent: Point,
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
}

pub(super) struct Completion {
    stamp: Stamp,
    branch: Vec<Point>,
    parent_header: Hash32,
    time: u64,
    execution: Result<Option<(Vec<Transaction>, Prepared)>>,
    scan: ProposalScan,
    elapsed: Duration,
}

pub(super) struct Pipeline {
    send: mpsc::Sender<Job>,
    running: bool,
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
                    let execution = workers.install(|| {
                        job.state.build_proposal(
                            &job.ancestors,
                            job.transactions,
                            job.time,
                            MAX_PROPOSAL_BYTES,
                            &mut scan,
                        )
                    });
                    let result = Completion {
                        stamp: job.stamp,
                        branch: job.branch,
                        parent_header: job.parent_header,
                        time: job.time,
                        execution,
                        scan,
                        elapsed: started.elapsed(),
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
    ) -> Result<()> {
        ensure!(
            !self.proposal_pipeline.running,
            "proposal construction already running"
        );
        let stamp = self.proposal_stamp();
        let state = self.store.pinned_view();
        let (transactions, scan) = self.mempool.proposal_candidates();
        let job = Job {
            stamp,
            branch,
            parent_header,
            time,
            state,
            ancestors,
            transactions: transactions.cloned().collect(),
            scan,
        };
        self.proposal_pipeline
            .send
            .try_send(job)
            .map_err(|_| anyhow::anyhow!("proposal construction worker stopped"))?;
        self.proposal_pipeline.running = true;
        let jobs = &mut self.execution.proposal_jobs;
        jobs.started = jobs.started.saturating_add(1);
        Ok(())
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
        let parent = if done.stamp == self.proposal_stamp()
            && !matches!(done.execution, Ok(None))
        {
            self.proposal_parent()?
        } else {
            None
        };
        let parent = match parent {
            Some((parent, timeout))
                if self.branch(parent.point)?.as_deref() == Some(done.branch.as_slice())
                    && self.since.elapsed() < self.timeout * (1 << self.backoff)
                    && can_vote_at(done.time) =>
            {
                Some((parent, timeout))
            }
            _ => None,
        };
        let Some((parent, timeout)) = parent else {
            let jobs = &mut self.execution.proposal_jobs;
            jobs.stale = jobs.stale.saturating_add(1);
            return Ok(());
        };
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
        Ok(())
    }
}
