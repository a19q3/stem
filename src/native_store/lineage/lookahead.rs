//! Plan materialization against an ordered block prefix.

use std::time::Instant;

use anyhow::{Context, Result};
#[cfg(not(feature = "native-rolling-lineage-execution"))]
use rayon::prelude::*;
use stem_ledger::{BlockContext, Origin, Transaction, block::Receipt};

use super::{Materialized, access_plan::plan, current_head};
use crate::native_store::{Budget, LineageSchedule, Overlay, Store};

#[cfg(feature = "native-leader-lineage-execution")]
mod leader;
#[cfg(feature = "native-rolling-lineage-execution")]
mod rolling;

const MAX_LOOKAHEAD: usize = 32;

struct Window {
    prefix: Vec<usize>,
    #[cfg(feature = "native-rolling-lineage-execution")]
    initial_heads: super::Heads,
}

impl Store {
    fn funded_lineage_jobs(&self, width: usize, budget: Budget) -> usize {
        usize::try_from(budget.cycles / self.limits.transaction_cycles.max(1))
            .unwrap_or(usize::MAX)
            .min(width)
    }

    fn lineage_window_limit(&self, width: usize, budget: Budget) -> usize {
        if self.funded_lineage_jobs(width, budget) < 2 {
            return 0;
        }
        width.saturating_mul(4).min(MAX_LOOKAHEAD)
    }

    fn plan_lineage_window(
        &self,
        transactions: &[Transaction],
        overlay: &Overlay,
        stats: &mut LineageSchedule,
    ) -> Option<Window> {
        if transactions.len() < 2 {
            return None;
        }
        let common = transactions[0]
            .lineage_command()
            .expect("Lineage segment")
            .targets[0]
            .reference
            .id();
        if transactions.iter().all(|tx| {
            tx.lineage_command().is_some_and(|command| {
                command
                    .targets
                    .iter()
                    .any(|target| target.reference.id() == common)
            })
        }) {
            // This window is chained through one shared Head.
            return None;
        }
        let started = Instant::now();
        #[cfg(feature = "native-rolling-lineage-execution")]
        let mut initial_heads = super::Heads::new();
        let prefix = plan(
            transactions,
            &self.network,
            |tx| tx.keeper_schedule().is_none(),
            |id| {
                let head = current_head(self, &overlay.lineages, id)?;
                #[cfg(feature = "native-rolling-lineage-execution")]
                initial_heads.insert(id.clone(), head.clone());
                Ok(head)
            },
        );
        stats.access_analysis_us += started.elapsed().as_micros() as u64;
        prefix
            .iter()
            .enumerate()
            .any(|(position, &required)| required < position)
            .then_some(Window {
                prefix,
                #[cfg(feature = "native-rolling-lineage-execution")]
                initial_heads,
            })
    }

    pub(in crate::native_store) fn prepare_lineage_lookahead(
        &self,
        transactions: &[Transaction],
        block: &BlockContext<'_>,
        origin: &Origin,
        width: usize,
        budget: &mut Budget,
        overlay: &mut Overlay,
        receipts: &mut Vec<Receipt>,
        fees: &mut u64,
        stats: &mut LineageSchedule,
    ) -> Result<usize> {
        let horizon = transactions
            .len()
            .min(self.lineage_window_limit(width, *budget));
        let transactions = &transactions[..horizon];
        let Some(window) = self.plan_lineage_window(transactions, overlay, stats) else {
            return Ok(0);
        };

        #[cfg(feature = "native-rolling-lineage-execution")]
        {
            self.drive_lineage_window(
                transactions,
                &window,
                block,
                width,
                budget,
                overlay,
                stats,
                |tx, budget, overlay, stats, material, attempted| {
                    self.settle_lineage_lookahead(
                        tx, block, origin, budget, overlay, receipts, fees, stats, material,
                        attempted,
                    )?;
                    Ok(false)
                },
            )?;
            Ok(window.prefix.len())
        }
        #[cfg(not(feature = "native-rolling-lineage-execution"))]
        self.prepare_lineage_waves(
            transactions,
            &window.prefix,
            block,
            origin,
            budget,
            overlay,
            receipts,
            fees,
            stats,
            |budget| self.funded_lineage_jobs(width, budget),
        )
    }

    #[cfg(not(feature = "native-rolling-lineage-execution"))]
    fn prepare_lineage_waves(
        &self,
        transactions: &[Transaction],
        prefix: &[usize],
        block: &BlockContext<'_>,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
        receipts: &mut Vec<Receipt>,
        fees: &mut u64,
        stats: &mut LineageSchedule,
        max_jobs: impl Fn(Budget) -> usize,
    ) -> Result<usize> {
        let mut prepared: Vec<Option<Materialized>> = (0..prefix.len()).map(|_| None).collect();
        let mut attempted = vec![false; prefix.len()];
        for position in 0..prefix.len() {
            let slot = *budget;
            let funded = max_jobs(slot);
            if prepared[position].is_none() && funded > 1 {
                let ready: Vec<_> = (position..prefix.len())
                    .filter(|&index| !attempted[index] && prefix[index] <= position)
                    .take(funded)
                    .collect();
                if ready.len() > 1 {
                    let immediate = ready
                        .iter()
                        .enumerate()
                        .take_while(|&(offset, &index)| position + offset == index)
                        .count();
                    let started = Instant::now();
                    let results: Vec<_> = ready
                        .par_iter()
                        .map(|&index| {
                            self.materialize_lineage(&transactions[index], block, overlay, slot)
                        })
                        .collect();
                    stats.parallel_materialize_us += started.elapsed().as_micros() as u64;
                    stats.parallel_waves += 1;
                    stats.parallel_commands += results.len() as u64;
                    stats.lookahead_commands += (results.len() - immediate) as u64;
                    for (index, result) in ready.into_iter().zip(results) {
                        attempted[index] = true;
                        prepared[index] = result.ok().flatten();
                    }
                }
            }

            self.settle_lineage_lookahead(
                &transactions[position],
                block,
                origin,
                budget,
                overlay,
                receipts,
                fees,
                stats,
                prepared[position].take(),
                attempted[position],
            )?;
        }
        Ok(prefix.len())
    }

    fn settle_lineage_lookahead(
        &self,
        tx: &Transaction,
        block: &BlockContext<'_>,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
        receipts: &mut Vec<Receipt>,
        fees: &mut u64,
        stats: &mut LineageSchedule,
        material: Option<Materialized>,
        attempted: bool,
    ) -> Result<()> {
        // Worker errors and tighter resources use the existing ordered path.
        // Successful materialization alone publishes no effects.
        let material = self.reuse_lineage_materialization(material, *budget, attempted, stats);
        let receipt = self.stage(
            tx,
            block,
            origin,
            budget,
            overlay,
            material,
            |resolved, limit| self.cache.verify(resolved, limit, false),
        )?;
        *fees = fees
            .checked_add(receipt.fee)
            .context("block fee overflow")?;
        receipts.push(receipt);
        Ok(())
    }

    pub(in crate::native_store) fn reuse_lineage_materialization(
        &self,
        material: Option<Materialized>,
        budget: Budget,
        attempted: bool,
        stats: &mut LineageSchedule,
    ) -> Option<Materialized> {
        let material = material.filter(|work| {
            budget.cycles >= self.limits.transaction_cycles
                && work.reads <= budget.reads
                && work.read_bytes <= budget.read_bytes
        });
        if material.is_some() {
            stats.lookahead_reused += 1;
        } else {
            stats.serial_commands += 1;
            if attempted {
                stats.lookahead_fallbacks += 1;
            }
        }
        material
    }
}

#[cfg(test)]
mod tests {
    use ckb_types::{
        packed::{Byte32, CellDep, OutPoint},
        prelude::*,
    };
    use stem_ledger::lineage::{Command, Head, LineageRef, Target};

    use super::*;

    #[test]
    fn reader_waits_for_possible_successor_consumption_while_other_head_can_start() -> Result<()> {
        let network = [1; 32].pack();
        let executor: Byte32 = [2; 32].pack();
        let code = OutPoint::new([3; 32].pack(), 0);
        let make = |fee, head: u8, dep: Option<OutPoint>| {
            let deps = std::iter::once(code.clone())
                .chain(dep)
                .map(|point| CellDep::new_builder().out_point(point).build())
                .collect();
            Transaction::new_lineage_command(
                deps,
                OutPoint::new([4; 32].pack(), fee),
                9,
                Command {
                    targets: vec![Target {
                        reference: LineageRef::new([head; 32].pack(), None)?,
                        latest_bounds: None,
                        min_capacity: 0,
                    }],
                    executor: executor.clone(),
                    executor_dep: code.clone(),
                    executor_cycles: 1_000,
                    validation_cycles: 1_000,
                    fee: 1,
                    payload: Default::default(),
                },
            )
        };
        let first = make(0, 5, None)?;
        let successor = OutPoint::new(first.id(&network), 1);
        let graph = plan(
            &[
                first,
                make(1, 5, None)?,
                make(2, 6, Some(successor))?,
                make(3, 7, None)?,
            ],
            &network,
            |_| true,
            |id| {
                Ok(Some(Head {
                    out_point: OutPoint::new(id.clone(), 0),
                    version: 0,
                    executor: executor.clone(),
                }))
            },
        );
        // The second producer may consume the first producer's output.
        // The fourth candidate touches neither producer nor reader.
        assert_eq!(graph, [0, 1, 2, 0]);
        Ok(())
    }
}
