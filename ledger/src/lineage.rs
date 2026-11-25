//! The stable identity of a sequence of ordinary Live Cells.

use anyhow::{Result, ensure};
use ckb_types::{
    bytes::Bytes,
    core::ScriptHashType,
    packed::{Byte32, OutPoint, Script},
    prelude::*,
};

use crate::{
    hash,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

pub const REGISTER: u16 = 0;
pub const REPLACE_EXECUTOR: u16 = 1;
pub const RETIRE: u16 = 2;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Head {
    pub out_point: OutPoint,
    pub version: u64,
    pub executor: Byte32,
}

impl Head {
    pub const MAX_ENCODED_BYTES: usize = 36 + 10 + 32;

    pub fn encode(&self) -> Bytes {
        wire::LineageHead::new_builder()
            .out_point(wire::OutPoint::new_unchecked(self.out_point.as_bytes()))
            .version(
                wire::CompactUint64::new_builder()
                    .set(self.version.into())
                    .build(),
            )
            .executor(wire::Byte32::new_unchecked(self.executor.as_bytes()))
            .build()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let record = wire::LineageHead::from_slice(bytes)?;
        let head = Self {
            out_point: OutPoint::new_unchecked(record.out_point().as_bytes()),
            version: record.version().as_reader().value().try_into()?,
            executor: Byte32::new_unchecked(record.executor().as_bytes()),
        };
        ensure!(
            !head.out_point.is_null() && head.executor != Byte32::default(),
            "invalid Lineage Head"
        );
        Ok(head)
    }
}

/// The registration transaction must consume this anchor as its first input.
/// Distinct outputs of that transaction occupy distinct registration slots.
pub fn id(network: &Byte32, anchor: &OutPoint, output_index: u32) -> Byte32 {
    hash(
        b"Stem Lineage ID\0",
        network,
        &[anchor.as_slice(), &output_index.to_le_bytes()],
    )
}

pub fn marker(code_hash: &Byte32, id: &Byte32) -> Script {
    Script::new_builder()
        .code_hash(code_hash.clone())
        .hash_type(ScriptHashType::Data)
        .args(id.as_bytes())
        .build()
}

/// Standard Lineage Lock args are the role byte and the owner's Blake160.
pub fn lock(code_hash: &Byte32, owner: &[u8; 20], is_head: bool) -> Script {
    let mut args = [0; 21];
    args[0] = u8::from(is_head);
    args[1..].copy_from_slice(owner);
    Script::new_builder()
        .code_hash(code_hash.clone())
        .hash_type(ScriptHashType::Data)
        .args(Bytes::copy_from_slice(&args))
        .build()
}

pub const MAX_TARGETS: usize = 16;
pub const MAX_PAYLOAD_BYTES: usize = 4 * 1024;
pub const MAX_DATA_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Target {
    pub id: Byte32,
    pub min_version: u64,
    pub max_version: u64,
    pub min_capacity: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Command {
    pub targets: Vec<Target>,
    pub executor: Byte32,
    pub executor_dep: OutPoint,
    pub executor_cycles: u64,
    pub validation_cycles: u64,
    pub fee: u64,
    pub payload: Bytes,
}

impl Command {
    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            (1..=MAX_TARGETS).contains(&self.targets.len())
                && self.targets.windows(2).all(|pair| pair[0].id < pair[1].id)
                && self.targets.iter().all(|target| {
                    target.id != Byte32::default() && target.min_version <= target.max_version
                }),
            "invalid or unordered Lineage targets"
        );
        ensure!(
            self.executor != Byte32::default()
                && !self.executor_dep.is_null()
                && self.executor_cycles > 0
                && self.validation_cycles > 0
                && self.fee > 0
                && self.payload.len() <= MAX_PAYLOAD_BYTES,
            "invalid Lineage Executor or fee bounds"
        );
        Ok(())
    }

    pub(crate) fn encode(&self) -> wire::LineageCommand {
        fn compact(value: u64) -> wire::CompactUint64 {
            wire::CompactUint64::new_builder().set(value.into()).build()
        }

        let targets = self.targets.iter().map(|target| {
            wire::LineageTarget::new_builder()
                .id(wire::Byte32::new_unchecked(target.id.as_bytes()))
                .min_version(compact(target.min_version))
                .max_version(compact(target.max_version))
                .min_capacity(compact(target.min_capacity))
                .build()
        });
        wire::LineageCommand::new_builder()
            .targets(
                wire::LineageTargetVec::new_builder()
                    .set(targets.collect())
                    .build(),
            )
            .executor(wire::Byte32::new_unchecked(self.executor.as_bytes()))
            .executor_dep(wire::OutPoint::new_unchecked(self.executor_dep.as_bytes()))
            .executor_cycles(compact(self.executor_cycles))
            .validation_cycles(compact(self.validation_cycles))
            .fee(compact(self.fee))
            .payload(wire::Bytes::new_unchecked(self.payload.pack().as_bytes()))
            .build()
    }

    pub(crate) fn decode(command: wire::LineageCommand) -> Result<Self> {
        fn compact(value: wire::CompactUint64) -> Result<u64> {
            Ok(value.as_reader().value().try_into()?)
        }

        let targets = command.targets();
        ensure!(targets.len() <= MAX_TARGETS, "too many Lineage targets");
        let targets = targets
            .into_iter()
            .map(|target| {
                Ok(Target {
                    id: Byte32::new_unchecked(target.id().as_bytes()),
                    min_version: compact(target.min_version())?,
                    max_version: compact(target.max_version())?,
                    min_capacity: compact(target.min_capacity())?,
                })
            })
            .collect::<Result<_>>()?;
        let result = Self {
            targets,
            executor: Byte32::new_unchecked(command.executor().as_bytes()),
            executor_dep: OutPoint::new_unchecked(command.executor_dep().as_bytes()),
            executor_cycles: compact(command.executor_cycles())?,
            validation_cycles: compact(command.validation_cycles())?,
            fee: compact(command.fee())?,
            payload: command.payload().raw_data(),
        };
        result.validate()?;
        Ok(result)
    }
}

/// Interpreted only against a transaction proof for a Lineage command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Rejected,
    Applied,
}

impl Outcome {
    pub fn from_applied(applied: bool) -> Self {
        if applied {
            Self::Applied
        } else {
            Self::Rejected
        }
    }

    pub fn encode(self) -> Bytes {
        match self {
            Self::Rejected => Bytes::from_static(&[0]),
            Self::Applied => Bytes::from_static(&[1]),
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        match bytes {
            [0] => Ok(Self::Rejected),
            [1] => Ok(Self::Applied),
            _ => anyhow::bail!("invalid Lineage outcome"),
        }
    }
}
