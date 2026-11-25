use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use stem_ledger::{
    Transaction,
    keeper::{Action, Trigger, decode_command},
};
use stem_stake::Hash32;

const RELAY_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Deadline {
    height: Option<u64>,
    parent_time: Option<u64>,
}

impl Deadline {
    fn for_transaction(tx: &Transaction) -> anyhow::Result<Self> {
        let mut deadline = Self {
            height: tx.valid_before_height(),
            parent_time: None,
        };
        if let Some(Action::Schedule(raw)) = tx.keeper_action() {
            let command = decode_command(raw)?;
            let schedule = command.keeper_schedule().expect("validated Keeper command");
            match schedule.trigger {
                Trigger::Height(target) => {
                    deadline.height =
                        Some(deadline.height.map_or(target, |height| height.min(target)));
                }
                Trigger::Time(target) => deadline.parent_time = Some(target),
            }
        }
        Ok(deadline)
    }
}

#[derive(Default)]
pub(super) struct Mempool {
    entries: BTreeMap<Hash32, Entry>,
    heights: BTreeSet<(u64, Hash32)>,
    parent_times: BTreeSet<(u64, Hash32)>,
    relays: BTreeSet<(Instant, Hash32)>,
    bytes: usize,
}

struct Entry {
    tx: Transaction,
    deadline: Deadline,
    relay: Option<PendingRelay>,
}

struct PendingRelay {
    at: Instant,
    epoch: u64,
    cursor: usize,
}

impl Mempool {
    pub(super) fn contains(&self, id: &Hash32) -> bool {
        self.entries.contains_key(id)
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn bytes(&self) -> usize {
        self.bytes
    }

    pub(super) fn transactions(&self) -> impl Iterator<Item = &Transaction> {
        self.entries.values().map(|entry| &entry.tx)
    }

    pub(super) fn insert(&mut self, id: Hash32, tx: Transaction) -> anyhow::Result<()> {
        let deadline = Deadline::for_transaction(&tx)?;
        if let Some(height) = deadline.height {
            self.heights.insert((height, id));
        }
        if let Some(time) = deadline.parent_time {
            self.parent_times.insert((time, id));
        }
        self.bytes += tx.as_slice().len();
        self.entries.insert(
            id,
            Entry {
                tx,
                deadline,
                relay: None,
            },
        );
        Ok(())
    }

    pub(super) fn track_local(&mut self, id: Hash32, now: Instant, epoch: u64) -> bool {
        let entry = self
            .entries
            .get_mut(&id)
            .expect("admitted Relay transaction");
        if entry.relay.is_some() {
            return false;
        }
        let at = now + RELAY_INTERVAL;
        entry.relay = Some(PendingRelay {
            at,
            epoch,
            cursor: 0,
        });
        self.relays.insert((at, id));
        true
    }

    /// Retry the oldest ready transaction. Moving its deadline to the back
    /// leaves other ready transactions eligible even under sustained load.
    pub(super) fn retry(
        &mut self,
        now: Instant,
        epoch: u64,
        members: usize,
    ) -> Option<(&Transaction, usize)> {
        let (at, id) = self.relays.first().copied().filter(|(at, _)| *at <= now)?;
        self.relays.remove(&(at, id));
        let entry = self
            .entries
            .get_mut(&id)
            .expect("tracked Relay transaction");
        let relay = entry.relay.as_mut().expect("local Relay transaction");
        if relay.epoch != epoch {
            relay.epoch = epoch;
            relay.cursor = 0;
        }
        let cursor = relay.cursor;
        relay.cursor += 1;
        // An unconfirmed entry may have become invalid after admission.
        // Stop automatic retries after one pass through the committee.
        if relay.cursor == members {
            entry.relay = None;
        } else {
            relay.at = now + RELAY_INTERVAL;
            self.relays.insert((relay.at, id));
        }
        Some((&entry.tx, cursor))
    }

    pub(super) fn remove(&mut self, id: &Hash32) {
        if let Some(Entry {
            tx,
            deadline,
            relay,
        }) = self.entries.remove(id)
        {
            if let Some(height) = deadline.height {
                self.heights.remove(&(height, *id));
            }
            if let Some(time) = deadline.parent_time {
                self.parent_times.remove(&(time, *id));
            }
            if let Some(relay) = relay {
                self.relays.remove(&(relay.at, *id));
            }
            self.bytes -= tx.as_slice().len();
        }
    }

    /// Finality makes an elapsed deadline irreversible. Candidate branches and
    /// temporarily full Keeper queues leave admission entries intact.
    pub(super) fn expire(&mut self, finalized_height: u64, finalized_time: u64) {
        let next_height = finalized_height.saturating_add(1);
        while let Some(&(height, id)) = self.heights.first() {
            if height > next_height {
                break;
            }
            self.remove(&id);
        }
        while let Some(&(time, id)) = self.parent_times.first() {
            if time > finalized_time {
                break;
            }
            self.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_retries_are_local_bounded_and_removed_with_the_entry() -> anyhow::Result<()> {
        let at = Instant::now();
        let mut pool = Mempool::default();
        let transaction = Transaction::new(vec![], vec![], vec![], vec![], vec![])?;
        let local = transaction.with_valid_before_height(12)?;
        let peer = transaction.with_valid_before_height(20)?;
        pool.insert([1; 32], local)?;
        pool.insert([2; 32], peer)?;
        assert!(pool.track_local([1; 32], at, 0));
        assert!(!pool.track_local([1; 32], at, 0));
        assert!(pool.retry(at, 0, 2).is_none());
        for (step, epoch, index) in [(1, 0, 0), (2, 1, 0), (3, 1, 1)] {
            let now = at + step * RELAY_INTERVAL;
            let (tx, cursor) = pool.retry(now, epoch, 2).expect("due local entry");
            assert_eq!(cursor, index);
            assert_eq!(tx.valid_before_height(), Some(12));
            assert!(pool.retry(now, epoch, 2).is_none());
        }
        let now = at + 4 * RELAY_INTERVAL;
        assert!(pool.retry(now, 1, 2).is_none());
        assert!(pool.track_local([1; 32], now, 1));
        pool.expire(11, 0);
        assert!(pool.retry(now + RELAY_INTERVAL, 1, 2).is_none());
        assert_eq!(pool.len(), 1);
        assert!(pool.contains(&[2; 32]));
        Ok(())
    }
}
