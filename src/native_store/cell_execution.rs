//! Resolve ordinary transactions against the preceding declared Cell edits.
//! The block adopts worker results only in proposal order.

use anyhow::{Context, Result};
use ckb_types::packed::OutPoint;
use rayon::prelude::*;
use stem_ledger::{BlockContext, LiveCell, Origin, Transaction, block::Receipt};

use super::{Budget, LiveChanges, Overlay, ProposalScan, Store, charge};
use std::{
    borrow::Borrow,
    cell::Cell,
    collections::{HashMap, hash_map::Entry},
    iter::{Peekable, Zip},
    time::Instant,
    vec::IntoIter,
};

#[cfg(feature = "native-shared-cell-deps")]
mod dependencies;
#[cfg(feature = "native-shared-cell-deps")]
use super::DependencyReadStats;
#[cfg(feature = "native-shared-cell-deps")]
pub(super) use dependencies::DependencyReads;
#[cfg(any(feature = "native-rolling-cell-execution", feature = "native-rolling-leader-execution"))]
mod rolling;

const MAX_WINDOW: usize = u32::BITS as usize;
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

fn project<T: Borrow<Transaction>>(
    network: &ckb_types::packed::Byte32,
    transactions: &[T],
    origin: &Origin,
) -> Result<Writes> {
    let mut writes = Writes::with_capacity(transactions.len() * 2);
    for (position, tx) in transactions.iter().enumerate() {
        let tx = tx.borrow();
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
        let id = tx.id(network);
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
    Ok(writes)
}

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
    projection: Option<ProjectedPrefix<'a>>,
    #[cfg(feature = "native-shared-cell-deps")]
    dependencies: dependencies::HintReader<'a>,
}

struct ProjectedPrefix<'a> {
    writes: &'a Writes,
    position: usize,
    sources: Sources<'a>,
}

enum Sources<'a> {
    Window(&'a Cell<u32>),
    #[cfg(any(feature = "native-rolling-cell-execution", feature = "native-rolling-leader-execution"))]
    Run(&'a std::cell::RefCell<rolling::SourcePositions>),
}

impl PrecedingCells<'_> {
    pub(super) fn is_speculative(&self) -> bool {
        self.projection.is_some()
    }

    #[cfg(feature = "native-shared-cell-deps")]
    pub(super) fn parent_hint(&self, point: &OutPoint) -> Option<&Option<LiveCell>> {
        self.dependencies.get(point)
    }

    pub(super) fn get(&self, point: &OutPoint) -> Option<Option<&LiveCell>> {
        if let Some(prefix) = &self.projection
            && let Some(history) = prefix.writes.get(point)
            && history.first.position < prefix.position
        {
            let cutoff = history
                .later
                .partition_point(|edit| edit.position < prefix.position);
            let edit = cutoff
                .checked_sub(1)
                .map_or(&history.first, |i| &history.later[i]);
            match prefix.sources {
                Sources::Window(mask) => mask.set(mask.get() | (1 << edit.position)),
                #[cfg(any(feature = "native-rolling-cell-execution", feature = "native-rolling-leader-execution"))]
                Sources::Run(sources) => sources.borrow_mut().record(edit.position),
            }
            return Some(edit.cell.as_ref());
        }
        self.base.get(point).map(Option::as_ref)
    }
}

pub(super) struct Executed {
    effects: Effects,
    dependencies: u32,
    position_bit: u32,
}

impl Executed {
    fn adopt(
        self,
        slot: Budget,
        budget: &mut Budget,
        overlay: &mut Overlay,
        adopted: &mut u32,
    ) -> Option<Receipt> {
        // Every projected edit observed by this trial must already have been
        // published. An ordered fallback does not certify its projection.
        if self.dependencies & !*adopted != 0 {
            return None;
        }
        let receipt = self.effects.adopt(slot, budget, overlay)?;
        *adopted |= self.position_bit;
        Some(receipt)
    }
}

struct Effects {
    receipt: Receipt,
    cells: LiveChanges,
    remaining: Budget,
}

impl Effects {
    fn adopt(self, slot: Budget, budget: &mut Budget, overlay: &mut Overlay) -> Option<Receipt> {
        let mut next = *budget;
        next.charge_cell_work(slot, self.remaining).ok()?;
        overlay.live.extend(self.cells);
        *budget = next;
        Some(self.receipt)
    }
}

pub(super) struct ProposalWindow<T> {
    ready: Zip<IntoIter<T>, IntoIter<Result<Executed>>>,
    slot: Budget,
    adopted: u32,
    #[cfg(feature = "native-shared-cell-deps")]
    dependency_reads: Option<DependencyReads>,
}

impl<T: Borrow<Transaction>> ProposalWindow<T> {
    pub(super) fn new() -> Self {
        Self {
            ready: Vec::new().into_iter().zip(Vec::new()),
            slot: Budget::empty(),
            adopted: 0,
            #[cfg(feature = "native-shared-cell-deps")]
            dependency_reads: None,
        }
    }

    #[cfg(feature = "native-shared-cell-deps")]
    pub(super) fn dependency_stats(&self) -> DependencyReadStats {
        self.dependency_reads
            .as_ref()
            .map_or_else(DependencyReadStats::default, DependencyReads::stats)
    }

    #[cfg(all(feature = "native-shared-cell-deps", feature = "native-rolling-leader-execution"))]
    pub(super) fn shared_reads(&mut self) -> &mut Option<DependencyReads> {
        &mut self.dependency_reads
    }

    pub(super) fn next(
        &mut self,
        input: &mut Peekable<impl Iterator<Item = T>>,
        store: &Store,
        block: &BlockContext<'_>,
        origin: &Origin,
        overlay: &Overlay,
        budget: Budget,
        wire_bytes: u64,
        available: usize,
        width: usize,
        scan: &mut ProposalScan,
    ) -> Result<Option<(T, Option<Result<Executed>>)>> {
        if self.ready.len() == 0 {
            let Some(first) = input.next() else {
                return Ok(None);
            };
            let limit = store.cell_window_limit(width, budget).min(available);
            let mut remaining = (budget.bytes, wire_bytes);
            if limit < 2
                || !store.reserve_proposal_cell(first.borrow(), &mut remaining)
                || !input
                    .peek()
                    .is_some_and(|item| store.reserve_proposal_cell(item.borrow(), &mut remaining))
            {
                return Ok(Some((first, None)));
            }
            let mut items = Vec::with_capacity(limit);
            items.push(first);
            items.push(input.next().expect("peeked proposal transaction"));
            while items.len() < limit
                && input
                    .peek()
                    .is_some_and(|item| store.reserve_proposal_cell(item.borrow(), &mut remaining))
            {
                items.push(input.next().expect("peeked proposal transaction"));
            }
            let references: Vec<_> = items.iter().map(Borrow::borrow).collect();
            let started = Instant::now();
            #[cfg(feature = "native-shared-cell-deps")]
            let shared = self.dependency_reads.get_or_insert_with(|| {
                DependencyReads::collect(store, references.iter().copied())
            });
            let results = store.execute_cell_window(
                &references,
                block,
                origin,
                width,
                budget,
                overlay,
                #[cfg(feature = "native-shared-cell-deps")]
                shared,
            )?;
            scan.parallel_cell_us = scan
                .parallel_cell_us
                .saturating_add(u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX));
            scan.parallel_cell_jobs = scan.parallel_cell_jobs.saturating_add(results.len() as u64);
            self.ready = items.into_iter().zip(results);
            self.slot = budget;
            self.adopted = 0;
        }
        let (item, executed) = self.ready.next().expect("prepared proposal window");
        Ok(Some((item, Some(executed))))
    }

    pub(super) fn adopt(
        &mut self,
        executed: Executed,
        budget: &mut Budget,
        overlay: &mut Overlay,
    ) -> Option<Receipt> {
        executed.adopt(self.slot, budget, overlay, &mut self.adopted)
    }
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

    fn cell_window_limit(&self, width: usize, budget: Budget) -> usize {
        let funded = usize::try_from(budget.cycles / self.limits.transaction_cycles.max(1))
            .unwrap_or(usize::MAX);
        width.saturating_mul(4).min(MAX_WINDOW).min(funded)
    }

    fn reserve_proposal_cell(&self, tx: &Transaction, remaining: &mut (u64, u64)) -> bool {
        let len = tx.as_slice().len();
        let entry = super::proposal_entry_bytes(len);
        if len as u64 <= remaining.0 && entry <= remaining.1 && self.has_declared_cell_writes(tx) {
            remaining.0 -= len as u64;
            remaining.1 -= entry;
            true
        } else {
            false
        }
    }

    #[cfg(feature = "native-rolling-leader-execution")]
    pub(super) fn collect_proposal_run<T: Borrow<Transaction>>(
        &self,
        input: &mut Peekable<impl Iterator<Item = T>>,
        budget: Budget,
        wire_bytes: u64,
        available: usize,
        width: usize,
    ) -> Vec<Transaction> {
        let mut run = Vec::new();
        if self.cell_window_limit(width, budget).min(available) < 2 {
            return run;
        }
        let limit = available.min(4 * MAX_WINDOW);
        let mut remaining = (budget.bytes, wire_bytes);
        while run.len() < limit
            && input.peek().is_some_and(|item| self.reserve_proposal_cell(item.borrow(), &mut remaining))
        {
            run.push(input.next().expect("peeked transaction").borrow().clone());
        }
        run
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
        #[cfg(feature = "native-shared-cell-deps")] dependency_reads: &mut Option<DependencyReads>,
    ) -> Result<usize> {
        let slot = *budget;
        let count = transactions
            .iter()
            .take(self.cell_window_limit(width, slot))
            .take_while(|tx| self.has_declared_cell_writes(tx))
            .count();
        if count < 2 {
            return Ok(0);
        }
        let window = &transactions[..count];
        #[cfg(feature = "native-shared-cell-deps")]
        let shared = dependency_reads.get_or_insert_with(|| {
            let eligible = transactions.iter().take_while(|tx| self.has_declared_cell_writes(tx));
            DependencyReads::collect(self, eligible)
        });
        let completed = self.execute_cell_window(
            window,
            block,
            origin,
            width,
            slot,
            overlay,
            #[cfg(feature = "native-shared-cell-deps")]
            shared,
        )?;

        let mut adopted = 0;
        for (tx, prepared) in window.iter().zip(completed) {
            let receipt = prepared
                .ok()
                .and_then(|work| work.adopt(slot, budget, overlay, &mut adopted));
            let receipt = if let Some(receipt) = receipt {
                receipt
            } else {
                self.stage(
                    tx,
                    block,
                    origin,
                    budget,
                    overlay,
                    None,
                    |resolved, limit| self.cache.verify(resolved, limit, false),
                )?
            };
            *fees = fees
                .checked_add(receipt.fee)
                .context("block fee overflow")?;
            receipts.push(receipt);
        }
        Ok(count)
    }

    fn execute_cell_window<T: Borrow<Transaction> + Sync>(
        &self,
        window: &[T],
        block: &BlockContext<'_>,
        origin: &Origin,
        width: usize,
        slot: Budget,
        overlay: &Overlay,
        #[cfg(feature = "native-shared-cell-deps")] shared: &DependencyReads,
    ) -> Result<Vec<Result<Executed>>> {
        let writes = project(&self.network, window, origin)?;
        Ok(window
            .par_iter()
            .enumerate()
            .map(|(position, tx)| {
                let dependencies = Cell::new(0);
                let view = PrecedingCells {
                    base: &overlay.live,
                    projection: Some(ProjectedPrefix {
                        writes: &writes,
                        position,
                        sources: Sources::Window(&dependencies),
                    }),
                    #[cfg(feature = "native-shared-cell-deps")]
                    dependencies: shared.reader(),
                };
                let effects = self.execute_cell_trial(
                    tx.borrow(),
                    block,
                    origin,
                    slot,
                    &view,
                    window.len() < width,
                )?;
                Ok(Executed {
                    effects,
                    dependencies: dependencies.get(),
                    position_bit: 1 << position,
                })
            })
            .collect())
    }

    fn execute_cell_trial(
        &self,
        tx: &Transaction,
        block: &BlockContext<'_>,
        origin: &Origin,
        slot: Budget,
        view: &PrecedingCells<'_>,
        parallel_groups: bool,
    ) -> Result<Effects> {
        let mut remaining = slot;
        let mut work = self.overlay(None);
        let receipt = self.stage_transaction(
            tx,
            block,
            origin,
            &mut remaining,
            &mut work,
            None,
            |resolved, limit| self.cache.verify(resolved, limit, parallel_groups),
            None,
            Some(view),
        )?;
        Ok(Effects {
            receipt,
            cells: work.live,
            remaining,
        })
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
