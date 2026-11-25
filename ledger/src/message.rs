use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_hash::new_blake2b;
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::*};

use crate::{
    MAX_SCRIPT_GROUPS, MAX_TRANSACTION_BYTES, ScriptGroup, ScriptGroupKind,
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

/// A Group Message identified without any transaction-local position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntentMessage {
    pub group_kind: ScriptGroupKind,
    pub script_hash: Byte32,
    pub actions: Vec<GroupAction>,
}

impl IntentMessage {
    fn key(&self) -> (ScriptGroupKind, &Byte32) {
        (self.group_kind, &self.script_hash)
    }
}

/// Hashes all application Actions in canonical (role, Script hash) order.
/// The Action sequence within a group keeps its original ordering.
pub fn intent_message_commitment(messages: &[IntentMessage]) -> Result<Byte32> {
    use stem_bridge_wire::intercell::{RECEIVE_ACTION, SEND_ACTION};

    ensure!(
        messages.len() <= MAX_SCRIPT_GROUPS,
        "too many intent messages"
    );
    ensure!(
        messages
            .windows(2)
            .all(|pair| pair[0].key() < pair[1].key()),
        "intent messages must be ordered by unique role and Script hash"
    );
    let mut digest = new_blake2b();
    digest.update(b"Stem intent Group Messages\0");
    digest.update(&(messages.len() as u16).to_le_bytes());
    let mut remaining = MAX_TRANSACTION_BYTES;
    for message in messages {
        ensure!(
            (1..=MAX_GROUP_ACTIONS).contains(&message.actions.len()),
            "intent Group Message Action count exceeds limit"
        );
        digest.update(&[message.group_kind as u8]);
        digest.update(message.script_hash.as_slice());
        digest.update(&(message.actions.len() as u16).to_le_bytes());
        for action in &message.actions {
            ensure!(
                !matches!(action.kind, SEND_ACTION | RECEIVE_ACTION),
                "Intercell Actions require complete Lock authorization"
            );
            remaining = remaining
                .checked_sub(action.data.len())
                .context("intent Action bytes exceed limit")?;
            digest.update(&action.kind.to_le_bytes());
            digest.update(&(action.data.len() as u32).to_le_bytes());
            digest.update(&action.data);
        }
    }
    let mut bytes = [0; 32];
    digest.finalize(&mut bytes);
    Ok(bytes.pack())
}

/// Resolve a signed manifest into the runtime Group Message indices.
pub fn bind_intent_messages(
    groups: &[ScriptGroup],
    manifest: &[IntentMessage],
) -> Result<Vec<GroupMessage>> {
    intent_message_commitment(manifest)?;
    let mut indices = BTreeMap::new();
    for (index, group) in groups.iter().enumerate() {
        let key = (group.kind, group.script.calc_script_hash());
        ensure!(
            indices.insert(key, u16::try_from(index)?).is_none(),
            "ambiguous Script group identity"
        );
    }
    let mut messages = manifest
        .iter()
        .map(|item| {
            let key = (item.group_kind, item.script_hash.clone());
            let group_index = *indices
                .get(&key)
                .context("intent message refers to an absent Script Group")?;
            Ok(GroupMessage {
                group_index,
                actions: item.actions.clone(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    messages.sort_unstable_by_key(|item| item.group_index);
    Ok(messages)
}

/// Recover the stable identities of the messages the VM will actually see.
pub fn resolve_intent_messages(
    groups: &[ScriptGroup],
    messages: impl IntoIterator<Item = GroupMessage>,
) -> Result<Byte32> {
    let mut manifest = messages
        .into_iter()
        .map(|message| {
            let group = groups
                .get(usize::from(message.group_index))
                .context("intent message refers to an absent Script Group")?;
            Ok(IntentMessage {
                group_kind: group.kind,
                script_hash: group.script.calc_script_hash(),
                actions: message.actions,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    manifest.sort_unstable_by(|left, right| left.key().cmp(&right.key()));
    intent_message_commitment(&manifest)
}

fn number(value: u16) -> wire::Uint16 {
    wire::Uint16::new_unchecked(Bytes::copy_from_slice(&value.to_le_bytes()))
}

fn read(value: wire::Uint16) -> u16 {
    u16::from_le_bytes(value.as_slice().try_into().expect("Uint16"))
}
