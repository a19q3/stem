//! Prepare a plain Cell run with a bounded, refillable queue of VM jobs.

#[cfg(feature = "native-reuse-ordered-cell-writes")]
use std::cell::Cell;
#[cfg(feature = "native-incremental-cell-projection")]
use std::time::Duration;
use std::{
    cell::RefCell,
    panic::{self, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Instant,
};

#[cfg(feature = "native-rolling-cell-execution")]
use super::Context;
#[cfg(feature = "native-rolling-leader-execution")]
use super::ProposalWindow;
#[cfg(feature = "native-reuse-rolling-parent-reads")]
use super::ParentReads;
#[cfg(feature = "native-rolling-dependency-hints")]
use super::dependencies::{DependencyReads, SAMPLE_TRANSACTIONS};
#[cfg(not(feature = "native-incremental-cell-projection"))]
use super::project;
use super::{
    BlockContext, Budget, Effects, Origin, Overlay, PrecedingCells, ProjectedPrefix, Projection,
    Receipt, Result, Sources, Store, Transaction,
};
#[cfg(feature = "native-incremental-cell-projection")]
use super::{IncrementalProjection, declared_writes};
#[cfg(feature = "native-incremental-cell-projection")]
use std::sync::RwLock;
use crate::native_store::{CellStreamStats, ProposalStep, ProposalTrial, worker_completion};
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

struct Completion {
    position: usize,
    slot: Budget,
    outcome: thread::Result<Result<Trial>>,
    #[cfg(feature = "native-reuse-rolling-parent-reads")]
    parent_reads: ParentReads,
}

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
            |tx, trial, budget, overlay, canonical| {
                let reused = match trial {
                    ProposalTrial::Ready(work) => work.adopt(budget, overlay),
                    _ => None,
                };
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
                        #[cfg(feature = "native-compact-cell-trial-edits")]
                        None,
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
                    ProposalTrial::<fn(&mut Budget, &mut Overlay) -> Option<Receipt>>::Ordered,
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
            |tx, trial, budget, overlay, canonical| {
                let trial = trial.map(|work| {
                    move |budget: &mut Budget, overlay: &mut Overlay| work.adopt(budget, overlay)
                });
                self.stage_proposal_entry(
                    tx,
                    block,
                    origin,
                    budget,
                    overlay,
                    progress,
                    scan,
                    trial,
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
            ProposalTrial<ReadyCell>,
            &mut Budget,
            &mut Overlay,
            &PrecedingCells<'_>,
        ) -> Result<ProposalStep>,
    ) -> Result<(usize, bool)> {
        let (processed, stopped) = self.drive_cell_segment(
            transactions,
            block,
            origin,
            width,
            budget,
            overlay,
            stats,
            finish_ordered_suffix,
            #[cfg(feature = "native-resume-rolling-proposal")]
            finish_ordered_suffix,
            #[cfg(feature = "native-shared-cell-deps")]
            dependency_reads,
            &mut settle,
        )?;
        #[cfg(feature = "native-resume-rolling-proposal")]
        if finish_ordered_suffix && !stopped && processed < transactions.len() {
            // The first scope has joined, and its accepted effects are back
            // in the overlay. Give the remaining entries one fresh scope.
            stats.projection_restarts += 1;
            let (tail, stopped) = self.drive_cell_segment(
                &transactions[processed..],
                block,
                origin,
                width,
                budget,
                overlay,
                stats,
                true,
                false,
                #[cfg(feature = "native-shared-cell-deps")]
                dependency_reads,
                &mut settle,
            )?;
            return Ok((processed + tail, stopped));
        }
        Ok((processed, stopped))
    }

    fn drive_cell_segment(
        &self,
        transactions: &[Transaction],
        block: &BlockContext<'_>,
        origin: &Origin,
        width: usize,
        budget: &mut Budget,
        overlay: &mut Overlay,
        stats: &mut CellStreamStats,
        finish_ordered_suffix: bool,
        #[cfg(feature = "native-resume-rolling-proposal")] resume_on_barrier: bool,
        #[cfg(feature = "native-shared-cell-deps")] dependency_reads: &mut Option<
            super::DependencyReads,
        >,
        settle: &mut impl FnMut(
            &Transaction,
            ProposalTrial<ReadyCell>,
            &mut Budget,
            &mut Overlay,
            &PrecedingCells<'_>,
        ) -> Result<ProposalStep>,
    ) -> Result<(usize, bool)> {
        let depth = self.rolling_cell_depth(width, *budget, finish_ordered_suffix);
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
        let writes = RwLock::new(IncrementalProjection::new(depth.saturating_mul(2)));
        #[cfg(feature = "native-incremental-cell-projection")]
        let projected = Projection::Incremental(&writes);
        #[cfg(feature = "native-incremental-cell-projection")]
        let mut projected_prefix = 0;
        #[cfg(feature = "native-incremental-cell-projection")]
        let mut projected_transactions = 0;
        #[cfg(feature = "native-incremental-cell-projection")]
        let mut projection_elapsed = Duration::ZERO;
        #[cfg(feature = "native-incremental-cell-projection")]
        let mut declarations = Vec::new();
        #[cfg(feature = "native-rolling-dependency-hints")]
        let mut sampled_through = if dependency_reads.is_some() { 0 } else { SAMPLE_TRANSACTIONS };
        #[cfg(feature = "native-shared-cell-deps")]
        let shared = &*dependency_reads
            .get_or_insert_with(|| super::DependencyReads::collect(self, transactions));
        #[cfg(feature = "native-rolling-dependency-hints")]
        let mut hints = shared.snapshot();
        #[cfg(feature = "native-rolling-dependency-hints")]
        let mut hint_stats = super::DependencyReadStats::default();
        // Workers see an immutable prefix and declared edits in this run.
        // The coordinator applies accepted effects in a separate delta.
        let mut base = std::mem::take(&mut overlay.live);
        let prior = &base;
        #[cfg(feature = "native-reuse-ordered-cell-writes")]
        let ordered_writes = Cell::new(false);
        #[cfg(feature = "native-reuse-rolling-parent-reads")]
        let retry_reads = RefCell::new(ParentReads::default());
        let canonical = PrecedingCells {
            base: prior,
            projection: None,
            #[cfg(feature = "native-reclaim-cell-projection")]
            settled: Some(&writes),
            #[cfg(feature = "native-reuse-ordered-cell-writes")]
            ordered_writes: Some(&ordered_writes),
            #[cfg(feature = "native-shared-cell-deps")]
            dependencies: shared.reader(),
            #[cfg(feature = "native-reuse-rolling-parent-reads")]
            parent_reads: self.pinned_parent_reads.then_some(&retry_reads),
        };
        let (sender, receiver) = mpsc::channel();
        let cancelled = &AtomicBool::new(false);
        let mut published = vec![false; count];
        let mut ready = (0..depth)
            .map(|_| None)
            .collect::<Vec<Option<Completion>>>();
        let in_flight_limit = width.saturating_mul(2).min(depth);
        let mut in_flight = 0;
        #[cfg(feature = "native-leader-tail-cycle-shares")]
        let mut reserved_cycles = 0u64;
        let mut submitted = 0;
        let mut prefix = 0;
        #[cfg(feature = "native-reclaim-cell-projection")]
        let mut retired_prefix = 0;
        #[cfg(feature = "native-reclaim-cell-projection")]
        let mut reclaim_elapsed = Duration::ZERO;
        #[cfg(feature = "native-reclaim-cell-projection")]
        let mut force_ordered = false;
        let result = rayon::in_place_scope_fifo(|scope| {
            let result = (|| -> Result<(usize, bool)> {
                while prefix < count {
                    self.check_preparation_cancelled()?;
                    let funded = self.cell_window_limit(width, *budget);
                    #[cfg(feature = "native-leader-tail-cycle-shares")]
                    let share_tail = finish_ordered_suffix && funded < 2 && budget.cycles >= 2;
                    #[cfg(feature = "native-leader-tail-cycle-shares")]
                    let funded = if share_tail { width.min(depth) } else { funded };
                    #[cfg(feature = "native-reclaim-cell-projection")]
                    let funded = if force_ordered { 0 } else { funded };
                    while submitted < count
                        && submitted - prefix < funded
                        && in_flight < in_flight_limit
                    {
                        let slot = *budget;
                        #[cfg(feature = "native-leader-tail-cycle-shares")]
                        let slot = if share_tail {
                            let vacancies = (funded - (submitted - prefix)).min(count - submitted);
                            let unreserved = slot.cycles.saturating_sub(reserved_cycles);
                            let cycles = self.limits.transaction_cycles.min(unreserved / vacancies as u64);
                            if cycles == 0 {
                                break;
                            }
                            stats.tail_cycle_share_jobs += u64::from(cycles < self.limits.transaction_cycles);
                            Budget { cycles, ..slot }
                        } else {
                            slot
                        };
                        let position = submitted;
                        #[cfg(feature = "native-rolling-dependency-hints")]
                        if position >= sampled_through && count - position >= 2 {
                            sampled_through = count.min(position + SAMPLE_TRANSACTIONS);
                            DependencyReads::refresh(
                                self,
                                &transactions[position..sampled_through],
                                &mut hints,
                                &mut hint_stats,
                            );
                        }
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
                                    rows.add(point, edit);
                                }
                                stats.peak_projection_edits =
                                    stats.peak_projection_edits.max(rows.len() as u64);
                            }
                            projection_elapsed += projecting.elapsed();
                            projected_transactions += position - projected_prefix;
                            projected_prefix = position;
                        }
                        let tx = &transactions[position];
                        #[cfg(feature = "native-leader-tail-cycle-shares")]
                        if finish_ordered_suffix {
                            // Hold the reservation through ordered settlement, even after
                            // its worker has returned a result.
                            reserved_cycles += slot.cycles.min(self.limits.transaction_cycles);
                        }
                        let send = sender.clone();
                        let parallel_groups = count - prefix < width;
                        #[cfg(feature = "native-rolling-dependency-hints")]
                        let hints = std::sync::Arc::clone(&hints);
                        scope.spawn_fifo(move |_| {
                            if cancelled.load(Ordering::Relaxed) {
                                return;
                            }
                            // A panic must still occupy its completion slot
                            // while the coordinator can need that result.
                            #[cfg(feature = "native-reuse-rolling-parent-reads")]
                            let parent_reads = RefCell::new(ParentReads::default());
                            let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                                let sources = RefCell::new(SourcePositions::default());
                                let view = PrecedingCells {
                                    base: prior,
                                    projection: Some(ProjectedPrefix {
                                        writes: projected,
                                        position,
                                        sources: Sources::Run(&sources),
                                    }),
                                    #[cfg(feature = "native-reclaim-cell-projection")]
                                    settled: None,
                                    #[cfg(feature = "native-reuse-ordered-cell-writes")]
                                    ordered_writes: None,
                                    #[cfg(all(
                                        feature = "native-shared-cell-deps",
                                        not(feature = "native-rolling-dependency-hints"),
                                    ))]
                                    dependencies: shared.reader(),
                                    #[cfg(feature = "native-rolling-dependency-hints")]
                                    dependencies: shared.reader_from(&hints),
                                    #[cfg(feature = "native-reuse-rolling-parent-reads")]
                                    parent_reads: self.pinned_parent_reads.then_some(&parent_reads),
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
                            let _ = send.send(Completion {
                                position,
                                slot,
                                outcome,
                                #[cfg(feature = "native-reuse-rolling-parent-reads")]
                                parent_reads: parent_reads.into_inner(),
                            });
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
                    #[cfg(feature = "native-leader-tail-cycle-shares")]
                    let mut tail_slot = false;
                    let work = if prefix == submitted {
                        if !finish_ordered_suffix {
                            break;
                        }
                        submitted += 1;
                        ProposalTrial::Ordered
                    } else {
                        let Some(completion) = ready[prefix % depth].take() else {
                            let completion = worker_completion::next(&receiver);
                            let index = completion.position % depth;
                            debug_assert!(ready[index].is_none());
                            ready[index] = Some(completion);
                            in_flight -= 1;
                            continue;
                        };
                        #[cfg(feature = "native-reuse-rolling-parent-reads")]
                        retry_reads.replace(completion.parent_reads);
                        let slot = completion.slot;
                        #[cfg(feature = "native-leader-tail-cycle-shares")]
                        if finish_ordered_suffix {
                            reserved_cycles -= slot.cycles.min(self.limits.transaction_cycles);
                            tail_slot = slot.cycles < self.limits.transaction_cycles;
                        }
                        match completion.outcome.unwrap_or_else(|panic| panic::resume_unwind(panic)) {
                            Ok(trial) if trial.sources.published(&published) => {
                                ProposalTrial::Ready(ReadyCell { effects: trial.effects, slot })
                            }
                            Err(error) if finish_ordered_suffix => ProposalTrial::failed(error),
                            _ => ProposalTrial::Recheck,
                        }
                    };
                    let had_trial = !matches!(work, ProposalTrial::Ordered);
                    let step = settle(&transactions[prefix], work, budget, overlay, &canonical);
                    #[cfg(feature = "native-reuse-rolling-parent-reads")]
                    {
                        let reads = retry_reads.take();
                        stats.retry_parent_records += reads.len() as u64;
                        stats.retry_parent_hits += reads.hits;
                    }
                    let step = step?;
                    #[cfg(feature = "native-mask-rejected-cell-projections")]
                    if finish_ordered_suffix
                        && prefix + 1 < count
                        && !cancelled.load(Ordering::Relaxed)
                        && step.rejected()
                    {
                        // Later reads can see past this candidate immediately.
                        // Jobs that saw it earlier still fail the source check.
                        writes.write().expect("Cell projection").reject(prefix);
                        stats.masked_projection_positions += 1;
                    }
                    #[cfg(feature = "native-reuse-parent-spend-rejections")]
                    let had_trial = had_trial && !matches!(step, ProposalStep::RejectedParentSpend);
                    if matches!(step, ProposalStep::Reused) {
                        stats.reused += 1;
                        #[cfg(feature = "native-leader-tail-cycle-shares")]
                        {
                            stats.tail_cycle_share_reused += u64::from(tail_slot);
                        }
                        published[prefix] = true;
                    } else if had_trial {
                        stats.fallbacks += 1;
                    }
                    #[cfg(feature = "native-reuse-ordered-cell-writes")]
                    if ordered_writes.replace(false) {
                        published[prefix] = true;
                        stats.ordered_declarations += 1;
                    }
                    #[cfg(feature = "native-reclaim-cell-projection")]
                    if matches!(step, ProposalStep::Ordered) && !published[prefix] && !force_ordered {
                        // A successful ordered transaction whose Cell effects
                        // were not certified cannot be represented by the
                        // declarations used by in-flight jobs.
                        force_ordered = true;
                        stats.projection_barriers += 1;
                        stats.discarded_jobs += (submitted - prefix - 1) as u64;
                        cancelled.store(true, Ordering::Relaxed);
                        submitted = prefix + 1;
                        ready.iter_mut().for_each(|slot| *slot = None);
                        #[cfg(feature = "native-resume-rolling-proposal")]
                        if resume_on_barrier
                            && count - submitted >= 2
                            && self.rolling_cell_depth(width, *budget, true) >= 2
                        {
                            prefix = submitted;
                            return Ok((prefix, false));
                        }
                    }
                    prefix += 1;
                    if matches!(step, ProposalStep::Stop) {
                        return Ok((prefix, true));
                    }
                    #[cfg(feature = "native-reclaim-cell-projection")]
                    if !force_ordered && prefix < count && prefix - retired_prefix >= depth {
                        // Every outstanding position is >= prefix. A read
                        // that already saw a rejected declaration still
                        // checks its source against the published flags.
                        let clearing = Instant::now();
                        let delta = std::mem::take(&mut overlay.live);
                        let retired = writes.write().expect("Cell projection").advance(prefix, delta);
                        stats.retired_projection_edits += retired as u64;
                        retired_prefix = prefix;
                        // The most recent settled job may not have needed
                        // its declaration published before the fold.
                        projected_prefix = projected_prefix.max(retired_prefix);
                        reclaim_elapsed += clearing.elapsed();
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
        #[cfg(feature = "native-rolling-dependency-hints")]
        dependency_reads
            .as_mut()
            .expect("candidate dependency reads")
            .set_snapshot(hints, hint_stats);
        #[cfg(feature = "native-incremental-cell-projection")]
        stats.record_projection(projected_transactions, projection_elapsed);
        #[cfg(feature = "native-reclaim-cell-projection")]
        {
            let state = writes.into_inner().expect("Cell projection");
            base.extend(state.settled);
            stats.projection_reclaim_us = stats.projection_reclaim_us.saturating_add(
                u64::try_from(reclaim_elapsed.as_micros()).unwrap_or(u64::MAX),
            );
        }
        base.extend(std::mem::take(&mut overlay.live));
        overlay.live = base;
        stats.elapsed_us = stats
            .elapsed_us
            .saturating_add(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
        result
    }
}
