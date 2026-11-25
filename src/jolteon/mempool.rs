use std::collections::{BTreeMap, BTreeSet};

use stem_ledger::{
    Transaction,
    keeper::{Action, Trigger, decode_command},
};
use stem_stake::Hash32;

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
    entries: BTreeMap<Hash32, (Transaction, Deadline)>,
    heights: BTreeSet<(u64, Hash32)>,
    parent_times: BTreeSet<(u64, Hash32)>,
    bytes: usize,
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
        self.entries.values().map(|(tx, _)| tx)
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
        self.entries.insert(id, (tx, deadline));
        Ok(())
    }

    pub(super) fn remove(&mut self, id: &Hash32) {
        if let Some((tx, deadline)) = self.entries.remove(id) {
            if let Some(height) = deadline.height {
                self.heights.remove(&(height, *id));
            }
            if let Some(time) = deadline.parent_time {
                self.parent_times.remove(&(time, *id));
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
