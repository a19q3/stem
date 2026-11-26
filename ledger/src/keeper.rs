//! Canonical actions and pending commands for the native Keeper.

use anyhow::{Result, bail, ensure};
use ckb_types::{
    bytes::Bytes,
    packed::{Byte32, OutPoint},
    prelude::*,
};

use crate::{
    Transaction,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity, Reader as WireReader},
    wire,
};

pub const MAX_COMMAND_BYTES: usize = 8 * 1024;
pub const MAX_PER_HEIGHT: usize = 16;
pub const MAX_AGENDA_PENDING: usize = 32;
pub const MAX_ATTEMPTS: u8 = 4;
const AGENDA_ENTRY_BYTES: usize = Trigger::SIZE + 2 * size_of::<u64>() + OutPoint::TOTAL_SIZE + 1;

pub const fn agenda_bytes(entries: usize) -> usize {
    if entries == 0 {
        0
    } else {
        size_of::<u32>() + entries * AGENDA_ENTRY_BYTES
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Trigger {
    Height(u64),
    Time(u64),
}

impl Trigger {
    pub const SIZE: usize = 9;

    pub fn encode(self) -> [u8; Self::SIZE] {
        let (kind, value) = match self {
            Self::Height(height) => (0, height),
            Self::Time(timestamp_ms) => (1, timestamp_ms),
        };
        let mut bytes = [kind; Self::SIZE];
        bytes[1..].copy_from_slice(&value.to_le_bytes());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() == Self::SIZE, "invalid Keeper trigger size");
        let value = u64::from_le_bytes(bytes[1..].try_into()?);
        match (bytes[0], value) {
            (0, 1..) => Ok(Self::Height(value)),
            (1, 1..) => Ok(Self::Time(value)),
            _ => bail!("invalid Keeper trigger"),
        }
    }

    pub fn ready(self, height: u64, parent_time_ms: u64) -> bool {
        match self {
            Self::Height(target) => target == height,
            Self::Time(target) => parent_time_ms >= target,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Retry {
    pub max_attempts: u8,
    pub delay_blocks: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Schedule {
    pub trigger: Trigger,
    pub retry: Option<Retry>,
}

impl Schedule {
    pub fn uses_agenda(self) -> bool {
        self.retry.is_some() || matches!(self.trigger, Trigger::Time(_))
    }

    pub fn attempts(self) -> u8 {
        self.retry.map_or(1, |retry| retry.max_attempts)
    }

    pub fn ready(self, height: u64, parent_time_ms: u64) -> bool {
        if let (Trigger::Height(first), Some(_)) = (self.trigger, self.retry) {
            return height >= first;
        }
        self.trigger.ready(height, parent_time_ms)
    }

    pub(crate) fn encode(self) -> wire::KeeperSchedule {
        wire::KeeperSchedule::new_builder()
            .trigger(wire::KeeperTrigger::new_unchecked(
                self.trigger.encode().to_vec().into(),
            ))
            .retry(
                wire::KeeperRetryOpt::new_builder()
                    .set(self.retry.map(|retry| {
                        wire::KeeperRetry::new_builder()
                            .max_attempts(retry.max_attempts.into())
                            .delay_blocks(wire::Uint16::new_unchecked(
                                retry.delay_blocks.to_le_bytes().to_vec().into(),
                            ))
                            .build()
                    }))
                    .build(),
            )
            .build()
    }

    pub(crate) fn decode(value: wire::KeeperSchedule) -> Result<Self> {
        let retry = value
            .retry()
            .to_opt()
            .map(|retry| {
                let max_attempts = u8::from(retry.max_attempts());
                let delay_blocks = u16::from_le_bytes(retry.delay_blocks().as_slice().try_into()?);
                ensure!(
                    (2..=MAX_ATTEMPTS).contains(&max_attempts) && delay_blocks != 0,
                    "invalid Keeper retry policy"
                );
                Ok(Retry {
                    max_attempts,
                    delay_blocks,
                })
            })
            .transpose()?;
        Ok(Self {
            trigger: Trigger::decode(value.trigger().as_slice())?,
            retry,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reservation {
    pub trigger: Trigger,
    pub primary_fee: OutPoint,
    pub command_id: Byte32,
    pub in_agenda: bool,
}

impl Reservation {
    pub const SIZE: usize = wire::KeeperReservation::TOTAL_SIZE;

    pub fn encode(&self) -> Vec<u8> {
        [
            self.trigger.encode().as_slice(),
            self.primary_fee.as_slice(),
            self.command_id.as_slice(),
            &[u8::from(self.in_agenda)],
        ]
        .concat()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let record = wire::KeeperReservationReader::from_slice(bytes)?;
        let (trigger, primary_fee, command_id) = decode_command_key(record.command().as_slice())?;
        let in_agenda = match record.in_agenda().as_slice()[0] {
            0 if matches!(trigger, Trigger::Height(_)) => false,
            1 => true,
            _ => bail!("invalid Keeper reservation index"),
        };
        Ok(Self {
            trigger,
            primary_fee,
            command_id,
            in_agenda,
        })
    }
}

/// Use the same view for an individual command and a batch of commands.
pub fn members(command: &Transaction) -> &[Transaction] {
    command
        .lineage_batch()
        .unwrap_or_else(|| std::slice::from_ref(command))
}

pub fn primary_fee(command: &Transaction) -> Option<OutPoint> {
    members(command)
        .first()?
        .input(0)
        .map(|input| input.previous_output)
}

fn validate_command(command: &Transaction) -> Result<()> {
    ensure!(
        command.keeper_trigger().is_some()
            && (command.lineage_batch().is_some()
                || (command.has_lineage_command()
                    && command.witness(0).is_some()
                    && command
                        .lineage_intent()
                        .is_none_or(|intent| intent.batch.is_none()))),
        "Keeper requires a sealed standalone command or a complete Lineage batch with a trigger"
    );
    ensure!(
        command.as_slice().len() <= MAX_COMMAND_BYTES,
        "Keeper command exceeds size limit"
    );
    Ok(())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgendaEntry {
    pub trigger: Trigger,
    pub eligible_height: u64,
    pub valid_before_height: u64,
    pub fee_input: OutPoint,
    pub attempts_left: u8,
}

impl AgendaEntry {
    pub fn order_key(&self) -> (Trigger, &OutPoint) {
        (self.trigger, &self.fee_input)
    }

    pub fn ready(&self, height: u64, parent_time_ms: u64) -> bool {
        height >= self.eligible_height
            && match self.trigger {
                Trigger::Height(at) => height >= at,
                Trigger::Time(at) => parent_time_ms >= at,
            }
    }
}

fn uint64(value: u64) -> wire::Uint64 {
    wire::Uint64::new_unchecked(value.to_le_bytes().to_vec().into())
}

pub fn encode_agenda(entries: &[AgendaEntry]) -> Bytes {
    wire::KeeperAgenda::new_builder()
        .set(
            entries
                .iter()
                .map(|entry| {
                    wire::KeeperAgendaEntry::new_builder()
                        .trigger(wire::KeeperTrigger::new_unchecked(
                            entry.trigger.encode().to_vec().into(),
                        ))
                        .eligible_height(uint64(entry.eligible_height))
                        .valid_before_height(uint64(entry.valid_before_height))
                        .fee_input(wire::OutPoint::new_unchecked(entry.fee_input.as_bytes()))
                        .attempts_left(entry.attempts_left.into())
                        .build()
                })
                .collect(),
        )
        .build()
        .as_bytes()
}

pub fn decode_agenda(bytes: &[u8]) -> Result<Vec<AgendaEntry>> {
    let agenda = wire::KeeperAgenda::from_slice(bytes)?;
    ensure!(
        (1..=MAX_AGENDA_PENDING).contains(&agenda.len()),
        "invalid Keeper agenda size"
    );
    let mut entries: Vec<AgendaEntry> = Vec::with_capacity(agenda.len());
    for item in agenda {
        let entry = AgendaEntry {
            trigger: Trigger::decode(item.trigger().as_slice())?,
            eligible_height: u64::from_le_bytes(item.eligible_height().as_slice().try_into()?),
            valid_before_height: u64::from_le_bytes(
                item.valid_before_height().as_slice().try_into()?,
            ),
            fee_input: OutPoint::from_slice(item.fee_input().as_slice())?,
            attempts_left: u8::from(item.attempts_left()),
        };
        ensure!(
            (1..=MAX_ATTEMPTS).contains(&entry.attempts_left)
                && entry.valid_before_height > 1
                && entry.eligible_height < entry.valid_before_height
                && !entry.fee_input.is_null()
                && entries
                    .last()
                    .is_none_or(|last| last.order_key() < entry.order_key())
                && entries
                    .iter()
                    .all(|prior| prior.fee_input != entry.fee_input),
            "invalid Keeper agenda entry"
        );
        entries.push(entry);
    }
    Ok(entries)
}

fn decode_command_key(bytes: &[u8]) -> Result<(Trigger, OutPoint, Byte32)> {
    let record = wire::KeeperCommandKeyReader::from_slice(bytes)?;
    let trigger = Trigger::decode(record.trigger().as_slice())?;
    let fee = OutPoint::from_slice(record.fee().as_slice())?;
    let command_id = Byte32::from_slice(record.command().as_slice())?;
    ensure!(
        !fee.is_null() && command_id != Byte32::default(),
        "Keeper command reference must identify a fee Cell and command"
    );
    Ok((trigger, fee, command_id))
}

#[derive(Clone, Debug)]
pub enum Action {
    Schedule(Bytes),
    Cancel {
        trigger: Trigger,
        fee_input: OutPoint,
        command_id: Byte32,
    },
}

impl Action {
    pub fn schedule(command: &Transaction) -> Result<Self> {
        validate_command(command)?;
        Ok(Self::Schedule(Bytes::copy_from_slice(command.as_slice())))
    }

    pub(crate) fn encode(&self) -> wire::KeeperAction {
        let (kind, payload) = match self {
            Self::Schedule(command) => (0u8, command.clone()),
            Self::Cancel {
                trigger,
                fee_input,
                command_id,
            } => (
                1u8,
                [
                    trigger.encode().as_slice(),
                    fee_input.as_slice(),
                    command_id.as_slice(),
                ]
                .concat()
                .into(),
            ),
        };
        wire::KeeperAction::new_builder()
            .kind(wire::KeeperActionKind::new_unchecked(vec![kind].into()))
            .payload(wire::Bytes::new_unchecked(payload.pack().as_bytes()))
            .build()
    }

    pub(crate) fn decode(action: wire::KeeperAction) -> Result<Self> {
        let payload = action.payload().raw_data();
        match action.kind().as_slice()[0] {
            0 if (1..=MAX_COMMAND_BYTES).contains(&payload.len()) => Ok(Self::Schedule(payload)),
            1 => {
                let (trigger, fee_input, command_id) = decode_command_key(&payload)?;
                Ok(Self::Cancel {
                    trigger,
                    fee_input,
                    command_id,
                })
            }
            _ => bail!("invalid Keeper action"),
        }
    }
}

pub fn encode_queue(commands: &[Transaction]) -> Bytes {
    wire::KeeperQueue::new_builder()
        .set(
            commands
                .iter()
                .map(|command| {
                    let bytes = Bytes::copy_from_slice(command.as_slice());
                    wire::Bytes::new_unchecked(bytes.pack().as_bytes())
                })
                .collect(),
        )
        .build()
        .as_bytes()
}

pub fn decode_queue(bytes: &[u8], height: u64) -> Result<Vec<Transaction>> {
    let queue = wire::KeeperQueue::from_slice(bytes)?;
    ensure!(
        (1..=MAX_PER_HEIGHT).contains(&queue.len()),
        "invalid Keeper queue length"
    );
    let mut previous = None;
    queue
        .into_iter()
        .map(|item| {
            let command = decode_command(&item.raw_data())?;
            ensure!(
                command.keeper_schedule()
                    == Some(Schedule {
                        trigger: Trigger::Height(height),
                        retry: None
                    }),
                "incorrect Keeper queue height"
            );
            let point = primary_fee(&command).expect("validated Keeper fee input");
            ensure!(
                previous.as_ref().is_none_or(|prior| prior < &point),
                "unordered Keeper queue"
            );
            previous = Some(point);
            Ok(command)
        })
        .collect()
}

pub fn decode_command(bytes: &[u8]) -> Result<Transaction> {
    ensure!(
        bytes.len() <= MAX_COMMAND_BYTES,
        "Keeper command exceeds size limit"
    );
    let command = Transaction::from_slice(bytes)?;
    validate_command(&command)?;
    Ok(command)
}
