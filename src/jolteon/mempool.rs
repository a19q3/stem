use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

#[cfg(feature = "jolteon-shared-proposal-snapshot")]
use imbl::{OrdMap, ordmap::Entry as MapEntry};
#[cfg(not(feature = "jolteon-shared-proposal-snapshot"))]
use std::collections::{BTreeMap, btree_map::Entry as MapEntry};
use stem_ledger::{
    Transaction,
    keeper::{Action, Trigger, decode_command},
};
use stem_stake::Hash32;

use crate::native_store::ProposalScan;

const RELAY_INTERVAL: Duration = Duration::from_secs(2);
#[cfg(feature = "jolteon-aged-proposal-selection")]
const PROPOSAL_PRIORITY_AGE: Duration = Duration::from_secs(5);
#[cfg(feature = "jolteon-aged-proposal-selection")]
const MAX_AGED_PROPOSAL_VISITS: usize = 256;

#[cfg_attr(feature = "jolteon-shared-proposal-snapshot", derive(Clone))]
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

#[cfg(feature = "jolteon-shared-proposal-snapshot")]
type Entries = OrdMap<Hash32, Entry>;
#[cfg(not(feature = "jolteon-shared-proposal-snapshot"))]
type Entries = BTreeMap<Hash32, Entry>;
#[cfg(all(feature = "jolteon-aged-proposal-selection", feature = "jolteon-shared-proposal-snapshot"))]
type Arrivals = imbl::OrdSet<(Instant, Hash32)>;
#[cfg(all(feature = "jolteon-aged-proposal-selection", not(feature = "jolteon-shared-proposal-snapshot")))]
type Arrivals = BTreeSet<(Instant, Hash32)>;

#[derive(Default)]
pub(super) struct Mempool {
    entries: Entries,
    heights: BTreeSet<(u64, Hash32)>,
    parent_times: BTreeSet<(u64, Hash32)>,
    relays: BTreeSet<(Instant, Hash32)>,
    #[cfg(feature = "jolteon-aged-proposal-selection")]
    arrivals: Arrivals,
    bytes: usize,
}

#[cfg_attr(feature = "jolteon-shared-proposal-snapshot", derive(Clone))]
struct Entry {
    tx: Transaction,
    deadline: Deadline,
    relay: Option<PendingRelay>,
    #[cfg(feature = "jolteon-aged-proposal-selection")]
    accepted: Instant,
}

#[cfg_attr(feature = "jolteon-shared-proposal-snapshot", derive(Clone))]
struct PendingRelay {
    at: Instant,
    epoch: u64,
    cursor: usize,
}

/// One worker holds a previous copy-on-write root. The Replica remains the
/// sole writer of both indexes and never waits for the worker to traverse them.
#[cfg(feature = "jolteon-shared-proposal-snapshot")]
pub(super) struct ProposalSnapshot {
    entries: Entries,
    #[cfg(feature = "jolteon-aged-proposal-selection")]
    arrivals: Arrivals,
    #[cfg(feature = "jolteon-aged-proposal-selection")]
    at: Instant,
}

#[cfg(feature = "jolteon-shared-proposal-snapshot")]
impl ProposalSnapshot {
    pub(super) fn candidates(&self) -> (impl Iterator<Item = &Transaction>, ProposalScan) {
        #[cfg(feature = "jolteon-aged-proposal-selection")]
        return transactions_at(&self.entries, &self.arrivals, self.at);
        #[cfg(not(feature = "jolteon-aged-proposal-selection"))]
        (self.entries.values().map(|entry| &entry.tx), ProposalScan::default())
    }
}

#[cfg(feature = "jolteon-aged-proposal-selection")]
fn transactions_at<'a>(
    entries: &'a Entries,
    arrivals: &'a Arrivals,
    now: Instant,
) -> (impl Iterator<Item = &'a Transaction>, ProposalScan) {
    let aged = arrivals
        .iter()
        .take(MAX_AGED_PROPOSAL_VISITS)
        .take_while(move |(at, _)| now.saturating_duration_since(*at) >= PROPOSAL_PRIORITY_AGE);
    // The promoted entries form a prefix of the arrival index. Skip exactly
    // that prefix when walking the regular hash order.
    let mut count = 0;
    let boundary = aged.clone().inspect(|_| count += 1).last().copied();
    let transactions = aged.map(|(_, id)| &entries[id].tx).chain(
        entries
            .iter()
            .filter(move |(id, entry)| boundary.is_none_or(|last| (entry.accepted, **id) > last))
            .map(|(_, entry)| &entry.tx),
    );
    let scan = ProposalScan {
        priority_candidates: count as u64,
        priority_at_limit: u64::from(count == MAX_AGED_PROPOSAL_VISITS),
        ..ProposalScan::default()
    };
    (transactions, scan)
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

    #[cfg(not(any(feature = "jolteon-aged-proposal-selection", feature = "jolteon-shared-proposal-snapshot")))]
    pub(super) fn proposal_candidates(&self) -> (impl Iterator<Item = &Transaction>, ProposalScan) {
        (self.entries.values().map(|entry| &entry.tx), ProposalScan::default())
    }

    #[cfg(all(feature = "jolteon-aged-proposal-selection", not(feature = "jolteon-shared-proposal-snapshot")))]
    pub(super) fn proposal_candidates(&self) -> (impl Iterator<Item = &Transaction>, ProposalScan) {
        transactions_at(&self.entries, &self.arrivals, Instant::now())
    }

    #[cfg(feature = "jolteon-shared-proposal-snapshot")]
    pub(super) fn proposal_snapshot(&self) -> ProposalSnapshot {
        ProposalSnapshot {
            entries: self.entries.clone(),
            #[cfg(feature = "jolteon-aged-proposal-selection")]
            arrivals: self.arrivals.clone(),
            #[cfg(feature = "jolteon-aged-proposal-selection")]
            at: Instant::now(),
        }
    }

    pub(super) fn insert(&mut self, id: Hash32, tx: Transaction) -> anyhow::Result<()> {
        let MapEntry::Vacant(entry) = self.entries.entry(id) else {
            return Ok(());
        };
        let deadline = Deadline::for_transaction(&tx)?;
        if let Some(height) = deadline.height {
            self.heights.insert((height, id));
        }
        if let Some(time) = deadline.parent_time {
            self.parent_times.insert((time, id));
        }
        self.bytes += tx.as_slice().len();
        #[cfg(feature = "jolteon-aged-proposal-selection")]
        let accepted = Instant::now();
        #[cfg(feature = "jolteon-aged-proposal-selection")]
        self.arrivals.insert((accepted, id));
        entry.insert(Entry {
            tx,
            deadline,
            relay: None,
            #[cfg(feature = "jolteon-aged-proposal-selection")]
            accepted,
        });
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
            #[cfg(feature = "jolteon-aged-proposal-selection")]
            accepted,
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
            #[cfg(feature = "jolteon-aged-proposal-selection")]
            self.arrivals.remove(&(accepted, *id));
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
    use ckb_types::packed::OutPoint;
    use stem_ledger::CellInput;

    use super::*;

    #[cfg(feature = "jolteon-aged-proposal-selection")]
    #[test]
    fn aged_proposal_order_is_bounded_and_entries_are_visited_once() -> anyhow::Result<()> {
        let before = Instant::now();
        let mut pool = Mempool::default();
        let input = CellInput::new(OutPoint::default(), None);
        let transaction = Transaction::new(vec![], vec![], vec![input], vec![], vec![])?;
        for height in (1..=260_u64).rev() {
            let mut id = [0; 32];
            id[30..].copy_from_slice(&(height as u16).to_be_bytes());
            pool.insert(id, transaction.with_valid_before_height(height)?)?;
        }
        let heights = |pool: &Mempool, at| {
            transactions_at(&pool.entries, &pool.arrivals, at)
                .0.map(|tx| tx.valid_before_height().unwrap()).collect::<Vec<_>>()
        };
        assert_eq!(heights(&pool, before), (1..=260).collect::<Vec<_>>());
        assert_eq!(transactions_at(&pool.entries, &pool.arrivals, before).1.priority_candidates, 0);
        let cutoff = pool.arrivals.iter().next_back().unwrap().0 + PROPOSAL_PRIORITY_AGE;
        let scan = transactions_at(&pool.entries, &pool.arrivals, cutoff).1;
        assert_eq!(scan.priority_candidates, MAX_AGED_PROPOSAL_VISITS as u64);
        assert_eq!(scan.priority_at_limit, 1);
        let ordered = heights(&pool, cutoff);
        let priority = pool
            .arrivals
            .iter()
            .take(MAX_AGED_PROPOSAL_VISITS)
            .map(|(_, id)| pool.entries[id].tx.valid_before_height().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(&ordered[..MAX_AGED_PROPOSAL_VISITS], priority.as_slice());
        assert!(ordered[MAX_AGED_PROPOSAL_VISITS..].is_sorted());
        assert_eq!(ordered.iter().copied().collect::<BTreeSet<_>>().len(), 260);

        let (_, oldest) = *pool.arrivals.iter().next().unwrap();
        let bytes = pool.bytes();
        pool.insert(oldest, transaction.with_valid_before_height(300)?)?;
        assert_eq!(pool.bytes(), bytes);
        assert_eq!(heights(&pool, cutoff), ordered);
        pool.remove(&oldest);
        pool.expire(4, 0);
        assert_eq!(pool.arrivals.len(), pool.len());
        assert!(!pool.arrivals.iter().any(|(_, id)| *id == oldest));
        assert!(heights(&pool, cutoff).iter().all(|height| *height > 5));
        Ok(())
    }

    #[cfg(feature = "jolteon-shared-proposal-snapshot")]
    #[test]
    fn scheduled_candidates_keep_the_snapshot_order_and_membership() -> anyhow::Result<()> {
        let before = Instant::now();
        let mut pool = Mempool::default();
        let input = CellInput::new(OutPoint::default(), None);
        let tx = Transaction::new(vec![], vec![], vec![input], vec![], vec![])?;
        // Enough entries to exercise updates to different branches of the map.
        for height in (1..=128_u8).rev() {
            pool.insert([height; 32], tx.with_valid_before_height(u64::from(height))?)?;
        }
        #[cfg(not(feature = "jolteon-aged-proposal-selection"))]
        let snapshot = pool.proposal_snapshot();
        #[cfg(feature = "jolteon-aged-proposal-selection")]
        let snapshot = {
            let mut snapshot = pool.proposal_snapshot();
            // Set the capture time explicitly so the test has no timing race.
            snapshot.at = pool.arrivals.iter().next_back().unwrap().0 + PROPOSAL_PRIORITY_AGE;
            snapshot
        };

        let heights = |snapshot: &ProposalSnapshot| {
            snapshot.candidates().0.map(|tx| tx.valid_before_height().unwrap()).collect::<Vec<_>>()
        };
        let scheduled = heights(&snapshot);
        #[cfg(not(feature = "jolteon-aged-proposal-selection"))]
        assert_eq!(scheduled, (1..=128).collect::<Vec<_>>());
        #[cfg(feature = "jolteon-aged-proposal-selection")]
        assert_eq!(scheduled, pool.arrivals.iter().map(|(_, id)| u64::from(id[0])).collect::<Vec<_>>());
        pool.expire(63, 0);
        pool.remove(&[128; 32]);
        pool.insert([63; 32], tx.with_valid_before_height(200)?)?;
        assert!(pool.track_local([65; 32], before, 0));

        assert_eq!(heights(&snapshot), scheduled);
        let fresh = pool.proposal_snapshot();
        #[cfg(feature = "jolteon-aged-proposal-selection")]
        let fresh = ProposalSnapshot { at: before, ..fresh };
        assert_eq!(heights(&fresh), [vec![200], (65..=127).collect()].concat());
        #[cfg(feature = "jolteon-aged-proposal-selection")]
        assert_eq!(snapshot.candidates().1.priority_candidates, 128);
        Ok(())
    }

    #[test]
    fn relay_retries_are_local_bounded_and_removed_with_the_entry() -> anyhow::Result<()> {
        let at = Instant::now();
        let mut pool = Mempool::default();
        let input = CellInput::new(OutPoint::default(), None);
        let transaction = Transaction::new(vec![], vec![], vec![input], vec![], vec![])?;
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
