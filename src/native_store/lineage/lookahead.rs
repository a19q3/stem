//! Plan materialization against an ordered block prefix.

use std::time::Instant;

use anyhow::{Context, Result};
use rayon::prelude::*;
use stem_ledger::{BlockContext, Origin, Transaction, block::Receipt};

use super::{Materialized, access_plan::plan, current_head};
use crate::native_store::{Budget, LineageSchedule, Overlay, Store};

const MAX_LOOKAHEAD: usize = 32;

impl Store {
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
        let limit = self.limits.transaction_cycles;
        let max_jobs = |budget: Budget| {
            usize::try_from(budget.cycles / limit.max(1))
                .unwrap_or(usize::MAX)
                .min(width)
        };
        let horizon = transactions
            .len()
            .min(width.saturating_mul(4))
            .min(MAX_LOOKAHEAD);
        if horizon < 2 || max_jobs(*budget) < 2 {
            return Ok(0);
        }
        let transactions = &transactions[..horizon];
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
            return Ok(0);
        }
        let started = Instant::now();
        let prefix = plan(
            transactions,
            &self.network,
            |tx| tx.keeper_schedule().is_none(),
            |id| current_head(self, &overlay.lineages, id),
        );
        stats.access_analysis_us += started.elapsed().as_micros() as u64;
        if !prefix
            .iter()
            .enumerate()
            .any(|(position, &required)| required < position)
        {
            return Ok(0);
        }

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

            // Worker errors and tighter resources use the existing ordered
            // path. Successful materialization alone publishes no effects.
            let material = prepared[position].take().filter(|work| {
                budget.cycles >= limit
                    && work.reads <= budget.reads
                    && work.read_bytes <= budget.read_bytes
            });
            if material.is_some() {
                stats.lookahead_reused += 1;
            } else {
                stats.serial_commands += 1;
                if attempted[position] {
                    stats.lookahead_fallbacks += 1;
                }
            }
            let receipt = self.stage(
                &transactions[position],
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
        }
        Ok(prefix.len())
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
