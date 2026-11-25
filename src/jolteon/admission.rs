use std::time::Instant;

use anyhow::{Context, Result, ensure};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;
use stem_ledger::{MAX_TRANSACTION_BYTES, Transaction};
use stem_stake::Hash32;
use tokio::sync::oneshot;

use super::{Incoming, MAX_MEMPOOL_BYTES, Replica, wire};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub struct Metrics {
    pub requests: u64,
    pub groups: u64,
    pub checks: u64,
    pub parallel_groups: u64,
    pub max_group: usize,
    pub verification_us: u64,
}

struct Request {
    id: Hash32,
    raw: Vec<u8>,
    reply: Option<oneshot::Sender<Vec<u8>>>,
}

impl Replica {
    pub(super) fn admit_transactions(&mut self, events: Vec<Incoming>) -> Result<()> {
        let mut requests = Vec::with_capacity(events.len());
        for event in events {
            if event.reply.is_none() {
                if self.peer_index(event.frame.sender).is_err() {
                    self.rejected = self.rejected.saturating_add(1);
                    continue;
                }
                if !self.accept_epoch(event.frame.epoch) {
                    continue;
                }
            }
            requests.push(Request {
                id: ckb_hash::blake2b_256(&event.frame.body),
                raw: event.frame.body,
                reply: event.reply,
            });
        }
        if requests.is_empty() {
            return Ok(());
        }

        let mut jobs: Vec<&Request> = Vec::with_capacity(requests.len());
        let order: Vec<_> = requests
            .iter()
            .map(|request| {
                jobs.iter()
                    .position(|job| job.id == request.id)
                    .unwrap_or_else(|| {
                        let index = jobs.len();
                        jobs.push(request);
                        index
                    })
            })
            .collect();
        let started = Instant::now();
        // Each check has its own speculative overlay over the same finalized tip.
        // Mutating the mempool and sending replies stay on the replica thread.
        let checked: Vec<Result<Option<Transaction>>> = self.workers.install(|| {
            jobs.par_iter()
                .map(|request| {
                    if self.mempool.contains(&request.id) || !self.admission_fits(request.raw.len())
                    {
                        return Ok(None);
                    }
                    let tx = Transaction::from_slice(&request.raw)?;
                    self.store.check(&tx)?;
                    Ok(Some(tx))
                })
                .collect()
        });
        let checks = checked
            .iter()
            .filter(|result| !matches!(result, Ok(None)))
            .count() as u64;
        self.admission.requests = self
            .admission
            .requests
            .saturating_add(requests.len() as u64);
        self.admission.groups = self.admission.groups.saturating_add(1);
        self.admission.checks = self.admission.checks.saturating_add(checks);
        self.admission.parallel_groups = self
            .admission
            .parallel_groups
            .saturating_add(u64::from(checks > 1));
        self.admission.max_group = self.admission.max_group.max(requests.len());
        self.admission.verification_us = self
            .admission
            .verification_us
            .saturating_add(started.elapsed().as_micros().try_into().unwrap_or(u64::MAX));

        for (request, index) in requests.into_iter().zip(order) {
            let result = self.admit_checked(&request, &checked[index]);
            if let Some(reply) = request.reply {
                let response = match result {
                    Ok(inserted) => {
                        if inserted {
                            self.net.broadcast(wire::RELAY_TX, request.raw);
                        }
                        json!({"admitted": hex::encode(request.id)})
                    }
                    Err(error) => json!({"error": format!("{error:#}")}),
                };
                let _ = reply.send(serde_json::to_vec(&response)?);
            } else if result.is_err() {
                self.rejected = self.rejected.saturating_add(1);
            }
        }
        Ok(())
    }

    fn admission_fits(&self, bytes: usize) -> bool {
        bytes <= MAX_TRANSACTION_BYTES
            && self.mempool.bytes().saturating_add(bytes) <= MAX_MEMPOOL_BYTES
    }

    fn admit_checked(
        &mut self,
        request: &Request,
        checked: &Result<Option<Transaction>>,
    ) -> Result<bool> {
        if self.mempool.contains(&request.id) {
            return Ok(false);
        }
        ensure!(
            self.admission_fits(request.raw.len()),
            "mempool capacity exceeded"
        );
        let tx = checked
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error:#}"))?
            .as_ref()
            .context("missing admission verification")?;
        self.mempool.insert(request.id, tx.clone())?;
        Ok(true)
    }
}
