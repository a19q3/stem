//! Canonical actions and pending commands for the native Keeper.

use anyhow::{Result, bail, ensure};
use ckb_types::{
    bytes::Bytes,
    packed::{Byte32, OutPoint},
    prelude::*,
};

use crate::{
    Transaction,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

pub const MAX_COMMAND_BYTES: usize = 8 * 1024;
pub const MAX_PER_HEIGHT: usize = 16;
pub const MAX_TIMED_PENDING: usize = 32;
const TIMED_ENTRY_BYTES: usize = 2 * size_of::<u64>() + OutPoint::TOTAL_SIZE;

pub const fn agenda_bytes(entries: usize) -> usize {
    if entries == 0 {
        0
    } else {
        size_of::<u32>() + entries * TIMED_ENTRY_BYTES
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reservation {
    pub trigger: Trigger,
    pub primary_fee: OutPoint,
    pub command_id: Byte32,
}

impl Reservation {
    pub const SIZE: usize = Trigger::SIZE + OutPoint::TOTAL_SIZE + 32;

    pub fn encode(&self) -> Vec<u8> {
        [
            self.trigger.encode().as_slice(),
            self.primary_fee.as_slice(),
            self.command_id.as_slice(),
        ]
        .concat()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() == Self::SIZE,
            "incorrect Keeper reservation size"
        );
        let trigger = Trigger::decode(&bytes[..Trigger::SIZE])?;
        let end = Trigger::SIZE + OutPoint::TOTAL_SIZE;
        let primary_fee = OutPoint::from_slice(&bytes[Trigger::SIZE..end])?;
        let command_id = Byte32::from_slice(&bytes[end..])?;
        ensure!(
            !primary_fee.is_null() && command_id != Byte32::default(),
            "Keeper reservation must identify a fee Cell and command"
        );
        Ok(Self {
            trigger,
            primary_fee,
            command_id,
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
pub struct TimedEntry {
    pub timestamp_ms: u64,
    pub valid_before_height: u64,
    pub fee_input: OutPoint,
}

pub fn encode_agenda(entries: &[TimedEntry]) -> Bytes {
    wire::KeeperTimedAgenda::new_builder()
        .set(
            entries
                .iter()
                .map(|entry| {
                    wire::KeeperTimedEntry::new_builder()
                        .timestamp_ms(wire::Uint64::new_unchecked(
                            entry.timestamp_ms.to_le_bytes().to_vec().into(),
                        ))
                        .valid_before_height(wire::Uint64::new_unchecked(
                            entry.valid_before_height.to_le_bytes().to_vec().into(),
                        ))
                        .fee_input(wire::OutPoint::new_unchecked(entry.fee_input.as_bytes()))
                        .build()
                })
                .collect(),
        )
        .build()
        .as_bytes()
}

pub fn decode_agenda(bytes: &[u8]) -> Result<Vec<TimedEntry>> {
    let agenda = wire::KeeperTimedAgenda::from_slice(bytes)?;
    ensure!(
        (1..=MAX_TIMED_PENDING).contains(&agenda.len()),
        "invalid Keeper time agenda size"
    );
    let mut entries: Vec<TimedEntry> = Vec::with_capacity(agenda.len());
    for item in agenda {
        let entry = TimedEntry {
            timestamp_ms: u64::from_le_bytes(item.timestamp_ms().as_slice().try_into()?),
            valid_before_height: u64::from_le_bytes(
                item.valid_before_height().as_slice().try_into()?,
            ),
            fee_input: OutPoint::from_slice(item.fee_input().as_slice())?,
        };
        ensure!(
            entry.timestamp_ms != 0
                && entry.valid_before_height > 1
                && !entry.fee_input.is_null()
                && entries
                    .last()
                    .is_none_or(|last| last.order_key() < entry.order_key())
                && entries
                    .iter()
                    .all(|prior| prior.fee_input != entry.fee_input),
            "invalid Keeper time agenda entry"
        );
        entries.push(entry);
    }
    Ok(entries)
}

impl TimedEntry {
    pub fn order_key(&self) -> (u64, &OutPoint) {
        (self.timestamp_ms, &self.fee_input)
    }
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
                let decoded = Reservation::decode(&payload)?;
                Ok(Self::Cancel {
                    trigger: decoded.trigger,
                    fee_input: decoded.primary_fee,
                    command_id: decoded.command_id,
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
                command.keeper_trigger() == Some(Trigger::Height(height)),
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
