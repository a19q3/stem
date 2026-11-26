//! Authenticated one-shot Lineage scheduling.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::*,
};
use stem_ledger::{
    BlockContext, Origin, ResolvedTransaction, Transaction,
    block::Receipt,
    keeper::{self, Action, AgendaEntry, Reservation, Schedule, Trigger},
};

use super::{Budget, Limits, Overlay, Proof, StateReadFault, Store, charge, lineage};

const CONFIG: &[u8] = b"keeper/policy";
const QUEUE: &[u8] = b"keeper/due/";
const AGENDA: &[u8] = b"keeper/time";
const COMMAND: &[u8] = b"keeper/time-command/";
const MAX_AGENDA_BYTES: u64 = keeper::agenda_bytes(keeper::MAX_AGENDA_PENDING) as u64;
const FEE: &[u8] = b"keeper/fee/";
const FEE_KEY_BYTES: u64 = (FEE.len() + OutPoint::TOTAL_SIZE) as u64;
const RESERVATION_BYTES: u64 = FEE_KEY_BYTES + Reservation::SIZE as u64;
const QUEUE_KEY_BYTES: u64 = (QUEUE.len() + 8) as u64;
const COMMAND_KEY_BYTES: u64 = (COMMAND.len() + OutPoint::TOTAL_SIZE) as u64;

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
        let record = stem_ledger::protocol::genesis::keeper_policy(
            self.max_delay, self.per_height, self.reservation_fee,
            self.time.map(|time| (time.per_height, time.max_delay_ms)),
        );
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
    reservations: BTreeMap<OutPoint, Option<Reservation>>,
    agenda: Option<Vec<AgendaEntry>>,
    timed_commands: BTreeMap<OutPoint, Option<Transaction>>,
    pub running: bool,
}

pub(super) struct Change {
    queue: QueueChange,
    primary_fee: OutPoint,
    fees: Vec<OutPoint>,
    reservation: Option<Reservation>,
    pub fee: u64,
    pub cycles: u64,
}

enum QueueChange {
    Height(u64, Vec<Transaction>),
    Agenda(Vec<AgendaEntry>, Option<Transaction>),
}

#[derive(Default)]
pub(super) struct Due {
    pub commands: Vec<Transaction>,
    agenda: Option<Vec<AgendaEntry>>,
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

    fn agenda(&self, store: &Store) -> Result<(Vec<AgendaEntry>, u64)> {
        if let Some(agenda) = &self.agenda {
            return Ok((agenda.clone(), keeper::agenda_bytes(agenda.len()) as u64));
        }
        let Some(record) = store.system_record(AGENDA)? else {
            return Ok((vec![], 0));
        };
        let agenda = keeper::decode_agenda(&record).context(StateReadFault)?;
        Ok((agenda, record.len() as u64))
    }

    fn reservation(&self, store: &Store, point: &OutPoint) -> Result<Option<Reservation>> {
        match self.reservations.get(point) {
            Some(reservation) => Ok(reservation.clone()),
            None => store
                .system_record(&reservation_key(point))?
                .map(|value| Reservation::decode(&value))
                .transpose()
                .context(StateReadFault),
        }
    }

    fn timed_command(&self, store: &Store, point: &OutPoint) -> Result<Transaction> {
        if let Some(command) = self.timed_commands.get(point) {
            return command.clone().context(StateReadFault);
        }
        let record = store
            .system_record(&timed_key(point))?
            .context(StateReadFault)?;
        keeper::decode_command(&record).context(StateReadFault)
    }

    pub(super) fn apply(&mut self, change: Change) {
        match change.queue {
            QueueChange::Height(height, queue) => {
                self.queues.insert(height, queue);
            }
            QueueChange::Agenda(agenda, command) => {
                self.agenda = Some(agenda);
                self.timed_commands.insert(change.primary_fee, command);
            }
        }
        self.reservations.extend(
            change
                .fees
                .into_iter()
                .map(|point| (point, change.reservation.clone())),
        );
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
            .chain(self.reservations.into_iter().map(|(point, reservation)| {
                (
                    reservation_key(&point),
                    reservation.map(|reservation| reservation.encode()),
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
    block: &BlockContext<'_>,
    budget: &mut Budget,
    fresh: impl FnMut(&OutPoint) -> Result<stem_ledger::LiveCell>,
) -> Result<Option<Change>> {
    let tx = carrier.transaction();
    let Some(action) = tx.keeper_action() else {
        return Ok(None);
    };
    let policy = store.keeper.context("Keeper is not configured")?;
    let (reservation, submitted, cancel) = match action {
        Action::Schedule(raw) => {
            let command = keeper::decode_command(raw)?;
            let schedule = command.keeper_schedule().expect("bound command");
            check_horizon(policy, &command, schedule, block)?;
            (
                Reservation {
                    trigger: schedule.trigger,
                    primary_fee: fee_point(&command),
                    command_id: command.id(store.network()),
                    in_agenda: schedule.uses_agenda(),
                },
                Some(command),
                None,
            )
        }
        Action::Cancel {
            trigger,
            fee_input,
            command_id,
        } => {
            ensure!(
                tx.inputs().any(|input| &input.previous_output == fee_input),
                "Keeper cancellation requires the exact reserved fee input"
            );
            let reservation = stage
                .reservation(store, fee_input)?
                .filter(|reservation| {
                    reservation.trigger == *trigger && &reservation.command_id == command_id
                })
                .context("Keeper cancellation does not match a reservation")?;
            (reservation, None, Some(fee_input))
        }
    };
    let point = &reservation.primary_fee;
    let (queue, affected, reads, old_bytes, new_bytes) =
        match (reservation.in_agenda, reservation.trigger) {
            (false, Trigger::Height(height)) => {
                let (mut queue, old_bytes) = stage.queue(store, height)?;
                let entry = queue.binary_search_by_key(point, fee_point);
                let affected = if let Some(command) = &submitted {
                    ensure!(
                        queue.len() < usize::from(policy.per_height),
                        "Keeper height is full"
                    );
                    let position = entry.err().context("Keeper fee Cell is already queued")?;
                    queue.insert(position, command.clone());
                    command.clone()
                } else {
                    queue.remove(entry.ok().context(StateReadFault)?)
                };
                let new_bytes = queue_size(&queue);
                (
                    QueueChange::Height(height, queue),
                    affected,
                    1,
                    QUEUE_KEY_BYTES + old_bytes,
                    QUEUE_KEY_BYTES + new_bytes,
                )
            }
            (true, trigger) => {
                let (mut agenda, old_size) = stage.agenda(store)?;
                let entry =
                    agenda.binary_search_by(|entry| entry.order_key().cmp(&(trigger, point)));
                let (affected, reads, command_read, command_size) =
                    if let Some(command) = &submitted {
                        ensure!(
                            agenda.len() < keeper::MAX_AGENDA_PENDING,
                            "Keeper time agenda is full"
                        );
                        let position = entry.err().context("Keeper fee Cell is already queued")?;
                        agenda.insert(
                            position,
                            AgendaEntry {
                                trigger,
                                eligible_height: match trigger {
                                    Trigger::Height(first) => first,
                                    Trigger::Time(_) => 0,
                                },
                                valid_before_height: command
                                    .valid_before_height()
                                    .expect("bound command"),
                                fee_input: point.clone(),
                                attempts_left: command
                                    .keeper_schedule()
                                    .expect("bound command")
                                    .attempts(),
                            },
                        );
                        (command.clone(), 1, 0, command.as_slice().len() as u64)
                    } else {
                        let command = stage.timed_command(store, point)?;
                        let entry = agenda.remove(entry.ok().context(StateReadFault)?);
                        ensure!(
                            command.valid_before_height() == Some(entry.valid_before_height),
                            StateReadFault
                        );
                        let bytes = command.as_slice().len() as u64;
                        (command, 2, COMMAND_KEY_BYTES + bytes, 0)
                    };
                let new_bytes = AGENDA.len() as u64
                    + keeper::agenda_bytes(agenda.len()) as u64
                    + COMMAND_KEY_BYTES
                    + command_size;
                (
                    QueueChange::Agenda(agenda, submitted.clone()),
                    affected,
                    reads,
                    AGENDA.len() as u64 + old_size + command_read,
                    new_bytes,
                )
            }
            (false, Trigger::Time(_)) => return Err(StateReadFault.into()),
        };
    ensure!(
        &fee_point(&affected) == point
            && affected
                .keeper_schedule()
                .is_some_and(|schedule| schedule.trigger == reservation.trigger
                    && schedule.uses_agenda() == reservation.in_agenda)
            && affected.id(store.network()) == reservation.command_id,
        StateReadFault
    );
    let fees = fee_inputs(&affected);
    let count = fees.len() as u64;
    let (fee, cycles) = if let Some(cancel) = cancel {
        ensure!(fees.contains(cancel), StateReadFault);
        for fee in fees.iter().filter(|point| *point != cancel) {
            ensure!(
                stage.reservation(store, fee)?.as_ref() == Some(&reservation),
                StateReadFault
            );
        }
        (0, 0)
    } else {
        for fee in &fees {
            ensure!(
                !tx.inputs().any(|input| &input.previous_output == fee)
                    && stage.reservation(store, fee)?.is_none(),
                "Keeper reservation requires fresh fee Cells"
            );
        }
        let attempts = affected
            .keeper_schedule()
            .expect("bound command")
            .attempts();
        let reservation_fee = policy
            .reservation_fee
            .checked_mul(u64::from(attempts))
            .context("Keeper reservation fee overflow")?;
        ensure!(
            carrier.fee() >= reservation_fee,
            "insufficient Keeper reservation fee"
        );
        let slot = policy.slot(store.limits);
        // Cleanup must always fit, even when the application no longer does.
        wake_budget(slot, &affected)?;
        let cycles = verify_reservation(store, &affected, block, budget, slot.cycles, fresh)?;
        (reservation_fee, cycles)
    };
    charge(&mut budget.reads, reads + count, "Keeper queue reads")?;
    charge(
        &mut budget.read_bytes,
        old_bytes + count * RESERVATION_BYTES,
        "Keeper queue read bytes",
    )?;
    let reservation = submitted.is_some().then_some(reservation);
    let fee_write_bytes = if reservation.is_some() {
        RESERVATION_BYTES
    } else {
        FEE_KEY_BYTES
    };
    charge(
        &mut budget.write_bytes,
        new_bytes + count * fee_write_bytes,
        "Keeper queue write bytes",
    )?;
    Ok(Some(Change {
        queue,
        primary_fee: fee_point(&affected),
        fees,
        reservation,
        fee,
        cycles,
    }))
}

fn check_horizon(
    policy: KeeperPolicy,
    command: &Transaction,
    schedule: Schedule,
    block: &BlockContext<'_>,
) -> Result<()> {
    let height = block.candidate().height;
    let latest = height
        .checked_add(u64::from(policy.max_delay))
        .context("Keeper horizon overflow")?;
    let earliest = height.checked_add(1).context("Keeper height overflow")?;
    let expiry = command.valid_before_height().expect("bound command");
    let first = match schedule.trigger {
        Trigger::Height(target) => {
            ensure!(
                earliest <= target && target <= latest,
                "Keeper deadline outside the reservation horizon"
            );
            target
        }
        Trigger::Time(target) => {
            let time = policy
                .time
                .context("Keeper time triggers are not configured")?;
            let now = block.parent().context.timestamp_ms;
            ensure!(
                target > now
                    && target - now <= time.max_delay_ms
                    && expiry > earliest
                    && expiry <= latest,
                "Keeper deadline outside the reservation horizon"
            );
            earliest
        }
    };
    if let Some(retry) = schedule.retry {
        ensure!(
            policy.time.is_some(),
            "Keeper retry requires configured agenda slots"
        );
        let delay = u64::from(retry.delay_blocks) * u64::from(retry.max_attempts - 1);
        ensure!(
            expiry <= latest && first.checked_add(delay).is_some_and(|last| last < expiry),
            "Keeper retry window does not fit its delay and expiry"
        );
    }
    Ok(())
}

fn verify_reservation(
    store: &Store,
    command: &Transaction,
    block: &BlockContext<'_>,
    budget: &mut Budget,
    slot_cycles: u64,
    mut fresh: impl FnMut(&OutPoint) -> Result<stem_ledger::LiveCell>,
) -> Result<u64> {
    let members = keeper::members(command);
    if command.lineage_batch().is_some() {
        Transaction::verify_lineage_batch_plan(store.network(), members)?;
    }
    let mut available = slot_cycles;
    for member in members {
        let terms = member.lineage_command().expect("Keeper command terms");
        available = available
            .checked_sub(terms.executor_cycles)
            .and_then(|left| left.checked_sub(terms.validation_cycles))
            .context("signed Lineage budgets exceed a Keeper slot")?;
    }
    let mut total_cycles = 0u64;
    for member in members {
        let terms = member.lineage_command().expect("Keeper command terms");
        let escrow = lineage::resolve_fee(member, terms, block, &mut fresh)?.resolved;
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
        let limit = available.min(budget.cycles);
        let (cycles, _) = lineage::verify_authorization(store, member, escrow, limit)?;
        charge(&mut available, cycles, "Keeper slot authorization cycles")?;
        charge(&mut budget.cycles, cycles, "Keeper authorization cycles")?;
        total_cycles = total_cycles
            .checked_add(cycles)
            .context("Keeper authorization cycle overflow")?;
    }
    Ok(total_cycles)
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

    pub fn prove_keeper_reservation(&self, fee: &OutPoint) -> Result<(Option<Vec<u8>>, Proof)> {
        self.prove_system(&reservation_key(fee))
    }

    pub fn verify_keeper_reservation(
        network: &Byte32,
        header: &stem_ledger::block::Header,
        fee: &OutPoint,
        record: Option<&[u8]>,
        proof: &Proof,
    ) -> bool {
        record.is_none_or(|bytes| Reservation::decode(bytes).is_ok())
            && Self::verify_system_record(network, header, &reservation_key(fee), record, proof)
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
                agenda: None,
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
            .filter(|entry| height < entry.valid_before_height && entry.ready(height, time));
        let selected: Vec<_> = expired
            .into_iter()
            .chain(ready)
            .take(policy.time_slots())
            .collect();
        for entry in &selected {
            let command = stage.timed_command(self, &entry.fee_input)?;
            ensure!(
                fee_point(&command) == entry.fee_input
                    && command
                        .keeper_schedule()
                        .is_some_and(|schedule| schedule.uses_agenda()
                            && schedule.trigger == entry.trigger
                            && entry.attempts_left <= schedule.attempts())
                    && command.valid_before_height() == Some(entry.valid_before_height),
                StateReadFault
            );
            commands.push(command);
        }
        let agenda = (!selected.is_empty()).then_some(agenda);
        Ok(Due { commands, agenda })
    }

    pub(super) fn run_keeper(
        &self,
        due: &Due,
        block: &BlockContext<'_>,
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
        overlay.keeper.agenda = due.agenda.clone();
        for command in &due.commands {
            let point = fee_point(command);
            let schedule = command.keeper_schedule().context(StateReadFault)?;
            let fees = fee_inputs(command);
            let reservation = Reservation {
                trigger: schedule.trigger,
                primary_fee: point.clone(),
                command_id: command.id(self.network()),
                in_agenda: schedule.uses_agenda(),
            };
            for fee in &fees {
                ensure!(
                    overlay.keeper.reservation(self, fee)?.as_ref() == Some(&reservation),
                    StateReadFault
                );
            }
            let mut remaining = wake_budget(slot, command)?;
            let (mut receipt, may_retry) = match self.stage(
                command,
                block,
                origin,
                &mut remaining,
                overlay,
                None,
                |resolved, limit| self.cache.verify(resolved, limit, false),
            ) {
                Ok(receipt) => (receipt, false),
                Err(error) if error.is::<StateReadFault>() => return Err(error),
                Err(error) => {
                    // The prepaid reservation purchased the attempt. A stale
                    // reference or unaffordable candidate never changes a Cell.
                    remaining = Budget::empty();
                    (
                        Receipt {
                            fee: 0,
                            cycles: slot.cycles,
                            protocol_result: stem_ledger::lineage::Outcome::Skipped.encode(),
                        },
                        !error.is::<lineage::MissingFeeCell>(),
                    )
                }
            };
            let mut pending = false;
            if schedule.uses_agenda() {
                let agenda = overlay.keeper.agenda.as_mut().context(StateReadFault)?;
                let index = agenda
                    .binary_search_by(|entry| entry.order_key().cmp(&(schedule.trigger, &point)))
                    .ok()
                    .context(StateReadFault)?;
                let entry = &mut agenda[index];
                if let Some(next) = schedule
                    .retry
                    .filter(|_| may_retry && entry.attempts_left > 1)
                    .and_then(|retry| {
                        block
                            .candidate()
                            .height
                            .checked_add(u64::from(retry.delay_blocks))
                    })
                    .filter(|next| *next < entry.valid_before_height)
                {
                    entry.eligible_height = next;
                    entry.attempts_left -= 1;
                    receipt.protocol_result = stem_ledger::lineage::Outcome::Retrying.encode();
                    pending = true;
                } else {
                    agenda.remove(index);
                    overlay.keeper.timed_commands.insert(point, None);
                }
            } else if let Trigger::Height(height) = schedule.trigger {
                overlay.keeper.queues.insert(height, vec![]);
            }
            if !pending {
                overlay
                    .keeper
                    .reservations
                    .extend(fees.into_iter().map(|fee| (fee, None)));
            }
            budget.charge_slot(slot, remaining)?;
            receipts.push(receipt);
        }
        if let Some(agenda) = &overlay.keeper.agenda {
            charge(
                &mut budget.write_bytes,
                AGENDA.len() as u64 + keeper::agenda_bytes(agenda.len()) as u64,
                "Keeper agenda writes",
            )?;
        }
        overlay.keeper.running = false;
        Ok(receipts)
    }
}

fn fee_point(tx: &Transaction) -> OutPoint {
    keeper::primary_fee(tx).expect("validated Keeper fee input")
}

fn fee_inputs(tx: &Transaction) -> Vec<OutPoint> {
    keeper::members(tx)
        .iter()
        .map(|member| {
            member
                .input(0)
                .expect("validated Keeper fee input")
                .previous_output
        })
        .collect()
}

fn wake_budget(mut slot: Budget, tx: &Transaction) -> Result<Budget> {
    let count = keeper::members(tx).len() as u64;
    let (read_bytes, write_bytes) = if tx.keeper_schedule().expect("bound command").uses_agenda() {
        (COMMAND_KEY_BYTES, COMMAND_KEY_BYTES)
    } else {
        (QUEUE_KEY_BYTES + 8, QUEUE_KEY_BYTES)
    };
    charge(&mut slot.reads, 1 + count, "Keeper slot reads")?;
    charge(
        &mut slot.read_bytes,
        read_bytes + tx.as_slice().len() as u64 + count * RESERVATION_BYTES,
        "Keeper slot read bytes",
    )?;
    charge(
        &mut slot.write_bytes,
        write_bytes + count * FEE_KEY_BYTES,
        "Keeper slot write bytes",
    )?;
    Ok(slot)
}

fn queue_size(queue: &[Transaction]) -> u64 {
    if queue.is_empty() {
        0
    } else {
        keeper::encode_queue(queue).len() as u64
    }
}

fn queue_key(height: u64) -> Vec<u8> {
    [QUEUE, height.to_be_bytes().as_slice()].concat()
}

fn reservation_key(point: &OutPoint) -> Vec<u8> {
    [FEE, point.as_slice()].concat()
}

fn timed_key(point: &OutPoint) -> Vec<u8> {
    [COMMAND, point.as_slice()].concat()
}
