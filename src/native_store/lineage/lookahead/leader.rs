//! Use the Lineage dependency plan without changing greedy proposal selection.

use std::{borrow::Borrow, iter::Peekable};

use anyhow::Result;
use stem_ledger::{BlockContext, Origin, Transaction, block::Receipt};

use super::Materialized;
use crate::native_store::{
    Budget, LineageSchedule, Overlay, ProposalProgress, ProposalScan, ProposalStep, ProposalTrial,
    Store,
};

impl Store {
    pub(in crate::native_store) fn stage_lineage_proposal<T: Borrow<Transaction>>(
        &self,
        input: &mut Peekable<impl Iterator<Item = T>>,
        block: &BlockContext<'_>,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
        progress: &mut ProposalProgress,
        scan: &mut ProposalScan,
        keeper_count: usize,
    ) -> Result<Option<bool>> {
        let width = rayon::current_num_threads();
        let available =
            self.limits.transaction_count as usize - progress.transactions.len() - keeper_count;
        let limit = self.lineage_window_limit(width, *budget).min(available);
        if limit < 2 {
            return Ok(None);
        }
        let mut transactions = Vec::new();
        while transactions.len() < limit
            && input.peek().is_some_and(|item| {
                let tx = item.borrow();
                tx.lineage_command().is_some() && tx.keeper_schedule().is_none()
            })
        {
            transactions.push(
                input
                    .next()
                    .expect("peeked Lineage transaction")
                    .borrow()
                    .clone(),
            );
        }
        if transactions.is_empty() {
            return Ok(None);
        }
        let mut stats = LineageSchedule::default();
        let window = self.plan_lineage_window(&transactions, overlay, &mut stats);
        let mut settle = |tx: &Transaction,
                          budget: &mut Budget,
                          overlay: &mut Overlay,
                          _stats: &mut LineageSchedule,
                          material: Option<Materialized>,
                          attempted: bool| {
            let step = self.stage_proposal_entry(
                tx,
                block,
                origin,
                budget,
                overlay,
                progress,
                scan,
                ProposalTrial::<fn(&mut Budget, &mut Overlay) -> Option<Receipt>>::Lineage(
                    material, attempted,
                ),
                None,
            )?;
            Ok::<_, anyhow::Error>(
                matches!(step, ProposalStep::Stop)
                    || progress.transactions.len() + keeper_count
                        == self.limits.transaction_count as usize
                    || budget.bytes == 0
                    || progress.wire_bytes == 0,
            )
        };
        let mut completed = 0;
        if let Some(window) = window {
            // The plan covers all potential writes before each readiness
            // threshold. Greedy rejections can only remove those writes.
            if self.drive_lineage_window(
                &transactions,
                &window,
                block,
                width,
                budget,
                overlay,
                &mut stats,
                &mut settle,
            )? {
                scan.lineage.record(stats);
                return Ok(Some(true));
            }
            completed = window.prefix.len();
        }
        // A dependency the planner could not inspect uses the ordered path.
        // Its position and the rest of the buffered candidates are retained.
        for tx in &transactions[completed..] {
            if settle(tx, budget, overlay, &mut stats, None, false)? {
                scan.lineage.record(stats);
                return Ok(Some(true));
            }
        }
        scan.lineage.record(stats);
        Ok(Some(false))
    }
}
