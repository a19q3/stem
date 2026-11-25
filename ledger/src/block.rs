//! Native commitments in their construction order.

use crate::telegraph::prelude::{Builder as WireBuilder, Entity as WireEntity};
use anyhow::{Context, Result, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::*};

use crate::{hash, history::History, proof, wire};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeaderContext {
    pub height: u64,
    pub timestamp_ms: u64,
    pub parent: Byte32,
    pub transaction_root: Byte32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StateCommitment {
    pub cell_root: Byte32,
    pub system_root: Byte32,
    pub history_root: Byte32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionCommitment {
    pub transaction_root: Byte32,
    pub receipt_root: Byte32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Header {
    pub context: HeaderContext,
    pub state: StateCommitment,
    pub receipt_root: Byte32,
}

impl HeaderContext {
    pub fn after(
        network: &Byte32,
        parent: &Header,
        timestamp_ms: u64,
        transaction_root: Byte32,
    ) -> Result<Self> {
        ensure!(
            timestamp_ms >= parent.context.timestamp_ms,
            "consensus time regressed"
        );
        Ok(Self {
            height: parent
                .context
                .height
                .checked_add(1)
                .context("height overflow")?,
            timestamp_ms,
            parent: parent.id(network),
            transaction_root,
        })
    }

    fn encode(&self) -> wire::HeaderContext {
        wire::HeaderContext::new_builder()
            .height(uint64(self.height))
            .timestamp_ms(uint64(self.timestamp_ms))
            .parent(byte32(&self.parent))
            .transaction_root(byte32(&self.transaction_root))
            .build()
    }

    fn decode(context: wire::HeaderContext) -> Self {
        Self {
            height: number(context.height()),
            timestamp_ms: number(context.timestamp_ms()),
            parent: hash32(context.parent()),
            transaction_root: hash32(context.transaction_root()),
        }
    }

    pub fn id(&self, network: &Byte32) -> Byte32 {
        hash(
            b"Stem block context\0",
            network,
            &[self.encode().as_slice()],
        )
    }
}

impl Header {
    pub fn next(
        network: &Byte32,
        parent: &Self,
        history: &mut History,
        context: HeaderContext,
        cell_root: Byte32,
        system_root: Byte32,
        receipt_root: Byte32,
    ) -> Result<Self> {
        ensure!(
            HeaderContext::after(
                network,
                parent,
                context.timestamp_ms,
                context.transaction_root.clone()
            )? == context,
            "unexpected block context"
        );
        ensure!(
            history.network() == network
                && history.count() == parent.context.height
                && history.root() == parent.state.history_root,
            "MMR disagrees with parent"
        );
        history.append(parent.context.height, &context.parent)?;
        Ok(Self {
            context,
            state: StateCommitment {
                cell_root,
                system_root,
                history_root: history.root(),
            },
            receipt_root,
        })
    }

    pub fn encode(&self) -> Bytes {
        let state = wire::StateCommitment::new_builder()
            .cell_root(byte32(&self.state.cell_root))
            .system_root(byte32(&self.state.system_root))
            .history_root(byte32(&self.state.history_root))
            .build();
        wire::Header::new_builder()
            .context(self.context.encode())
            .state(state)
            .receipt_root(byte32(&self.receipt_root))
            .build()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let header = wire::Header::from_slice(bytes)?;
        let state = header.state();
        Ok(Self {
            context: HeaderContext::decode(header.context()),
            state: StateCommitment {
                cell_root: hash32(state.cell_root()),
                system_root: hash32(state.system_root()),
                history_root: hash32(state.history_root()),
            },
            receipt_root: hash32(header.receipt_root()),
        })
    }

    pub fn id(&self, network: &Byte32) -> Byte32 {
        hash(b"Stem Header\0", network, &[&self.encode()])
    }

    pub fn app_hash(&self, network: &Byte32) -> Byte32 {
        hash(
            b"Stem AppHash\0",
            network,
            &[
                &self.context.height.to_le_bytes(),
                self.id(network).as_slice(),
            ],
        )
    }

    pub fn execution(&self) -> ExecutionCommitment {
        ExecutionCommitment {
            transaction_root: self.context.transaction_root.clone(),
            receipt_root: self.receipt_root.clone(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt {
    pub fee: u64,
    pub cycles: u64,
    pub protocol_result: Bytes,
}

impl Receipt {
    pub fn encode(&self) -> Bytes {
        wire::Receipt::new_builder()
            .fee(
                wire::CompactUint64::new_builder()
                    .set(self.fee.into())
                    .build(),
            )
            .cycles(
                wire::CompactUint64::new_builder()
                    .set(self.cycles.into())
                    .build(),
            )
            .protocol_result(wire::Bytes::new_unchecked(
                self.protocol_result.pack().as_bytes(),
            ))
            .build()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let receipt = wire::Receipt::from_slice(bytes)?;
        Ok(Self {
            fee: receipt.fee().as_reader().value().try_into()?,
            cycles: receipt.cycles().as_reader().value().try_into()?,
            protocol_result: receipt.protocol_result().raw_data(),
        })
    }

    pub fn root(network: &Byte32, receipts: &[Self]) -> Result<Byte32> {
        Ok(proof::Tree::receipts(network.clone(), receipts)?.root())
    }
}

fn byte32(value: &Byte32) -> wire::Byte32 {
    wire::Byte32::new_unchecked(value.as_bytes())
}

fn hash32(value: wire::Byte32) -> Byte32 {
    Byte32::new_unchecked(value.as_bytes())
}

fn uint64(value: u64) -> wire::Uint64 {
    wire::Uint64::new_unchecked(Bytes::copy_from_slice(&value.to_le_bytes()))
}

fn number(value: wire::Uint64) -> u64 {
    u64::from_le_bytes(value.as_slice().try_into().expect("Uint64"))
}
