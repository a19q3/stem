//! Keep independently prepared Keeper commands until their canonical slots.

use std::time::Instant;

use rayon::prelude::*;
use stem_ledger::{BlockContext, Transaction};

use crate::native_store::{
    Budget, KeeperWakeMetrics, Overlay, Store,
    lineage::{Materialized, access_plan::plan, current_head},
};

use super::wake_budget;

#[derive(Default)]
pub(super) struct WakePreparation {
    first: usize,
    required_prefix: Vec<usize>,
    prepared: Vec<Option<Materialized>>,
    attempted: Vec<bool>,
}

impl WakePreparation {
    pub(super) fn take(
        &mut self,
        store: &Store,
        commands: &[Transaction],
        position: usize,
        block: &BlockContext<'_>,
        overlay: &Overlay,
        slot: Budget,
        workers: usize,
        metrics: &mut KeeperWakeMetrics,
    ) -> Option<Materialized> {
        if workers < 2 || commands.len() < 2 {
            return None;
        }
        if position >= self.first + self.required_prefix.len() {
            self.first = position;
            let height = block.candidate().height;
            let time = block.parent().context.timestamp_ms;
            let started = Instant::now();
            self.required_prefix = plan(
                &commands[position..],
                store.network(),
                |tx| {
                    tx.keeper_schedule().is_some_and(|schedule| {
                        schedule.retry.is_none()
                            && schedule.ready(height, time)
                            && tx
                                .valid_before_height()
                                .is_some_and(|expiry| height < expiry)
                    })
                },
                |id| current_head(store, &overlay.lineages, id),
            );
            metrics.access_us += started.elapsed().as_micros() as u64;
            self.prepared = (0..self.required_prefix.len()).map(|_| None).collect();
            self.attempted = vec![false; self.required_prefix.len()];
        }
        let index = position - self.first;
        if index >= self.required_prefix.len() {
            return None;
        }
        if self.prepared[index].is_none() {
            let ready: Vec<_> = (index..self.required_prefix.len())
                .filter(|&item| !self.attempted[item] && self.required_prefix[item] <= index)
                .take(workers)
                .collect();
            if ready.len() > 1 {
                let started = Instant::now();
                let results: Vec<_> = ready
                    .par_iter()
                    .map(|&item| {
                        let tx = &commands[self.first + item];
                        let remaining = wake_budget(slot, tx)?;
                        store.materialize_lineage(tx, block, overlay, remaining)
                    })
                    .collect();
                metrics.materialize_us += started.elapsed().as_micros() as u64;
                metrics.jobs += results.len() as u64;
                for (item, result) in ready.into_iter().zip(results) {
                    self.attempted[item] = true;
                    self.prepared[item] = result.ok().flatten();
                }
            }
        }
        let result = self.prepared[index].take();
        if result.is_some() {
            metrics.reused += 1;
        } else if self.attempted[index] {
            metrics.fallbacks += 1;
        }
        result
    }
}
