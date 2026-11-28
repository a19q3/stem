//! Prepare a plain Cell run with a bounded, refillable queue of VM jobs.

#[cfg(feature = "native-reuse-ordered-cell-writes")]
use std::cell::Cell;
use std::{
    cell::RefCell,
    panic::{self, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(feature = "native-rolling-cell-execution")]
use super::Context;
#[cfg(feature = "native-rolling-leader-execution")]
use super::ProposalWindow;
#[cfg(not(feature = "native-incremental-cell-projection"))]
use super::project;
use super::{
    BlockContext, Budget, Effects, Origin, Overlay, PrecedingCells, ProjectedPrefix, Projection,
    Receipt, Result, Sources, Store, Transaction,
};
#[cfg(feature = "native-incremental-cell-projection")]
use super::{Writes, declared_writes, record};
#[cfg(feature = "native-incremental-cell-projection")]
use std::sync::RwLock;
use crate::native_store::{CellStreamStats, ProposalStep};
#[cfg(feature = "native-rolling-leader-execution")]
use crate::native_store::{ProposalProgress, ProposalScan};
#[cfg(feature = "native-rolling-leader-execution")]
use std::{borrow::Borrow, iter::Peekable};

#[derive(Default)]
pub(super) struct SourcePositions {
    first: Option<usize>,
    later: Vec<usize>,
}

impl SourcePositions {
    pub(super) fn record(&mut self, position: usize) {
        match self.first {
            None => self.first = Some(position),
            Some(first) if first == position => {}
            Some(_) => self.later.push(position),
        }
    }

    fn published(&self, positions: &[bool]) -> bool {
        self.first.is_none_or(|position| positions[position])
            && self.later.iter().all(|&position| positions[position])
    }
}

struct Trial {
    effects: Effects,
    sources: SourcePositions,
}

type Completion = (usize, Budget, thread::Result<Result<Trial>>);

pub(in crate::native_store) struct ReadyCell {
    effects: Effects,
    slot: Budget,
}

impl ReadyCell {
    pub(in crate::native_store) fn adopt(
        self,
        budget: &mut Budget,
        overlay: &mut Overlay,
    ) -> Option<Receipt> {
        self.effects.adopt(self.slot, budget, overlay)
    }
}

// The coordinator can reach an entry after its speculative budget runs out,
// or recheck a trial whose result or dependencies could not be adopted.
pub(in crate::native_store) enum CellAttempt {
    Ordered,
    Recheck,
    Ready(ReadyCell),
}

impl CellAttempt {
    fn adopt(self, budget: &mut Budget, overlay: &mut Overlay) -> Option<Receipt> {
        match self {
            Self::Ready(work) => work.adopt(budget, overlay),
            Self::Ordered | Self::Recheck => None,
        }
    }
}

fn next_completion(receiver: &Receiver<Completion>) -> Completion {
    loop {
        match receiver.try_recv() {
            Ok(completion) => return completion,
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => unreachable!("Cell workers lost their scope"),
        }
        // The coordinator can be on the same pool as the workers. Run pending
        // tasks before parking, and periodically retry if several blocks share
        // the pool and work becomes available after the probe.
        if rayon::yield_now() != Some(rayon::Yield::Executed) {
            match receiver.recv_timeout(Duration::from_micros(100)) {
                Ok(completion) => return completion,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    unreachable!("Cell workers lost their scope")
                }
            }
        }
    }
}

impl Store {
    #[cfg(feature = "native-rolling-cell-execution")]
    pub(in crate::native_store) fn prepare_cell_run(
        &self,
        transactions: &[Transaction],
        block: &BlockContext<'_>,
        origin: &Origin,
        width: usize,
        budget: &mut Budget,
        overlay: &mut Overlay,
        receipts: &mut Vec<Receipt>,
        fees: &mut u64,
        stats: &mut CellStreamStats,
        #[cfg(feature = "native-shared-cell-deps")] dependency_reads: &mut Option<
            super::DependencyReads,
        >,
    ) -> Result<usize> {
        let depth = self.cell_window_limit(width, *budget);
        if depth < 2 {
            return Ok(0);
        }
        let count = transactions
            .iter()
            .take_while(|tx| self.has_declared_cell_writes(tx))
            .count();
        if count <= depth {
            return Ok(0);
        }
        let (processed, _) = self.drive_cell_run(
            &transactions[..count],
            block,
            origin,
            width,
            budget,
            overlay,
            stats,
            false,
            #[cfg(feature = "native-shared-cell-deps")]
            dependency_reads,
            |tx, ready, budget, overlay, canonical| {
                let reused = ready.adopt(budget, overlay);
                let (receipt, step) = if let Some(receipt) = reused {
                    (receipt, ProposalStep::Reused)
                } else {
                    let receipt = self.stage_transaction(
                        tx,
                        block,
                        origin,
                        budget,
                        overlay,
                        None,
                        |resolved, limit| self.cache.verify(resolved, limit, false),
                        None,
                        Some(canonical),
                    )?;
                    (receipt, ProposalStep::Ordered)
                };
                *fees = fees
                    .checked_add(receipt.fee)
                    .context("block fee overflow")?;
                receipts.push(receipt);
                Ok(step)
            },
        )?;
        Ok(processed)
    }

    #[cfg(feature = "native-rolling-leader-execution")]
    pub(in crate::native_store) fn stage_rolling_proposal<T: Borrow<Transaction>>(
        &self,
        input: &mut Peekable<impl Iterator<Item = T>>,
        window: &mut ProposalWindow<T>,
        block: &BlockContext<'_>,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
        progress: &mut ProposalProgress,
        scan: &mut ProposalScan,
        width: usize,
        keeper_count: usize,
    ) -> Result<Option<bool>> {
        let available =
            self.limits.transaction_count as usize - progress.transactions.len() - keeper_count;
        let run = self.collect_proposal_run(input, *budget, progress.wire_bytes, available, width);
        match run.as_slice() {
            [] => return Ok(None),
            [tx] => {
                let step = self.stage_proposal_entry(
                    tx,
                    block,
                    origin,
                    budget,
                    overlay,
                    progress,
                    scan,
                    None::<fn(&mut Budget, &mut Overlay) -> Option<Receipt>>,
                    None,
                )?;
                return Ok(Some(matches!(step, ProposalStep::Stop)));
            }
            _ => {}
        }
        // When shared dependency reads are enabled the window owns the one
        // read cache for this proposal, including the fixed/ordered paths.
        #[cfg(not(feature = "native-shared-cell-deps"))]
        let _ = window;
        let mut stats = CellStreamStats::default();
        let (_, stopped) = self.drive_cell_run(
            &run,
            block,
            origin,
            width,
            budget,
            overlay,
            &mut stats,
            true,
            #[cfg(feature = "native-shared-cell-deps")]
            window.shared_reads(),
            |tx, ready, budget, overlay, canonical| {
                let adoption = (!matches!(ready, CellAttempt::Ordered)).then_some(
                    move |budget: &mut Budget, overlay: &mut Overlay| ready.adopt(budget, overlay),
                );
                self.stage_proposal_entry(
                    tx,
                    block,
                    origin,
                    budget,
                    overlay,
                    progress,
                    scan,
                    adoption,
                    Some(canonical),
                )
            },
        )?;
        scan.parallel_cell_jobs = scan.parallel_cell_jobs.saturating_add(stats.jobs);
        scan.rolling_cells.record(stats);
        Ok(Some(stopped))
    }

    pub(in crate::native_store) fn drive_cell_run(
        &self,
        transactions: &[Transaction],
        block: &BlockContext<'_>,
        origin: &Origin,
        width: usize,
        budget: &mut Budget,
        overlay: &mut Overlay,
        stats: &mut CellStreamStats,
        finish_ordered_suffix: bool,
        #[cfg(feature = "native-shared-cell-deps")] dependency_reads: &mut Option<
            super::DependencyReads,
        >,
        mut settle: impl FnMut(
            &Transaction,
            CellAttempt,
            &mut Budget,
            &mut Overlay,
            &PrecedingCells<'_>,
        ) -> Result<ProposalStep>,
    ) -> Result<(usize, bool)> {
        let depth = self.cell_window_limit(width, *budget);
        debug_assert!(depth >= 2);
        let count = transactions.len();
        let started = Instant::now();
        stats.runs += 1;
        #[cfg(not(feature = "native-incremental-cell-projection"))]
        let writes = {
            let projecting = Instant::now();
            let writes = project(&self.network, transactions, origin)?;
            stats.record_projection(count, projecting.elapsed());
            writes
        };
        #[cfg(not(feature = "native-incremental-cell-projection"))]
        let projected = Projection::Complete(&writes);
        #[cfg(feature = "native-incremental-cell-projection")]
        let writes = RwLock::new(Writes::with_capacity(depth.saturating_mul(2)));
        #[cfg(feature = "native-incremental-cell-projection")]
        let projected = Projection::Incremental(&writes);
        #[cfg(feature = "native-incremental-cell-projection")]
        let mut projected_prefix = 0;
        #[cfg(feature = "native-incremental-cell-projection")]
        let mut projection_elapsed = Duration::ZERO;
        #[cfg(feature = "native-incremental-cell-projection")]
        let mut declarations = Vec::new();
        #[cfg(feature = "native-shared-cell-deps")]
        let shared = &*dependency_reads
            .get_or_insert_with(|| super::DependencyReads::collect(self, transactions));
        // Workers see an immutable prefix and declared edits in this run.
        // The coordinator applies accepted effects in a separate delta.
        let mut base = std::mem::take(&mut overlay.live);
        let prior = &base;
        #[cfg(feature = "native-reuse-ordered-cell-writes")]
        let ordered_writes = Cell::new(false);
        let canonical = PrecedingCells {
            base: prior,
            projection: None,
            #[cfg(feature = "native-reuse-ordered-cell-writes")]
            ordered_writes: Some(&ordered_writes),
            #[cfg(feature = "native-shared-cell-deps")]
            dependencies: shared.reader(),
        };
        let (sender, receiver) = mpsc::channel();
        let cancelled = &AtomicBool::new(false);
        let mut published = vec![false; count];
        let mut ready = (0..depth)
            .map(|_| None)
            .collect::<Vec<Option<Completion>>>();
        let in_flight_limit = width.saturating_mul(2).min(depth);
        let mut in_flight = 0;
        let mut submitted = 0;
        let mut prefix = 0;
        let result = rayon::in_place_scope_fifo(|scope| {
            let result = (|| -> Result<(usize, bool)> {
                while prefix < count {
                    self.check_preparation_cancelled()?;
                    let funded = self.cell_window_limit(width, *budget);
                    while submitted < count
                        && submitted - prefix < funded
                        && in_flight < in_flight_limit
                    {
                        let position = submitted;
                        #[cfg(feature = "native-incremental-cell-projection")]
                        if projected_prefix < position {
                            let projecting = Instant::now();
                            for (offset, earlier) in
                                transactions[projected_prefix..position].iter().enumerate()
                            {
                                let index = projected_prefix + offset;
                                // Hash and materialize before taking the write lock.
                                for edit in declared_writes(&self.network, earlier, origin, index) {
                                    declarations.push(edit?);
                                }
                                let mut rows = writes.write().expect("Cell projection");
                                for (point, edit) in declarations.drain(..) {
                                    record(&mut rows, point, edit);
                                }
                            }
                            projection_elapsed += projecting.elapsed();
                            projected_prefix = position;
                        }
                        let tx = &transactions[position];
                        let slot = *budget;
                        let send = sender.clone();
                        let parallel_groups = count - prefix < width;
                        scope.spawn_fifo(move |_| {
                            if cancelled.load(Ordering::Relaxed) {
                                return;
                            }
                            // A panic must still occupy its completion slot
                            // while the coordinator can need that result.
                            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                                let sources = RefCell::new(SourcePositions::default());
                                let view = PrecedingCells {
                                    base: prior,
                                    projection: Some(ProjectedPrefix {
                                        writes: projected,
                                        position,
                                        sources: Sources::Run(&sources),
                                    }),
                                    #[cfg(feature = "native-reuse-ordered-cell-writes")]
                                    ordered_writes: None,
                                    #[cfg(feature = "native-shared-cell-deps")]
                                    dependencies: shared.reader(),
                                };
                                let effects = self.execute_cell_trial(
                                    tx,
                                    block,
                                    origin,
                                    slot,
                                    &view,
                                    parallel_groups,
                                )?;
                                #[cfg(feature = "native-shared-cell-deps")]
                                drop(view);
                                Ok(Trial {
                                    effects,
                                    sources: sources.into_inner(),
                                })
                            }));
                            let _ = send.send((position, slot, outcome));
                        });
                        if position == 0 {
                            stats.first_submit_us = stats.first_submit_us.saturating_add(
                                u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                            );
                        }
                        if submitted >= depth && submitted > prefix {
                            stats.refilled_jobs += 1;
                        }
                        submitted += 1;
                        in_flight += 1;
                        stats.jobs += 1;
                    }
                    stats.peak_outstanding =
                        stats.peak_outstanding.max((submitted - prefix) as u64);
                    let work = if prefix == submitted {
                        if !finish_ordered_suffix {
                            break;
                        }
                        submitted += 1;
                        CellAttempt::Ordered
                    } else {
                        let Some((_, slot, outcome)) = ready[prefix % depth].take() else {
                            let completion = next_completion(&receiver);
                            let index = completion.0 % depth;
                            debug_assert!(ready[index].is_none());
                            ready[index] = Some(completion);
                            in_flight -= 1;
                            continue;
                        };
                        let trial = outcome.unwrap_or_else(|panic| panic::resume_unwind(panic));
                        trial
                            .ok()
                            .filter(|trial| trial.sources.published(&published))
                            .map_or(CellAttempt::Recheck, |trial| {
                                CellAttempt::Ready(ReadyCell {
                                    effects: trial.effects,
                                    slot,
                                })
                            })
                    };
                    let had_trial = !matches!(work, CellAttempt::Ordered);
                    let step = settle(&transactions[prefix], work, budget, overlay, &canonical)?;
                    if matches!(step, ProposalStep::Reused) {
                        stats.reused += 1;
                        published[prefix] = true;
                    } else if had_trial {
                        stats.fallbacks += 1;
                    }
                    #[cfg(feature = "native-reuse-ordered-cell-writes")]
                    if ordered_writes.replace(false) {
                        published[prefix] = true;
                        stats.ordered_declarations += 1;
                    }
                    prefix += 1;
                    if matches!(step, ProposalStep::Stop) {
                        return Ok((prefix, true));
                    }
                }
                Ok((prefix, false))
            })();
            if submitted > prefix {
                cancelled.store(true, Ordering::Relaxed);
                stats.discarded_jobs += (submitted - prefix) as u64;
            }
            result
        });
        #[cfg(feature = "native-shared-cell-deps")]
        drop(canonical);
        #[cfg(feature = "native-incremental-cell-projection")]
        stats.record_projection(projected_prefix, projection_elapsed);
        base.extend(std::mem::take(&mut overlay.live));
        overlay.live = base;
        stats.elapsed_us = stats
            .elapsed_us
            .saturating_add(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
        result
    }
}
