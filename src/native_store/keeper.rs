//! Authenticated one-shot Lineage scheduling.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_types::{packed::OutPoint, prelude::*};
use stem_ledger::{
    BlockContext, Origin, ResolvedTransaction, Transaction,
    block::{HeaderContext, Receipt},
    keeper::{self, Action, TimedEntry, Trigger},
};

use super::{Budget, Limits, Overlay, Proof, StateReadFault, Store, charge, lineage};

const CONFIG: &[u8] = b"keeper/policy";
const QUEUE: &[u8] = b"keeper/due/";
const AGENDA: &[u8] = b"keeper/time";
const COMMAND: &[u8] = b"keeper/time-command/";
const MAX_AGENDA_BYTES: u64 = keeper::agenda_bytes(keeper::MAX_TIMED_PENDING) as u64;
const FEE: &[u8] = b"keeper/fee/";

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeeperPolicy {
    pub max_delay: u16,
    pub per_height: u16,
    /// Nonrefundable reservation fee in shannons, in addition to resource fees.
    pub reservation_fee: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<KeeperTimePolicy>,
}

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeeperTimePolicy {
    pub per_height: u16,
    pub max_delay_ms: u64,
}

impl KeeperPolicy {
    pub(super) fn validate(self, limits: Limits) -> Result<()> {
        ensure!(
            self.max_delay > 0
                && self.reservation_fee > 0
                && (1..=keeper::MAX_PER_HEIGHT).contains(&usize::from(self.per_height))
                && self.per_height as usize + self.time_slots() <= keeper::MAX_PER_HEIGHT
                && (self.per_height as usize + self.time_slots())
                    < limits.transaction_count as usize
                && self.time.is_none_or(|time| self.max_delay >= 2
                    && time.per_height > 0
                    && time.max_delay_ms > 0),
            "invalid Keeper policy"
        );
        let slot = self.slot(limits);
        ensure!(
            slot.bytes >= keeper::MAX_COMMAND_BYTES as u64
                && slot.reads >= 4
                && slot.read_bytes >= keeper::MAX_COMMAND_BYTES as u64 + 128
                && slot.write_bytes >= 128
                && slot.cycles > 0,
            "block limits cannot reserve Keeper slots"
        );
        Ok(())
    }

    pub(super) fn record(self) -> (&'static [u8], Vec<u8>) {
        let mut record = Vec::with_capacity(22);
        record.extend(self.max_delay.to_le_bytes());
        record.extend(self.per_height.to_le_bytes());
        record.extend(self.reservation_fee.to_le_bytes());
        if let Some(time) = self.time {
            record.extend(time.per_height.to_le_bytes());
            record.extend(time.max_delay_ms.to_le_bytes());
        }
        (CONFIG, record)
    }

    fn time_slots(self) -> usize {
        self.time.map_or(0, |time| usize::from(time.per_height))
    }

    fn slot(self, limits: Limits) -> Budget {
        // Keep one share of each block resource for submitted transactions.
        let shares = u64::from(self.per_height) + self.time_slots() as u64 + 1;
        let shared_reads = u64::from(self.time.is_some());
        let shared_bytes = shared_reads * (MAX_AGENDA_BYTES + AGENDA.len() as u64);
        Budget {
            bytes: limits.transaction_bytes / shares,
            cycles: limits.transaction_cycles.min(limits.block_cycles / shares),
            reads: limits.cell_reads.saturating_sub(shared_reads) / shares,
            read_bytes: limits.cell_read_bytes.saturating_sub(shared_bytes) / shares,
            write_bytes: limits.cell_write_bytes.saturating_sub(shared_bytes) / shares,
        }
    }
}

#[derive(Default)]
pub(super) struct Stage {
    queues: BTreeMap<u64, Vec<Transaction>>,
    reservations: BTreeMap<OutPoint, Option<Trigger>>,
    agenda: Option<Vec<TimedEntry>>,
    timed_commands: BTreeMap<OutPoint, Option<Transaction>>,
    pub running: bool,
}

pub(super) struct Change {
    queue: QueueChange,
    point: OutPoint,
    reservation: Option<Trigger>,
    pub fee: u64,
    pub cycles: u64,
}

enum QueueChange {
    Height(u64, Vec<Transaction>),
    Timed(Vec<TimedEntry>, Option<Transaction>),
}

#[derive(Default)]
pub(super) struct Due {
    pub commands: Vec<Transaction>,
    remaining_agenda: Option<Vec<TimedEntry>>,
}

impl Stage {
    fn queue(&self, store: &Store, height: u64) -> Result<(Vec<Transaction>, u64)> {
        if let Some(queue) = self.queues.get(&height) {
            return Ok((queue.clone(), queue_size(queue)));
        }
        let Some(record) = store.system_record(&queue_key(height))? else {
            return Ok((vec![], 0));
        };
        let queue = keeper::decode_queue(&record, height).context(StateReadFault)?;
        Ok((queue, record.len() as u64))
    }

    fn agenda(&self, store: &Store) -> Result<(Vec<TimedEntry>, u64)> {
        if let Some(agenda) = &self.agenda {
            return Ok((agenda.clone(), keeper::agenda_bytes(agenda.len()) as u64));
        }
        let Some(record) = store.system_record(AGENDA)? else {
            return Ok((vec![], 0));
        };
        let agenda = keeper::decode_agenda(&record).context(StateReadFault)?;
        Ok((agenda, record.len() as u64))
    }

    fn reservation(&self, store: &Store, point: &OutPoint) -> Result<Option<Trigger>> {
        match self.reservations.get(point) {
            Some(trigger) => Ok(*trigger),
            None => store
                .system_record(&reservation_key(point))?
                .map(|value| Trigger::decode(&value))
                .transpose()
                .context(StateReadFault),
        }
    }

    pub(super) fn apply(&mut self, change: Change) {
        match change.queue {
            QueueChange::Height(height, queue) => {
                self.queues.insert(height, queue);
            }
            QueueChange::Timed(agenda, command) => {
                self.agenda = Some(agenda);
                self.timed_commands.insert(change.point.clone(), command);
            }
        }
        self.reservations.insert(change.point, change.reservation);
    }

    pub(super) fn finish(self) -> impl Iterator<Item = (Vec<u8>, Option<Vec<u8>>)> {
        self.queues
            .into_iter()
            .map(|(height, queue)| {
                (
                    queue_key(height),
                    (!queue.is_empty()).then(|| keeper::encode_queue(&queue).to_vec()),
                )
            })
            .chain(self.reservations.into_iter().map(|(point, trigger)| {
                (
                    reservation_key(&point),
                    trigger.map(|trigger| trigger.encode().to_vec()),
                )
            }))
            .chain(self.agenda.into_iter().map(|entries| {
                (
                    AGENDA.to_vec(),
                    (!entries.is_empty()).then(|| keeper::encode_agenda(&entries).to_vec()),
                )
            }))
            .chain(self.timed_commands.into_iter().map(|(point, command)| {
                (
                    timed_key(&point),
                    command.map(|command| command.as_slice().to_vec()),
                )
            }))
    }
}

pub(super) fn change(
    store: &Store,
    stage: &Stage,
    carrier: &ResolvedTransaction,
    block: BlockContext<'_>,
    budget: &mut Budget,
    fresh: impl FnMut(&OutPoint) -> Result<stem_ledger::LiveCell>,
) -> Result<Option<Change>> {
    let tx = carrier.transaction();
    let Some(action) = tx.keeper_action() else {
        return Ok(None);
    };
    let policy = store.keeper.context("Keeper is not configured")?;
    let (trigger, point, command) = match action {
        Action::Schedule(raw) => {
            let command = keeper::decode_command(raw)?;
            (
                command.keeper_trigger().expect("bound command"),
                fee_point(&command),
                Some(command),
            )
        }
        Action::Cancel { trigger, fee_input } => (*trigger, fee_input.clone(), None),
    };
    if let Some(command) = &command {
        let height = block.candidate.height;
        let latest = height
            .checked_add(u64::from(policy.max_delay))
            .context("Keeper horizon overflow")?;
        match trigger {
            Trigger::Height(target) => ensure!(
                height < target && target <= latest,
                "Keeper deadline outside the reservation horizon"
            ),
            Trigger::Time(target) => {
                let time = policy
                    .time
                    .context("Keeper time triggers are not configured")?;
                let now = block.parent.context.timestamp_ms;
                let expiry = command.valid_before_height().expect("bound command");
                let earliest = height.checked_add(1).context("Keeper height overflow")?;
                ensure!(
                    target > now
                        && target - now <= time.max_delay_ms
                        && expiry > earliest
                        && expiry <= latest,
                    "Keeper deadline outside the reservation horizon"
                );
            }
        }
    }
    let reserved = stage.reservation(store, &point)?;
    let consumed = tx.inputs().any(|input| input.previous_output == point);
    ensure!(
        if command.is_some() {
            !consumed && reserved.is_none()
        } else {
            consumed && reserved == Some(trigger)
        },
        "Keeper reservation requires a fresh fee Cell or an exact cancellation input"
    );
    let (queue, old_bytes, new_bytes) = match trigger {
        Trigger::Height(height) => {
            let (mut queue, old_bytes) = stage.queue(store, height)?;
            let entry = queue.binary_search_by_key(&point, fee_point);
            if let Some(command) = &command {
                ensure!(
                    queue.len() < usize::from(policy.per_height),
                    "Keeper height is full"
                );
                queue.insert(entry.err().context(StateReadFault)?, command.clone());
            } else {
                queue.remove(entry.ok().context(StateReadFault)?);
            }
            let new_bytes = queue_size(&queue);
            (
                QueueChange::Height(height, queue),
                record_bytes(&point, old_bytes),
                record_bytes(&point, new_bytes),
            )
        }
        Trigger::Time(time) => {
            let (mut agenda, old_size) = stage.agenda(store)?;
            let entry = agenda.binary_search_by(|entry| entry.order_key().cmp(&(time, &point)));
            if let Some(command) = &command {
                ensure!(
                    agenda.len() < keeper::MAX_TIMED_PENDING,
                    "Keeper time agenda is full"
                );
                agenda.insert(
                    entry.err().context(StateReadFault)?,
                    TimedEntry {
                        timestamp_ms: time,
                        valid_before_height: command.valid_before_height().expect("bound command"),
                        fee_input: point.clone(),
                    },
                );
            } else {
                agenda.remove(entry.ok().context(StateReadFault)?);
            }
            let old_bytes = timed_record_bytes(&point, old_size, 0);
            let new_bytes = timed_record_bytes(
                &point,
                keeper::agenda_bytes(agenda.len()) as u64,
                command
                    .as_ref()
                    .map_or(0, |cmd| cmd.as_slice().len() as u64),
            );
            (
                QueueChange::Timed(agenda, command.clone()),
                old_bytes,
                new_bytes,
            )
        }
    };
    charge(&mut budget.reads, 2, "Keeper queue reads")?;
    charge(&mut budget.read_bytes, old_bytes, "Keeper queue read bytes")?;
    let (reservation, fee, cycles) = if let Some(command) = command {
        ensure!(
            carrier.fee() >= policy.reservation_fee,
            "insufficient Keeper reservation fee"
        );
        let terms = command.lineage_command().expect("Keeper command terms");
        let slot = policy.slot(store.limits);
        let escrow = lineage::resolve_fee(store, &command, terms, block, fresh)?.resolved;
        let authorization_limit = slot
            .cycles
            .checked_sub(terms.executor_cycles)
            .and_then(|left| left.checked_sub(terms.validation_cycles))
            .context("signed Lineage budgets exceed a Keeper slot")?;
        // The future fee Cell authorizes this particular command now. The
        // Keeper revalidates the actual fee and all business scripts when due.
        charge(
            &mut budget.reads,
            escrow.cell_reads() as u64 + 1,
            "Keeper authorization reads",
        )?;
        charge(
            &mut budget.read_bytes,
            escrow.resolved_cell_bytes() as u64 + escrow.inputs()[0].cell.as_slice().len() as u64,
            "Keeper authorization read bytes",
        )?;
        let (cycles, _) = lineage::verify_authorization(
            store,
            &command,
            escrow,
            authorization_limit.min(budget.cycles),
        )?;
        charge(&mut budget.cycles, cycles, "Keeper authorization cycles")?;
        (Some(trigger), policy.reservation_fee, cycles)
    } else {
        (None, 0, 0)
    };
    charge(
        &mut budget.write_bytes,
        new_bytes,
        "Keeper queue write bytes",
    )?;
    Ok(Some(Change {
        queue,
        point,
        reservation,
        fee,
        cycles,
    }))
}

impl Store {
    pub fn prove_keeper_queue(&self, height: u64) -> Result<(Option<Vec<u8>>, Proof)> {
        self.prove_system(&queue_key(height))
    }

    pub fn verify_keeper_queue(
        network: &ckb_types::packed::Byte32,
        header: &stem_ledger::block::Header,
        height: u64,
        record: Option<&[u8]>,
        proof: &Proof,
    ) -> bool {
        let tree = stem_ledger::patricia::Patricia::new(
            network.clone(),
            stem_ledger::patricia::Namespace::System,
        );
        tree.verify(
            &super::as_key(&header.state.system_root),
            &tree.record_key(&queue_key(height)),
            record.map(|bytes| tree.value_hash(bytes)).as_ref(),
            proof,
        )
    }

    pub fn prove_keeper_agenda(&self) -> Result<(Option<Vec<u8>>, Proof)> {
        self.prove_system(AGENDA)
    }

    pub fn prove_keeper_timed_command(
        &self,
        fee_input: &OutPoint,
    ) -> Result<(Option<Vec<u8>>, Proof)> {
        self.prove_system(&timed_key(fee_input))
    }

    pub fn verify_keeper_agenda(
        network: &ckb_types::packed::Byte32,
        header: &stem_ledger::block::Header,
        record: Option<&[u8]>,
        proof: &Proof,
    ) -> bool {
        Self::verify_system_record(network, header, AGENDA, record, proof)
    }

    pub fn verify_keeper_timed_command(
        network: &ckb_types::packed::Byte32,
        header: &stem_ledger::block::Header,
        fee_input: &OutPoint,
        record: Option<&[u8]>,
        proof: &Proof,
    ) -> bool {
        Self::verify_system_record(network, header, &timed_key(fee_input), record, proof)
    }

    pub(super) fn due(&self, height: u64, budget: &mut Budget) -> Result<Due> {
        let Some(policy) = self.keeper else {
            return Ok(Due::default());
        };
        let stage = Stage::default();
        let (mut commands, _) = stage.queue(self, height)?;
        ensure!(
            commands.len() <= usize::from(policy.per_height),
            StateReadFault
        );
        if policy.time.is_none() {
            return Ok(Due {
                commands,
                remaining_agenda: None,
            });
        }
        let (agenda, bytes) = stage.agenda(self)?;
        charge(&mut budget.reads, 1, "Keeper agenda reads")?;
        charge(
            &mut budget.read_bytes,
            AGENDA.len() as u64 + bytes,
            "Keeper agenda bytes",
        )?;
        let mut expired: Vec<_> = agenda
            .iter()
            .filter(|entry| height >= entry.valid_before_height)
            .collect();
        // Expiry order bounds how long a burst can keep older expired records
        // pending. Ready entries retain the signed time and OutPoint order.
        expired.sort_unstable_by(|left, right| {
            (left.valid_before_height, left.order_key())
                .cmp(&(right.valid_before_height, right.order_key()))
        });
        let time = self.tip.context.timestamp_ms;
        let ready = agenda
            .iter()
            .filter(|entry| height < entry.valid_before_height && time >= entry.timestamp_ms);
        let selected: Vec<_> = expired
            .into_iter()
            .chain(ready)
            .take(policy.time_slots())
            .collect();
        for entry in &selected {
            let record = self
                .system_record(&timed_key(&entry.fee_input))?
                .context(StateReadFault)?;
            let command = keeper::decode_command(&record).context(StateReadFault)?;
            ensure!(
                fee_point(&command) == entry.fee_input
                    && command.keeper_trigger() == Some(Trigger::Time(entry.timestamp_ms))
                    && command.valid_before_height() == Some(entry.valid_before_height),
                StateReadFault
            );
            commands.push(command);
        }
        let remaining_agenda = if selected.is_empty() {
            None
        } else {
            let remaining = agenda
                .iter()
                .filter(|entry| !selected.contains(entry))
                .cloned()
                .collect::<Vec<_>>();
            charge(
                &mut budget.write_bytes,
                AGENDA.len() as u64 + keeper::agenda_bytes(remaining.len()) as u64,
                "Keeper agenda writes",
            )?;
            Some(remaining)
        };
        Ok(Due {
            commands,
            remaining_agenda,
        })
    }

    pub(super) fn run_keeper(
        &self,
        due: &Due,
        context: &HeaderContext,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
    ) -> Result<Vec<Receipt>> {
        if due.commands.is_empty() {
            return Ok(vec![]);
        }
        let slot = self.keeper.expect("Keeper is enabled").slot(self.limits);
        let mut receipts = Vec::with_capacity(due.commands.len());
        overlay.keeper.running = true;
        overlay.keeper.agenda = due.remaining_agenda.clone();
        for command in &due.commands {
            let point = fee_point(command);
            let trigger = command.keeper_trigger().context(StateReadFault)?;
            ensure!(
                overlay.keeper.reservation(self, &point)? == Some(trigger),
                StateReadFault
            );
            let read_bytes = match trigger {
                Trigger::Height(_) => record_bytes(&point, command.as_slice().len() as u64 + 8),
                Trigger::Time(_) => timed_record_bytes(&point, 0, command.as_slice().len() as u64),
            };
            let write_bytes = match trigger {
                Trigger::Height(height) => {
                    overlay.keeper.queues.insert(height, vec![]);
                    record_bytes(&point, 0)
                }
                Trigger::Time(_) => {
                    overlay.keeper.timed_commands.insert(point.clone(), None);
                    timed_record_bytes(&point, 0, 0)
                }
            };
            let mut remaining = slot;
            charge(&mut remaining.reads, 2, "Keeper slot reads")?;
            charge(
                &mut remaining.read_bytes,
                read_bytes,
                "Keeper slot read bytes",
            )?;
            charge(
                &mut remaining.write_bytes,
                write_bytes,
                "Keeper slot write bytes",
            )?;
            let receipt = match self.stage(
                command,
                context,
                origin,
                &mut remaining,
                overlay,
                None,
                |resolved, limit| self.cache.verify(resolved, limit, false),
            ) {
                Ok(receipt) => receipt,
                Err(error) if error.is::<StateReadFault>() => return Err(error),
                Err(_) => {
                    // The prepaid reservation purchased the attempt. A stale
                    // reference or unaffordable candidate never changes a Cell.
                    remaining = Budget::empty();
                    Receipt {
                        fee: 0,
                        cycles: slot.cycles,
                        protocol_result: stem_ledger::lineage::Outcome::Skipped.encode(),
                    }
                }
            };
            budget.charge_slot(slot, remaining)?;
            overlay.keeper.reservations.insert(point, None);
            receipts.push(receipt);
        }
        overlay.keeper.running = false;
        Ok(receipts)
    }
}

fn fee_point(tx: &Transaction) -> OutPoint {
    tx.input(0)
        .expect("validated Keeper fee input")
        .previous_output
}

fn queue_size(queue: &[Transaction]) -> u64 {
    if queue.is_empty() {
        0
    } else {
        keeper::encode_queue(queue).len() as u64
    }
}

fn record_bytes(point: &OutPoint, queue_bytes: u64) -> u64 {
    (QUEUE.len() + 8 + FEE.len() + point.as_slice().len() + Trigger::SIZE) as u64 + queue_bytes
}

fn queue_key(height: u64) -> Vec<u8> {
    [QUEUE, height.to_be_bytes().as_slice()].concat()
}

fn reservation_key(point: &OutPoint) -> Vec<u8> {
    [FEE, point.as_slice()].concat()
}

fn timed_record_bytes(point: &OutPoint, agenda_bytes: u64, command_bytes: u64) -> u64 {
    (AGENDA.len() + COMMAND.len() + FEE.len() + 2 * point.as_slice().len() + Trigger::SIZE) as u64
        + agenda_bytes
        + command_bytes
}

fn timed_key(point: &OutPoint) -> Vec<u8> {
    [COMMAND, point.as_slice()].concat()
}
