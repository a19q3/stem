//! Height and millisecond locks use the committed parent time.

use crate::strand::prelude::{Builder as WireBuilder, Entity as WireEntity};
use anyhow::{Context, Result, bail, ensure};
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::*,
};

use crate::{
    CapacityPolicy,
    block::{Header, HeaderContext},
    wire,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Since {
    AbsoluteHeight(u64),
    RelativeBlocks(u64),
    AbsoluteTime(u64),
    RelativeTime(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CellInput {
    pub previous_output: OutPoint,
    pub since: Option<Since>,
}

impl CellInput {
    pub fn new(previous_output: OutPoint, since: Option<Since>) -> Self {
        Self {
            previous_output,
            since,
        }
    }

    pub(crate) fn encode(&self) -> wire::CellInput {
        let since = self.since_bytes();
        wire::CellInput::new_builder()
            .since(wire::Since::new_unchecked(since.to_vec().into()))
            .previous_output(wire::OutPoint::new_unchecked(
                self.previous_output.as_bytes(),
            ))
            .build()
    }

    pub(crate) fn decode(input: wire::CellInput) -> Result<Self> {
        let since = input.since();
        let value = u64::from_le_bytes(since.value().as_slice().try_into()?);
        let since = match (since.kind().as_slice()[0], value) {
            (0, 0) => None,
            (1, height) => Some(Since::AbsoluteHeight(height)),
            (2, delay) if delay != 0 => Some(Since::RelativeBlocks(delay)),
            (3, time) => Some(Since::AbsoluteTime(time)),
            (4, delay) if delay != 0 => Some(Since::RelativeTime(delay)),
            _ => bail!("noncanonical Stem Since"),
        };
        Ok(Self {
            previous_output: OutPoint::new_unchecked(input.previous_output().as_bytes()),
            since,
        })
    }

    pub fn encode_bytes(&self) -> ckb_types::bytes::Bytes {
        self.encode().as_bytes()
    }

    pub fn since_bytes(&self) -> [u8; 9] {
        let (tag, value) = match self.since {
            None => (0, 0),
            Some(Since::AbsoluteHeight(value)) => (1, value),
            Some(Since::RelativeBlocks(value)) => (2, value),
            Some(Since::AbsoluteTime(value)) => (3, value),
            Some(Since::RelativeTime(value)) => (4, value),
        };
        let mut bytes = [0; 9];
        bytes[0] = tag;
        bytes[1..].copy_from_slice(&value.to_le_bytes());
        bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Origin {
    pub height: u64,
    pub timestamp_ms: u64,
    pub context_id: Byte32,
}

impl Origin {
    pub const LEN: usize = 48;

    pub fn new(network: &Byte32, context: &HeaderContext) -> Self {
        Self {
            height: context.height,
            timestamp_ms: context.timestamp_ms,
            context_id: context.id(network),
        }
    }

    pub fn encode(&self) -> [u8; Self::LEN] {
        let mut bytes = [0; Self::LEN];
        bytes[..8].copy_from_slice(&self.height.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.timestamp_ms.to_le_bytes());
        bytes[16..].copy_from_slice(self.context_id.as_slice());
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() == Self::LEN, "invalid Cell origin length");
        Ok(Self {
            height: u64::from_le_bytes(bytes[..8].try_into()?),
            timestamp_ms: u64::from_le_bytes(bytes[8..16].try_into()?),
            context_id: Byte32::from_slice(&bytes[16..])?,
        })
    }
}

/// A child derived from one immutable parent and network for transaction execution.
pub struct BlockContext<'a> {
    network: &'a Byte32,
    candidate: HeaderContext,
    parent: &'a Header,
    capacity: CapacityPolicy,
}

impl<'a> BlockContext<'a> {
    pub fn after(
        network: &'a Byte32,
        parent: &'a Header,
        timestamp_ms: u64,
        transaction_root: Byte32,
        capacity: CapacityPolicy,
    ) -> Result<Self> {
        ensure!(network != &Byte32::default(), "zero Stem network identity");
        Ok(Self {
            network,
            candidate: HeaderContext::after(network, parent, timestamp_ms, transaction_root)?,
            parent,
            capacity,
        })
    }

    pub fn candidate(&self) -> &HeaderContext {
        &self.candidate
    }

    pub(crate) fn network(&self) -> &Byte32 {
        self.network
    }

    pub fn into_candidate(self) -> HeaderContext {
        self.candidate
    }

    pub fn parent(&self) -> &Header {
        self.parent
    }

    pub fn capacity(&self) -> CapacityPolicy {
        self.capacity
    }

    pub fn verify_since(&self, since: Option<Since>, origin: &Origin) -> Result<()> {
        let mature = match since {
            None => true,
            Some(Since::AbsoluteHeight(height)) => self.candidate.height >= height,
            Some(Since::RelativeBlocks(delay)) => {
                self.candidate.height
                    >= origin
                        .height
                        .checked_add(delay)
                        .context("Since height overflow")?
            }
            Some(Since::AbsoluteTime(time)) => self.parent.context.timestamp_ms >= time,
            Some(Since::RelativeTime(delay)) => {
                self.parent.context.timestamp_ms
                    >= origin
                        .timestamp_ms
                        .checked_add(delay)
                        .context("Since time overflow")?
            }
        };
        ensure!(mature, "input Since lock is immature");
        Ok(())
    }
}
