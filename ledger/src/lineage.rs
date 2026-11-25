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
