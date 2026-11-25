use anyhow::{Context, Result, ensure};
use std::collections::BTreeSet;

use ckb_types::{
    bytes::Bytes,
    packed::{Byte32, Script},
    prelude::Entity as MoleculeEntity,
};

use crate::{
    protocol::{byte32, bytes, compact64, hash32},
    strand::prelude::*,
    wire,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum FeeFund {
    Validators = 0,
    Development = 1,
    Ecosystem = 2,
    ProofOfWork = 3,
}

impl FeeFund {
    pub const ALL: [Self; 4] = [
        Self::Validators,
        Self::Development,
        Self::Ecosystem,
        Self::ProofOfWork,
    ];
    pub const BASIS_POINTS: u16 = 10_000;

    pub fn from_id(id: u8) -> Result<Self> {
        Self::ALL
            .get(usize::from(id))
            .copied()
            .context("unknown fee fund")
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Validators => "validators-and-delegators",
            Self::Development => "dev-fund",
            Self::Ecosystem => "ecosystem-fund",
            Self::ProofOfWork => "pow-security-fund",
        }
    }

    pub fn allocation(total_fees: u64, basis_points: u16) -> Result<u64> {
        Ok(
            (u128::from(total_fees) * u128::from(basis_points) / u128::from(Self::BASIS_POINTS))
                .try_into()?,
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeeClaim {
    pub fund: FeeFund,
    pub amount: u64,
    pub claimed_before: u64,
}

impl FeeClaim {
    pub(super) fn encode(&self) -> wire::FeeClaim {
        wire::FeeClaim::new_builder()
            .fund((self.fund as u8).into())
            .amount(compact64(self.amount))
            .claimed_before(compact64(self.claimed_before))
            .build()
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        let claim = wire::FeeClaim::from_slice(bytes)?;
        let amount = claim.amount().as_reader().value().try_into()?;
        ensure!(amount > 0, "fee claim amount must be positive");
        Ok(Self {
            fund: FeeFund::from_id(claim.fund().into())?,
            amount,
            claimed_before: claim.claimed_before().as_reader().value().try_into()?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeFundDestination {
    pub basis_points: u16,
    pub recipient: Script,
}

impl FeeFundDestination {
    fn encode(&self) -> wire::FeeFundDestination {
        wire::FeeFundDestination::new_builder()
            .basis_points(wire::Uint16::new_unchecked(Bytes::copy_from_slice(
                &self.basis_points.to_le_bytes(),
            )))
            .recipient(bytes(self.recipient.as_bytes()))
            .build()
    }

    fn decode(destination: wire::FeeFundDestination) -> Result<Self> {
        Ok(Self {
            basis_points: u16::from_le_bytes(destination.basis_points().as_slice().try_into()?),
            recipient: Script::from_slice(&destination.recipient().raw_data())?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeFundPolicy {
    pub lane: Byte32,
    pub funds: [FeeFundDestination; 4],
}

impl FeeFundPolicy {
    pub fn validate(&self) -> Result<()> {
        let mut recipients = BTreeSet::new();
        let mut shares = 0u32;
        for fund in &self.funds {
            ensure!(
                fund.basis_points > 0 && recipients.insert(fund.recipient.as_slice()),
                "zero share or duplicate fee fund recipient"
            );
            shares += u32::from(fund.basis_points);
        }
        ensure!(
            shares == u32::from(FeeFund::BASIS_POINTS),
            "fee fund shares must sum to 10000 basis points"
        );
        Ok(())
    }

    pub fn encode(&self) -> Bytes {
        wire::FeeFundPolicy::new_builder()
            .lane(byte32(&self.lane))
            .validators(self.funds[FeeFund::Validators as usize].encode())
            .development(self.funds[FeeFund::Development as usize].encode())
            .ecosystem(self.funds[FeeFund::Ecosystem as usize].encode())
            .pow(self.funds[FeeFund::ProofOfWork as usize].encode())
            .build()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let record = wire::FeeFundPolicy::from_slice(bytes)?;
        let policy = Self {
            lane: hash32(record.lane()),
            funds: [
                FeeFundDestination::decode(record.validators())?,
                FeeFundDestination::decode(record.development())?,
                FeeFundDestination::decode(record.ecosystem())?,
                FeeFundDestination::decode(record.pow())?,
            ],
        };
        policy.validate()?;
        Ok(policy)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FeePayout {
    pub queued: u64,
    pub paid: u64,
}

impl FeePayout {
    fn encode(self) -> wire::FeePayout {
        wire::FeePayout::new_builder()
            .queued(compact64(self.queued))
            .paid(compact64(self.paid))
            .build()
    }

    fn decode(payout: wire::FeePayout) -> Result<Self> {
        let queued = payout.queued().as_reader().value().try_into()?;
        let paid = payout.paid().as_reader().value().try_into()?;
        ensure!(paid <= queued, "fee payouts exceed queued claims");
        Ok(Self { queued, paid })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FeePayouts([FeePayout; 4]);

impl FeePayouts {
    pub fn fund(&self, fund: FeeFund) -> FeePayout {
        self.0[fund as usize]
    }

    pub fn queue(&mut self, claim: FeeClaim, allocation: u64) -> Result<()> {
        ensure!(claim.amount > 0, "fee claim amount must be positive");
        let fund = &mut self.0[claim.fund as usize];
        ensure!(fund.queued == claim.claimed_before, "stale fee claim");
        let queued = fund
            .queued
            .checked_add(claim.amount)
            .context("fee claims overflow")?;
        ensure!(queued <= allocation, "fee claim exceeds fund allocation");
        fund.queued = queued;
        Ok(())
    }

    pub fn settle(&mut self, fund: FeeFund, amount: u64) -> Result<()> {
        let fund = &mut self.0[fund as usize];
        let paid = fund
            .paid
            .checked_add(amount)
            .context("fee payouts overflow")?;
        ensure!(paid <= fund.queued, "fee payouts exceed queued claims");
        fund.paid = paid;
        Ok(())
    }

    pub fn total_queued(&self) -> Result<u64> {
        self.0.iter().try_fold(0u64, |total, fund| {
            total
                .checked_add(fund.queued)
                .context("fee claims overflow")
        })
    }

    pub fn encode(&self) -> Bytes {
        wire::FeePayouts::new_builder()
            .validators(self.fund(FeeFund::Validators).encode())
            .development(self.fund(FeeFund::Development).encode())
            .ecosystem(self.fund(FeeFund::Ecosystem).encode())
            .pow(self.fund(FeeFund::ProofOfWork).encode())
            .build()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let record = wire::FeePayouts::from_slice(bytes)?;
        let payouts = Self([
            FeePayout::decode(record.validators())?,
            FeePayout::decode(record.development())?,
            FeePayout::decode(record.ecosystem())?,
            FeePayout::decode(record.pow())?,
        ]);
        payouts.total_queued()?;
        Ok(payouts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Transaction, protocol::BridgeBacking};

    #[test]
    fn fee_claims_preserve_backing_and_reject_stale_debits() -> Result<()> {
        let fee = 100_003;
        let percentages = [6500, 1500, 1000, 1000];
        let mut backing = BridgeBacking::default()
            .credit_mint(2 * fee)?
            .extinguish_fee(fee)?;
        assert!(backing.reserve_fee_payout(fee + 1).is_err());
        let mut record = FeePayouts::default();
        for (fund, basis_points) in FeeFund::ALL.into_iter().zip(percentages) {
            let amount = FeeFund::allocation(fee, basis_points)?;
            assert!(FeeFund::allocation(u64::MAX, basis_points)? > amount);
            let claim = FeeClaim {
                fund,
                amount,
                claimed_before: 0,
            };
            let transaction = Transaction::fee_claim(claim)?;
            assert_eq!(
                Transaction::from_slice(transaction.as_slice())?.operation(),
                Some(super::super::Operation::FeeClaim(claim))
            );
            assert!(transaction.certificate().is_none());
            assert!(record.settle(fund, 1).is_err());
            assert!(
                record
                    .queue(
                        FeeClaim {
                            amount: amount + 1,
                            ..claim
                        },
                        amount
                    )
                    .is_err()
            );
            record.queue(claim, amount)?;
            assert!(record.queue(claim, amount).is_err());
            backing = backing.reserve_fee_payout(amount)?;
            assert_eq!(backing.liquid(), fee * 2);
            assert_eq!(backing.circulating(), fee);
        }
        assert_eq!(record.total_queued()? + backing.unallocated(), fee);
        assert_eq!(backing.unallocated(), 2);
        for fund in FeeFund::ALL {
            let queued = record.fund(fund).queued;
            record.settle(fund, queued)?;
            assert!(record.settle(fund, 1).is_err());
            backing = backing.settle_withdrawal(queued)?;
        }
        assert_eq!(backing.liquid(), fee + 2);
        assert_eq!(backing.pending(), 0);
        assert_eq!(record, FeePayouts::decode(&record.encode())?);
        Ok(())
    }
}
