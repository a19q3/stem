//! Prepare a plain Cell run with a bounded, refillable queue of VM jobs.

use std::{
    cell::RefCell,
    panic::{self, AssertUnwindSafe},
    sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use super::{
    BlockContext, Budget, Context, Effects, Origin, Overlay, PrecedingCells, ProjectedPrefix,
    Receipt, Result, Sources, Store, Transaction, project,
};
use crate::native_store::CellStreamStats;

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

    fn adopted(&self, positions: &[bool]) -> bool {
        self.first.is_none_or(|position| positions[position])
            && self.later.iter().all(|&position| positions[position])
    }
}

struct Trial {
    effects: Effects,
    sources: SourcePositions,
}

type Completion = (usize, Budget, thread::Result<Result<Trial>>);

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

        let transactions = &transactions[..count];
        let started = Instant::now();
        stats.runs += 1;
        let writes = project(&self.network, transactions, origin)?;
        // Workers see an immutable prefix and the declared edits in this run.
        // Ordered staging writes into a separate delta until the scope joins.
        let mut base = std::mem::take(&mut overlay.live);
        let prior = &base;
        let canonical = PrecedingCells {
            base: prior,
            projection: None,
        };
        let projected = &writes;
        let (sender, receiver) = mpsc::channel();
        let mut adopted = vec![false; count];
        let mut ready = (0..depth)
            .map(|_| None)
            .collect::<Vec<Option<Completion>>>();
        let in_flight_limit = width.saturating_mul(2).min(depth);
        let mut in_flight = 0;
        let mut submitted = 0;
        let mut prefix = 0;
        let result = rayon::in_place_scope_fifo(|scope| -> Result<usize> {
            while prefix < count {
                let funded = self.cell_window_limit(width, *budget);
                while submitted < count
                    && submitted - prefix < funded
                    && in_flight < in_flight_limit
                {
                    let position = submitted;
                    let tx = &transactions[position];
                    let slot = *budget;
                    let send = sender.clone();
                    let parallel_groups = count - prefix < width;
                    scope.spawn_fifo(move |_| {
                        // A worker panic must still occupy its completion slot.
                        // Otherwise the coordinator can wait forever for it.
                        let outcome = panic::catch_unwind(AssertUnwindSafe(|| {
                            let sources = RefCell::new(SourcePositions::default());
                            let view = PrecedingCells {
                                base: prior,
                                projection: Some(ProjectedPrefix {
                                    writes: projected,
                                    position,
                                    sources: Sources::Run(&sources),
                                }),
                            };
                            let effects = self.execute_cell_trial(
                                tx,
                                block,
                                origin,
                                slot,
                                &view,
                                parallel_groups,
                            )?;
                            Ok(Trial {
                                effects,
                                sources: sources.into_inner(),
                            })
                        }));
                        let _ = send.send((position, slot, outcome));
                    });
                    if submitted >= depth && submitted > prefix {
                        stats.refilled_jobs += 1;
                    }
                    submitted += 1;
                    in_flight += 1;
                    stats.jobs += 1;
                }
                stats.peak_outstanding = stats.peak_outstanding.max((submitted - prefix) as u64);
                if prefix == submitted {
                    break;
                }

                let Some((_, slot, outcome)) = ready[prefix % depth].take() else {
                    let completion = next_completion(&receiver);
                    let index = completion.0 % depth;
                    debug_assert!(ready[index].is_none());
                    ready[index] = Some(completion);
                    in_flight -= 1;
                    continue;
                };
                let trial = outcome.unwrap_or_else(|panic| panic::resume_unwind(panic));
                let reused = trial.ok().and_then(|trial| {
                    if trial.sources.adopted(&adopted) {
                        trial.effects.adopt(slot, budget, overlay)
                    } else {
                        None
                    }
                });
                let receipt = if let Some(receipt) = reused {
                    stats.reused += 1;
                    adopted[prefix] = true;
                    receipt
                } else {
                    stats.fallbacks += 1;
                    // These transactions have no wrapper operations; the
                    // ordinary staging function reads the exact accepted prefix.
                    self.stage_transaction(
                        &transactions[prefix],
                        block,
                        origin,
                        budget,
                        overlay,
                        None,
                        |resolved, limit| self.cache.verify(resolved, limit, false),
                        None,
                        Some(&canonical),
                    )?
                };
                *fees = fees
                    .checked_add(receipt.fee)
                    .context("block fee overflow")?;
                receipts.push(receipt);
                prefix += 1;
            }
            Ok(prefix)
        });
        base.extend(std::mem::take(&mut overlay.live));
        overlay.live = base;
        stats.elapsed_us = stats
            .elapsed_us
            .saturating_add(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
        result
    }
}
