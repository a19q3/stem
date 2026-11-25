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
    accrued_stake_rewards: u64,
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

    pub fn accrued_stake_rewards(&self) -> u64 {
        self.accrued_stake_rewards
    }

    pub fn unallocated(&self) -> u64 {
        // Decoding checks coverage. Every successful transition preserves it.
        self.liquid - self.circulating - self.pending - self.accrued_stake_rewards
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

    pub fn accrue_stake_rewards(mut self, amount: u64) -> Result<Self> {
        self.unallocated()
            .checked_sub(amount)
            .context("stake rewards exceed unallocated reserve")?;
        self.accrued_stake_rewards = self
            .accrued_stake_rewards
            .checked_add(amount)
            .context("accrued stake reward overflow")?;
        Ok(self)
    }

    pub fn reserve_stake_payout(mut self, amount: u64) -> Result<Self> {
        self.accrued_stake_rewards = self
            .accrued_stake_rewards
            .checked_sub(amount)
            .context("stake payout exceeds accrued rewards")?;
        self.reserve_fee_payout(amount)
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
            .accrued_stake_rewards(compact64(self.accrued_stake_rewards))
            .build()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let data = wire::BridgeBacking::from_slice(bytes)?;
        let record = Self {
            liquid: data.liquid().as_reader().value().try_into()?,
            circulating: data.circulating().as_reader().value().try_into()?,
            pending: data.pending().as_reader().value().try_into()?,
            accrued_stake_rewards: data
                .accrued_stake_rewards()
                .as_reader()
                .value()
                .try_into()?,
        };
        record
            .liquid
            .checked_sub(record.circulating)
            .and_then(|remaining| remaining.checked_sub(record.pending))
            .and_then(|remaining| remaining.checked_sub(record.accrued_stake_rewards))
            .context("uncovered bridge liability")?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accrued_rewards_remain_backed_through_reservation_and_settlement() -> Result<()> {
        let initial = BridgeBacking::default()
            .credit_mint(1000)?
            .extinguish_fee(800)?;
        assert!(initial.accrue_stake_rewards(801).is_err());
        let accrued = initial.accrue_stake_rewards(52)?.accrue_stake_rewards(78)?;
        assert_eq!(accrued.liquid(), initial.liquid());
        assert_eq!(accrued.circulating(), initial.circulating());
        assert_eq!(accrued.pending(), 0);
        assert_eq!(accrued.unallocated(), 670);
        assert!(accrued.reserve_fee_payout(671).is_err());
        assert!(accrued.reserve_stake_payout(131).is_err());
        assert_eq!(BridgeBacking::decode(&accrued.encode())?, accrued);

        let reserved = accrued.reserve_stake_payout(130)?;
        assert_eq!(reserved.accrued_stake_rewards(), 0);
        assert_eq!(reserved.unallocated(), accrued.unallocated());
        assert_eq!(reserved.pending(), 130);
        let settled = reserved.settle_withdrawal(130)?;
        assert_eq!(settled.liquid(), 870);
        assert_eq!(settled.unallocated(), accrued.unallocated());
        assert_eq!(settled.pending(), 0);
        Ok(())
    }
}
