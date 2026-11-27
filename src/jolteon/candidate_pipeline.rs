//! One bounded follower-preparation job at a time. The worker never votes or commits.
use std::{sync::Arc, thread, time::Instant};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ckb_types::prelude::*;
use stem_stake::{Hash32, jolteon::Point};
use tokio::sync::mpsc;

use super::{Replica, Store};
use crate::native_store::{PinnedView, Prepared};

struct Binding {
    epoch: u64,
    base: Point,
    base_header: Hash32,
    branch: Vec<Point>,
    target: Point,
    parent: Hash32,
    transactions: Vec<Bytes>,
}

impl Binding {
    fn matches(
        &self,
        epoch: u64,
        base: Point,
        base_header: Hash32,
        mut candidate: impl FnMut(Point) -> Option<(Point, Hash32, bool)>,
    ) -> bool {
        if self.epoch != epoch {
            return false;
        }
        let suffix = if self.base == base {
            if self.base_header != base_header {
                return false;
            }
            self.branch.as_slice()
        } else if cfg!(feature = "jolteon-commit-pipeline")
            && base.view > self.base.view
            && base.header == base_header
        {
            // The committed Point must be the exact certified prefix of the
            // pinned execution. The unfinished suffix is still speculative.
            let Some(index) = self.branch.iter().position(|point| *point == base) else {
                return false;
            };
            &self.branch[index + 1..]
        } else {
            return false;
        };
        if suffix.is_empty() {
            return false;
        }
        let mut parent = base;
        for point in suffix {
            let Some((point_parent, header_parent, same_transactions)) = candidate(*point) else {
                return false;
            };
            if point_parent != parent
                || (*point == self.target && (header_parent != self.parent || !same_transactions))
            {
                return false;
            }
            parent = *point;
        }
        parent == self.target
    }
}

struct Job {
    binding: Binding,
    state: PinnedView,
    ancestors: Vec<Arc<Prepared>>,
    time: u64,
}

pub(super) struct Completion {
    binding: Binding,
    prepared: Result<Option<Prepared>>,
    elapsed: std::time::Duration,
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
        let (complete, results) = mpsc::channel(1);
        thread::Builder::new()
            .name("stem-candidate".into())
            .spawn(move || {
                while let Some(job) = jobs.blocking_recv() {
                    let started = Instant::now();
                    let prepared = workers.install(|| {
                        job.state.prepare_candidate(
                            &job.ancestors,
                            &job.binding.transactions,
                            job.time,
                        )
                    });
                    if complete
                        .blocking_send(Completion {
                            binding: job.binding,
                            prepared,
                            elapsed: started.elapsed(),
                        })
                        .is_err()
                    {
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

    pub(super) fn schedule(
        &mut self,
        store: &Store,
        epoch: u64,
        base: Point,
        branch: &[Point],
        candidate: &super::Checked,
        ancestors: Vec<Arc<Prepared>>,
    ) -> Result<bool> {
        if self.running {
            return Ok(false);
        }
        let target = *branch.last().context("empty preparation branch")?;
        let binding = Binding {
            epoch,
            base,
            base_header: store.tip().id(store.network()).unpack(),
            branch: branch.to_vec(),
            target,
            parent: candidate.header.context.parent.unpack(),
            transactions: candidate.wire.transactions.clone(),
        };
        self.send
            .try_send(Job {
                binding,
                state: store.pinned_view(),
                ancestors,
                time: candidate.header.context.timestamp_ms,
            })
            .map_err(|_| anyhow::anyhow!("candidate preparation worker stopped"))?;
        self.running = true;
        Ok(true)
    }
}

impl Replica {
    pub(super) fn complete_preparation(&mut self, completion: Completion) -> Result<()> {
        ensure!(
            self.candidate_pipeline.running,
            "unexpected candidate completion"
        );
        self.candidate_pipeline.running = false;
        self.execution.candidate_verify.observe(completion.elapsed);
        let Completion {
            binding, prepared, ..
        } = completion;
        if !self.preparation_is_current(&binding) || matches!(prepared, Ok(None)) {
            self.execution.candidate_jobs.stale =
                self.execution.candidate_jobs.stale.saturating_add(1);
            return Ok(());
        }
        let candidate = self
            .candidates
            .get_mut(&binding.target)
            .context("missing prepared candidate")?;
        if candidate.execution.is_some() {
            return Ok(());
        }
        let verified = prepared.and_then(|result| {
            let prepared = result.context("missing prepared state")?;
            ensure!(
                prepared.header() == &candidate.proposal.header,
                "candidate execution differs from the signed Header"
            );
            Ok(prepared)
        });
        match verified {
            Ok(prepared) => {
                candidate.execution = Some(Arc::new(prepared));
                self.execution.candidate_jobs.applied =
                    self.execution.candidate_jobs.applied.saturating_add(1);
                if binding.base != self.finalized {
                    let jobs = &mut self.execution.candidate_jobs;
                    jobs.rebased = jobs.rebased.saturating_add(1);
                }
            }
            Err(error) => {
                // Keep a deterministic invalid candidate from consuming the one job every tick.
                // Storage faults use the same fatal path as synchronous finalization.
                if error.is::<crate::native_store::StateReadFault>() {
                    return Err(error.context(super::StorageFault));
                }
                candidate.execution_error = Some(format!("{error:#}"));
                self.execution.candidate_jobs.invalid =
                    self.execution.candidate_jobs.invalid.saturating_add(1);
            }
        }
        Ok(())
    }

    fn preparation_is_current(&self, binding: &Binding) -> bool {
        let base_header: Hash32 = self.store.tip().id(self.store.network()).unpack();
        binding.matches(
            self.committee.epoch(),
            self.finalized,
            base_header,
            |point| {
                let proposal = &self.candidates.get(&point)?.proposal;
                if proposal.wire.view != point.view {
                    return None;
                }
                Some((
                    proposal.parent.point,
                    proposal.header.context.parent.unpack(),
                    point != binding.target || proposal.wire.transactions == binding.transactions,
                ))
            },
        ) && self.preparation_needed(binding.target)
    }

    fn preparation_needed(&self, point: Point) -> bool {
        let interests = [
            Some(self.high.point),
            self.preferred.get(&self.view).copied(),
            self.handoff_tip.as_ref().map(|quorum| quorum.point),
            self.recovery.preparation_target(),
        ];
        interests.into_iter().flatten().any(|mut head| {
            // A later view may still need the same exact ancestor for execution.
            for _ in 0..super::MAX_BRANCH {
                if head == point {
                    return true;
                }
                if head.view <= point.view {
                    return false;
                }
                let Some(candidate) = self.candidates.get(&head) else {
                    return false;
                };
                head = candidate.proposal.parent.point;
            }
            false
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changed_epoch_base_fork_or_wire_sequence_rejects_a_completion() {
        let point = |view, parent_view, byte| Point {
            view,
            parent_view,
            header: [byte; 32],
        };
        let base = point(3, 2, 3);
        let parent = point(4, 3, 4);
        let target = point(5, 4, 5);
        let fork = point(4, 3, 6);
        let binding = Binding {
            epoch: 7,
            base,
            base_header: base.header,
            branch: vec![parent, target],
            target,
            parent: parent.header,
            transactions: vec![Bytes::from_static(b"a"), Bytes::from_static(b"b")],
        };
        let matches = |epoch, anchor, committed, actual_parent: Point, keep_order| {
            binding.matches(epoch, anchor, committed, |point| match point {
                p if p == parent => Some((base, base.header, true)),
                p if p == target => Some((actual_parent, actual_parent.header, keep_order)),
                _ => None,
            })
        };
        assert!(matches(7, base, base.header, parent, true));
        assert!(!matches(8, base, base.header, parent, true));
        assert!(!matches(7, base, fork.header, parent, true));
        assert_eq!(
            matches(7, parent, parent.header, parent, true),
            cfg!(feature = "jolteon-commit-pipeline")
        );
        assert!(!matches(7, fork, fork.header, parent, true));
        assert!(!matches(7, parent, fork.header, parent, true));
        assert!(!matches(7, target, target.header, parent, true));
        assert!(!matches(7, base, base.header, fork, true));
        assert!(!matches(7, base, base.header, parent, false));
    }
}
