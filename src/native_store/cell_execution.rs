//! Resolve ordinary transactions against the preceding declared Cell edits.
//! The block adopts worker results only in proposal order.

use anyhow::{Context, Result};
use ckb_types::packed::OutPoint;
use rayon::prelude::*;
use stem_ledger::{BlockContext, LiveCell, Origin, Transaction, block::Receipt};

use super::{Budget, LiveChanges, Overlay, Store, charge};
use std::collections::{HashMap, hash_map::Entry};

const MAX_WINDOW: usize = 32;
const MAX_DECLARED_EDITS: usize = 64;
pub(super) const MAX_SPECULATIVE_READ_BYTES: usize = 4 * 1024 * 1024;

struct Write {
    position: usize,
    cell: Option<LiveCell>,
}

struct History {
    first: Write,
    later: Vec<Write>,
}

type Writes = HashMap<OutPoint, History>;

fn record(writes: &mut Writes, point: OutPoint, edit: Write) {
    match writes.entry(point) {
        Entry::Occupied(mut entry) => entry.get_mut().later.push(edit),
        Entry::Vacant(entry) => {
            entry.insert(History {
                first: edit,
                later: Vec::new(),
            });
        }
    }
}

/// Contains only immutable Cell data. System-state effects are published by
/// the ordinary staging path before a later result can become authoritative.
pub(super) struct PrecedingCells<'a> {
    base: &'a LiveChanges,
    writes: &'a Writes,
    position: usize,
}

impl PrecedingCells<'_> {
    pub(super) fn get(&self, point: &OutPoint) -> Option<Option<&LiveCell>> {
        if let Some(history) = self.writes.get(point)
            && history.first.position < self.position
        {
            let cutoff = history
                .later
                .partition_point(|edit| edit.position < self.position);
            let edit = cutoff
                .checked_sub(1)
                .map_or(&history.first, |i| &history.later[i]);
            return Some(edit.cell.as_ref());
        }
        self.base.get(point).map(Option::as_ref)
    }
}

struct Executed {
    receipt: Receipt,
    cells: LiveChanges,
    remaining: Budget,
}

impl Store {
    fn has_declared_cell_writes(&self, tx: &Transaction) -> bool {
        tx.inputs().len().saturating_add(tx.outputs().len()) <= MAX_DECLARED_EDITS
            && tx.header_deps().len() <= MAX_DECLARED_EDITS
            && tx.operation().is_none()
            && !tx.has_lineage_command()
            && tx.lineage_batch().is_none()
            && tx.lineage_intent().is_none()
            && tx.keeper_action().is_none()
            && tx.keeper_schedule().is_none()
            && tx.l1_reference_certificate().is_none()
            && tx.messages().len() == 0
            && self
                .bridge
                .as_ref()
                .is_none_or(|bridge| tx.outputs().all(|cell| !bridge.is_burn(&cell.lock())))
    }

    pub(super) fn prepare_cell_window(
        &self,
        transactions: &[Transaction],
        block: &BlockContext<'_>,
        origin: &Origin,
        width: usize,
        budget: &mut Budget,
        overlay: &mut Overlay,
        receipts: &mut Vec<Receipt>,
        fees: &mut u64,
    ) -> Result<usize> {
        let slot = *budget;
        let funded = usize::try_from(slot.cycles / self.limits.transaction_cycles.max(1))
            .unwrap_or(usize::MAX);
        let count = transactions
            .iter()
            .take(width.saturating_mul(4).min(MAX_WINDOW).min(funded))
            .take_while(|tx| self.has_declared_cell_writes(tx))
            .count();
        if count < 2 {
            return Ok(0);
        }
        let window = &transactions[..count];
        let mut writes = Writes::with_capacity(window.len() * 2);
        for (position, tx) in window.iter().enumerate() {
            for input in tx.inputs() {
                record(
                    &mut writes,
                    input.previous_output,
                    Write {
                        position,
                        cell: None,
                    },
                );
            }
            let id = tx.id(&self.network);
            for (index, cell) in tx.outputs().enumerate() {
                record(
                    &mut writes,
                    OutPoint::new(id.clone(), u32::try_from(index)?),
                    Write {
                        position,
                        cell: Some(LiveCell {
                            cell,
                            origin: origin.clone(),
                        }),
                    },
                );
            }
        }
        let completed: Vec<Result<Executed>> = window
            .par_iter()
            .enumerate()
            .map(|(position, tx)| {
                let view = PrecedingCells {
                    base: &overlay.live,
                    writes: &writes,
                    position,
                };
                let mut remaining = slot;
                let mut work = self.overlay(None);
                let receipt = self.stage_transaction(
                    tx,
                    block,
                    origin,
                    &mut remaining,
                    &mut work,
                    None,
                    |resolved, limit| self.cache.verify(resolved, limit, count < width),
                    None,
                    Some(&view),
                )?;
                Ok(Executed {
                    receipt,
                    cells: work.live,
                    remaining,
                })
            })
            .collect();

        let mut use_speculation = true;
        for (tx, prepared) in window.iter().zip(completed) {
            let mut next = *budget;
            let receipt = match prepared {
                Ok(executed)
                    if use_speculation
                        && next.charge_cell_work(slot, executed.remaining).is_ok() =>
                {
                    overlay.live.extend(executed.cells);
                    *budget = next;
                    executed.receipt
                }
                _ => {
                    // Once ordered execution takes over, subsequent jobs must
                    // see its actual edits rather than the projected prefix.
                    use_speculation = false;
                    self.stage(
                        tx,
                        block,
                        origin,
                        budget,
                        overlay,
                        None,
                        |resolved, limit| self.cache.verify(resolved, limit, false),
                    )?
                }
            };
            *fees = fees
                .checked_add(receipt.fee)
                .context("block fee overflow")?;
            receipts.push(receipt);
        }
        Ok(count)
    }
}

impl Budget {
    fn charge_cell_work(&mut self, slot: Self, remaining: Self) -> Result<()> {
        charge(
            &mut self.bytes,
            slot.bytes - remaining.bytes,
            "block transaction bytes",
        )?;
        charge(
            &mut self.reads,
            slot.reads - remaining.reads,
            "block Cell reads",
        )?;
        charge(
            &mut self.read_bytes,
            slot.read_bytes - remaining.read_bytes,
            "block Cell read bytes",
        )?;
        charge(
            &mut self.write_bytes,
            slot.write_bytes - remaining.write_bytes,
            "block Cell write bytes",
        )?;
        charge(
            &mut self.cycles,
            slot.cycles - remaining.cycles,
            "block VM cycles",
        )
    }
}
