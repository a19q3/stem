//! Refill ready Lineage materializations as the accepted prefix advances.

use std::{
    panic::{self, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::Result;
use ckb_types::packed::OutPoint;
use stem_ledger::{BlockContext, Transaction};

use super::{Materialized, Window};
use crate::native_store::{
    Budget, LineageSchedule, LiveChanges, Overlay, Store, lineage::Heads, worker_completion,
};

struct Inputs {
    heads: Heads,
    live: LiveChanges,
}

impl Inputs {
    fn capture(
        tx: &Transaction,
        initial: &Heads,
        accepted_heads: &Heads,
        accepted_cells: &LiveChanges,
    ) -> Self {
        let command = tx.lineage_command().expect("planned Lineage command");
        let mut heads = Heads::new();
        let mut live = LiveChanges::new();
        let mut copy_cell = |point: OutPoint| {
            if let Some(cell) = accepted_cells.get(&point) {
                live.insert(point, cell.clone());
            }
        };
        copy_cell(tx.input(0).expect("validated fee input").previous_output);
        for dep in tx.cell_deps() {
            copy_cell(dep.out_point());
        }
        for target in &command.targets {
            let id = target.reference.id();
            let head = accepted_heads
                .get(id)
                .unwrap_or_else(|| initial.get(id).expect("Head checked by the access plan"));
            if let Some(head) = head {
                copy_cell(head.out_point.clone());
            }
            heads.insert(id.clone(), head.clone());
        }
        Self { heads, live }
    }
}

enum Task {
    Pending,
    Running,
    Ready(Option<Materialized>),
}

struct Completion {
    position: usize,
    elapsed: Duration,
    output: std::thread::Result<Result<Option<Materialized>>>,
}

impl Store {
    pub(super) fn drive_lineage_window(
        &self,
        transactions: &[Transaction],
        window: &Window,
        block: &BlockContext<'_>,
        width: usize,
        budget: &mut Budget,
        overlay: &mut Overlay,
        stats: &mut LineageSchedule,
        mut settle: impl FnMut(
            &Transaction,
            &mut Budget,
            &mut Overlay,
            &mut LineageSchedule,
            Option<Materialized>,
            bool,
        ) -> Result<bool>,
    ) -> Result<bool> {
        let prefix = &window.prefix;
        let (sender, receiver) = mpsc::channel::<Completion>();
        let cancelled = &AtomicBool::new(false);
        let mut tasks: Vec<_> = (0..prefix.len()).map(|_| Task::Pending).collect();
        let mut outstanding = 0;
        let mut candidates = Vec::new();
        stats.rolling_windows += 1;
        rayon::in_place_scope_fifo(|scope| {
            let result = (|| {
                for position in 0..prefix.len() {
                    self.check_preparation_cancelled()?;
                    let slot = *budget;
                    let funded = self.funded_lineage_jobs(width, slot);
                    candidates.clear();
                    if funded > 1 {
                        let available = funded.saturating_sub(outstanding);
                        candidates.extend(
                            (position..prefix.len())
                                .filter(|&index| {
                                    prefix[index] <= position
                                        && matches!(tasks[index], Task::Pending)
                                })
                                .take(available),
                        );
                    }

                    if outstanding != 0 || candidates.len() > 1 {
                        let mut contiguous = position;
                        for index in candidates.drain(..) {
                            let tx = &transactions[index];
                            let inputs = Inputs::capture(
                                tx,
                                &window.initial_heads,
                                &overlay.lineages,
                                &overlay.live,
                            );
                            let send = sender.clone();
                            tasks[index] = Task::Running;
                            outstanding += 1;
                            stats.parallel_commands += 1;
                            if index == contiguous {
                                contiguous += 1;
                            } else {
                                stats.lookahead_commands += 1;
                            }
                            scope.spawn_fifo(move |_| {
                                if cancelled.load(Ordering::Relaxed) {
                                    return;
                                }
                                let started = Instant::now();
                                let output = panic::catch_unwind(AssertUnwindSafe(|| {
                                    self.check_preparation_cancelled()?;
                                    self.materialize_lineage_from_view(
                                        tx,
                                        block,
                                        &inputs.heads,
                                        slot,
                                        |point| self.resolve_live(&inputs.live, point),
                                    )
                                }));
                                let _ = send.send(Completion {
                                    position: index,
                                    elapsed: started.elapsed(),
                                    output,
                                });
                            });
                        }
                    }
                    stats.rolling_peak_outstanding =
                        stats.rolling_peak_outstanding.max(outstanding as u64);

                    if matches!(tasks[position], Task::Running) {
                        let started = Instant::now();
                        while matches!(tasks[position], Task::Running) {
                            let completed = worker_completion::next(&receiver);
                            stats.rolling_worker_us += completed.elapsed.as_micros() as u64;
                            // Keep errors at the signed position. Ordered
                            // staging reproduces any failed materialization.
                            let material = completed
                                .output
                                .unwrap_or_else(|panic| panic::resume_unwind(panic))
                                .ok()
                                .flatten();
                            tasks[completed.position] = Task::Ready(material);
                        }
                        stats.rolling_wait_us += started.elapsed().as_micros() as u64;
                    }
                    let (material, attempted) =
                        match std::mem::replace(&mut tasks[position], Task::Pending) {
                            Task::Pending => (None, false),
                            Task::Ready(material) => {
                                outstanding -= 1;
                                (material, true)
                            }
                            Task::Running => unreachable!("current Lineage task completed"),
                        };
                    if settle(
                        &transactions[position],
                        budget,
                        overlay,
                        stats,
                        material,
                        attempted,
                    )? {
                        return Ok(true);
                    }
                }
                Ok(false)
            })();
            // A failed ordered stage can leave later jobs queued.
            cancelled.store(true, Ordering::Relaxed);
            result
        })
    }
}

#[cfg(test)]
mod tests {
    use ckb_types::{
        packed::{Byte32, CellDep, Script},
        prelude::*,
    };
    use stem_ledger::{
        Cell, LiveCell, Origin,
        lineage::{Command, Head, LineageRef, Target},
    };

    use super::*;

    #[test]
    fn worker_snapshot_keeps_only_the_accepted_head_and_needed_cell_edits() -> Result<()> {
        let id: Byte32 = [1; 32].pack();
        let executor = [2; 32].pack();
        let point = |index| OutPoint::new([3; 32].pack(), index);
        let code = point(0);
        let fee = point(1);
        let original = Head {
            out_point: point(2),
            version: 0,
            executor,
        };
        let updated = Head {
            out_point: point(3),
            version: 1,
            executor: original.executor.clone(),
        };
        let tx = Transaction::new_lineage_command(
            vec![CellDep::new_builder().out_point(code.clone()).build()],
            fee.clone(),
            9,
            Command {
                targets: vec![Target {
                    reference: LineageRef::new(id.clone(), None)?,
                    latest_bounds: None,
                    min_capacity: 0,
                }],
                executor: original.executor.clone(),
                executor_dep: code.clone(),
                executor_cycles: 1_000,
                validation_cycles: 1_000,
                fee: 1,
                payload: Default::default(),
            },
        )?;
        let initial = Heads::from([(id.clone(), Some(original.clone()))]);
        let mut heads = Heads::from([(id.clone(), Some(updated.clone()))]);
        let cell = LiveCell {
            cell: Cell::new(1_000_000_000, Script::default(), vec![], Default::default())?,
            origin: Origin {
                height: 1,
                timestamp_ms: 2,
                context_id: Byte32::default(),
            },
        };
        let mut live = LiveChanges::from([
            (code.clone(), None),
            (fee.clone(), Some(cell.clone())),
            (original.out_point.clone(), Some(cell.clone())),
            (updated.out_point.clone(), Some(cell)),
        ]);

        let captured = Inputs::capture(&tx, &initial, &heads, &live);
        heads.insert(id.clone(), None);
        live.clear();
        assert_eq!(captured.heads.get(&id), Some(&Some(updated.clone())));
        assert!(matches!(captured.live.get(&code), Some(None)));
        assert!(matches!(captured.live.get(&fee), Some(Some(_))));
        assert!(matches!(
            captured.live.get(&updated.out_point),
            Some(Some(_))
        ));
        assert!(!captured.live.contains_key(&original.out_point));
        let newer = Inputs::capture(&tx, &initial, &heads, &live);
        assert_eq!(newer.heads.get(&id), Some(&None));
        Ok(())
    }
}
