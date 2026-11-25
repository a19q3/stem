//! Canonical actions and pending commands for the native Keeper.

use anyhow::{Result, bail, ensure};
use ckb_types::{bytes::Bytes, packed::OutPoint, prelude::*};

use crate::{
    Transaction,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

pub const MAX_COMMAND_BYTES: usize = 8 * 1024;
pub const MAX_PER_HEIGHT: usize = 16;

#[derive(Clone, Debug)]
pub enum Action {
    Schedule(Bytes),
    Cancel { height: u64, fee_input: OutPoint },
}

impl Action {
    pub fn schedule(command: &Transaction) -> Result<Self> {
        ensure!(
            command.execute_at().is_some()
                && command.witness(0).is_some()
                && command.as_slice().len() <= MAX_COMMAND_BYTES,
            "Keeper requires a sealed, height-bound Lineage command"
        );
        Ok(Self::Schedule(Bytes::copy_from_slice(command.as_slice())))
    }

    pub(crate) fn encode(&self) -> wire::KeeperAction {
        let (kind, payload) = match self {
            Self::Schedule(command) => (0u8, command.clone()),
            Self::Cancel { height, fee_input } => (
                1u8,
                [&height.to_le_bytes(), fee_input.as_slice()]
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
            1 if payload.len() == 8 + OutPoint::TOTAL_SIZE => {
                let height = u64::from_le_bytes(payload[..8].try_into()?);
                let fee_input = OutPoint::from_slice(&payload[8..])?;
                ensure!(
                    height != 0 && !fee_input.is_null(),
                    "invalid Keeper cancellation"
                );
                Ok(Self::Cancel { height, fee_input })
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
                command.execute_at() == Some(height),
                "incorrect Keeper queue height"
            );
            let point = command
                .input(0)
                .expect("validated Keeper fee input")
                .previous_output;
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
    ensure!(
        command.execute_at().is_some() && command.witness(0).is_some(),
        "Keeper requires a sealed, height-bound Lineage command"
    );
    Ok(command)
}
