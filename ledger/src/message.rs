use anyhow::{Result, ensure};
use ckb_types::{bytes::Bytes, prelude::*};

use crate::{
    MAX_SCRIPT_GROUPS, MAX_TRANSACTION_BYTES,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

const MAX_GROUP_ACTIONS: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupAction {
    pub kind: u16,
    pub data: Bytes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GroupMessage {
    pub group_index: u16,
    pub actions: Vec<GroupAction>,
}

impl GroupMessage {
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let message = Self::decode(wire::GroupMessage::from_slice(bytes)?);
        Self::validate_all(std::slice::from_ref(&message))?;
        Ok(message)
    }

    pub(crate) fn encode(&self) -> wire::GroupMessage {
        let actions = self
            .actions
            .iter()
            .map(|action| {
                wire::GroupAction::new_builder()
                    .kind(number(action.kind))
                    .data(wire::Bytes::new_unchecked(action.data.pack().as_bytes()))
                    .build()
            })
            .collect();
        wire::GroupMessage::new_builder()
            .group_index(number(self.group_index))
            .actions(wire::GroupActionVec::new_builder().set(actions).build())
            .build()
    }

    pub(crate) fn decode(message: wire::GroupMessage) -> Self {
        Self {
            group_index: read(message.group_index()),
            actions: message
                .actions()
                .into_iter()
                .map(|action| GroupAction {
                    kind: read(action.kind()),
                    data: action.data().raw_data(),
                })
                .collect(),
        }
    }

    pub(crate) fn validate_all(messages: &[Self]) -> Result<()> {
        ensure!(
            messages.len() <= MAX_SCRIPT_GROUPS,
            "too many Group Messages"
        );
        let mut previous = None;
        let mut data_bytes = 0;
        for message in messages {
            ensure!(
                usize::from(message.group_index) < MAX_SCRIPT_GROUPS,
                "Group Message index exceeds limit"
            );
            ensure!(
                previous.is_none_or(|last| last < message.group_index),
                "Group Message entries must be strictly increasing"
            );
            ensure!(
                (1..=MAX_GROUP_ACTIONS).contains(&message.actions.len()),
                "Group Message Action count exceeds limit"
            );
            previous = Some(message.group_index);
            for action in &message.actions {
                data_bytes += action.data.len().min(MAX_TRANSACTION_BYTES + 1);
                ensure!(
                    data_bytes <= MAX_TRANSACTION_BYTES,
                    "Group Message data exceeds byte limit"
                );
            }
        }
        Ok(())
    }

    pub fn encode_bytes(&self) -> Bytes {
        self.encode().as_bytes()
    }
}

fn number(value: u16) -> wire::Uint16 {
    wire::Uint16::new_unchecked(Bytes::copy_from_slice(&value.to_le_bytes()))
}

fn read(value: wire::Uint16) -> u16 {
    u16::from_le_bytes(value.as_slice().try_into().expect("Uint16"))
}
