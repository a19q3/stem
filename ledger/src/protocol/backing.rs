use anyhow::{Context, Result};
use ckb_types::bytes::Bytes;

use crate::{
    protocol::compact64,
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

/// Aggregate CKB capacity recognized from authenticated vault transitions.
/// Wrapped token units are still tracked separately for each lane.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BridgeBacking {
    liquid: u64,
    circulating: u64,
    pending: u64,
}

impl BridgeBacking {
    pub fn liquid(&self) -> u64 {
        self.liquid
    }

    pub fn circulating(&self) -> u64 {
        self.circulating
    }

    pub fn pending(&self) -> u64 {
        self.pending
    }

    pub fn unallocated(&self) -> u64 {
        // Decoding checks coverage. Every successful transition preserves it.
        self.liquid - self.circulating - self.pending
    }

    pub fn credit_mint(mut self, amount: u64) -> Result<Self> {
        self.liquid = self
            .liquid
            .checked_add(amount)
            .context("bridge reserve overflow")?;
        self.circulating = self
            .circulating
            .checked_add(amount)
            .context("bridge issuance overflow")?;
        Ok(self)
    }

    pub fn reserve_withdrawal(mut self, amount: u64) -> Result<Self> {
        self.circulating = self
            .circulating
            .checked_sub(amount)
            .context("withdrawal exceeds circulation")?;
        self.pending = self
            .pending
            .checked_add(amount)
            .context("bridge pending liability overflow")?;
        Ok(self)
    }

    pub fn settle_withdrawal(mut self, amount: u64) -> Result<Self> {
        self.pending = self
            .pending
            .checked_sub(amount)
            .context("withdrawal exceeds pending liability")?;
        self.liquid = self
            .liquid
            .checked_sub(amount)
            .context("withdrawal exceeds reserve")?;
        Ok(self)
    }

    pub fn reserve_fee_payout(mut self, amount: u64) -> Result<Self> {
        self.unallocated()
            .checked_sub(amount)
            .context("fee payout exceeds unallocated reserve")?;
        self.pending = self
            .pending
            .checked_add(amount)
            .context("bridge pending liability overflow")?;
        Ok(self)
    }

    pub fn extinguish_fee(mut self, amount: u64) -> Result<Self> {
        self.circulating = self
            .circulating
            .checked_sub(amount)
            .context("fees exceed circulation")?;
        Ok(self)
    }

    pub fn encode(&self) -> Bytes {
        wire::BridgeBacking::new_builder()
            .liquid(compact64(self.liquid))
            .circulating(compact64(self.circulating))
            .pending(compact64(self.pending))
            .build()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let data = wire::BridgeBacking::from_slice(bytes)?;
        let record = Self {
            liquid: data.liquid().as_reader().value().try_into()?,
            circulating: data.circulating().as_reader().value().try_into()?,
            pending: data.pending().as_reader().value().try_into()?,
        };
        record
            .liquid
            .checked_sub(record.circulating)
            .and_then(|remaining| remaining.checked_sub(record.pending))
            .context("uncovered bridge liability")?;
        Ok(record)
    }
}
