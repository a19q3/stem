//! Plan materialization against an ordered block prefix.

use std::{collections::BTreeMap, time::Instant};

use anyhow::{Context, Result};
use ckb_types::packed::{Byte32, OutPoint};
use rayon::prelude::*;
use stem_ledger::{BlockContext, Origin, Transaction, block::Receipt, lineage::Head};

use super::{Materialized, current_head};
use crate::native_store::{Budget, LineageSchedule, Overlay, Store};

const MAX_LOOKAHEAD: usize = 32;

#[derive(Default)]
struct Access {
    read: usize,
    write: usize,
}

/// Earliest accepted prefix length for each candidate within the window.
fn plan(
    transactions: &[Transaction],
    network: &Byte32,
    mut load_head: impl FnMut(&Byte32) -> Result<Option<Head>>,
) -> Vec<usize> {
    let mut positions = Vec::with_capacity(transactions.len());
    let mut heads: BTreeMap<Byte32, Vec<OutPoint>> = BTreeMap::new();
    let mut accesses: BTreeMap<OutPoint, Access> = BTreeMap::new();
    'planning: for (index, tx) in transactions.iter().enumerate() {
        let Some(command) = tx.lineage_command() else {
            break;
        };
        if tx.keeper_schedule().is_some()
            || tx.lineage_batch().is_some()
            || tx
                .lineage_intent()
                .is_some_and(|intent| intent.batch.is_some())
        {
            break;
        }
        let mut reads = Vec::with_capacity(tx.cell_deps().len());
        for dep in tx.cell_deps() {
            // A Dep Group can read a member which is also written in the
            // proposed prefix. Its expansion is left to ordered staging.
            if dep.dep_type().as_slice() != [0] {
                break 'planning;
            }
            reads.push(dep.out_point());
        }
        let id = tx.id(network);
        let mut writes = vec![
            tx.input(0)
                .expect("validated Lineage fee input")
                .previous_output,
            OutPoint::new(id.clone(), 0),
        ];
        for (output, target) in command.targets.iter().enumerate() {
            let target_id = target.reference.id();
            let points = match heads.entry(target_id.clone()) {
                std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
                std::collections::btree_map::Entry::Vacant(entry) => {
                    let Ok(Some(head)) = load_head(target_id) else {
                        // Resolve missing or damaged state at its ordered
                        // position so earlier block errors retain priority.
                        break 'planning;
                    };
                    entry.insert(vec![head.out_point])
                }
            };
            // A prior command can leave the old Head live through fee-only
            // rollback. All its possible successors must remain in the set.
            writes.extend(points.iter().cloned());
            let successor = OutPoint::new(id.clone(), output as u32 + 1);
            writes.push(successor.clone());
            points.push(successor);
        }

        let read_after_write = reads
            .iter()
            .filter_map(|point| accesses.get(point).map(|access| access.write))
            .max()
            .unwrap_or_default();
        let conflicting_write = writes
            .iter()
            .filter_map(|point| {
                accesses
                    .get(point)
                    .map(|access| access.read.max(access.write))
            })
            .max()
            .unwrap_or_default();
        positions.push(read_after_write.max(conflicting_write));
        for point in reads {
            accesses.entry(point).or_default().read = index + 1;
        }
        for point in writes {
            accesses.entry(point).or_default().write = index + 1;
        }
    }
    positions
}

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
        let prefix = plan(transactions, &self.network, |id| {
            current_head(self, &overlay.lineages, id)
        });
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
    use ckb_types::{packed::CellDep, prelude::*};
    use stem_ledger::lineage::{Command, LineageRef, Target};

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
