//! Deterministic allocation from an epoch's certified fee budget.

use alloc::vec::Vec;

use crate::{
    Error, Hash32, Registry, Result, ValidatorKey, delegation,
    jolteon::{CommitteeChange, roster_id},
    l1::{DelegationId, StakeCell},
};

pub const CURRENT_EPOCH: &[u8] = b"stake/rewards/current";

pub fn epoch_key(epoch: u64) -> Vec<u8> {
    [b"stake/rewards/epoch/".as_slice(), &epoch.to_le_bytes()].concat()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EpochRewards {
    pub epoch: u64,
    pub start_height: u64,
    pub end_height: u64,
    pub start_fees: u64,
    pub end_fees: u64,
    pub budget: u64,
    pub registry: Hash32,
    pub registry_commitment: Hash32,
    pub committee_commitment: Hash32,
}

impl EpochRewards {
    pub const SIZE: usize = 6 * 8 + 3 * 32;

    pub fn start(
        committee: &CommitteeChange,
        registry: Hash32,
        commitment: Hash32,
        fees: u64,
    ) -> Self {
        Self {
            epoch: committee.epoch,
            start_height: committee.activation_height,
            end_height: 0,
            start_fees: fees,
            end_fees: 0,
            budget: 0,
            registry,
            registry_commitment: commitment,
            committee_commitment: roster_id(&committee.encode()),
        }
    }

    pub fn close(self, height: u64, fees: u64, budget: u64) -> Result<Self> {
        if self.end_height != 0
            || height < self.start_height
            || fees < self.start_fees
            || budget > fees - self.start_fees
        {
            return Err(Error::Sequence);
        }
        Ok(Self {
            end_height: height,
            end_fees: fees,
            budget,
            ..self
        })
    }

    pub fn encode(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        for (field, value) in bytes[..48].chunks_exact_mut(8).zip([
            self.epoch,
            self.start_height,
            self.end_height,
            self.start_fees,
            self.end_fees,
            self.budget,
        ]) {
            field.copy_from_slice(&value.to_le_bytes());
        }
        bytes[48..80].copy_from_slice(&self.registry);
        bytes[80..112].copy_from_slice(&self.registry_commitment);
        bytes[112..].copy_from_slice(&self.committee_commitment);
        bytes
    }

    pub fn decode(data: &[u8]) -> Result<Self> {
        let bytes: &[u8; Self::SIZE] = data.try_into().map_err(|_| Error::Encoding)?;
        let number = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
        let record = Self {
            epoch: number(0),
            start_height: number(8),
            end_height: number(16),
            start_fees: number(24),
            end_fees: number(32),
            budget: number(40),
            registry: bytes[48..80].try_into().unwrap(),
            registry_commitment: bytes[80..112].try_into().unwrap(),
            committee_commitment: bytes[112..].try_into().unwrap(),
        };
        if record.start_height == 0
            || [
                record.registry,
                record.registry_commitment,
                record.committee_commitment,
            ]
            .contains(&[0; 32])
            || if record.end_height == 0 {
                record.end_fees != 0 || record.budget != 0
            } else {
                record.end_height < record.start_height
                    || record.end_fees < record.start_fees
                    || record.budget > record.end_fees - record.start_fees
            }
        {
            return Err(Error::Encoding);
        }
        Ok(record)
    }

    pub fn quote(
        self,
        network: &Hash32,
        registry: &Registry,
        committee: &CommitteeChange,
        subject: RewardSubject<'_>,
    ) -> Result<RewardQuote> {
        self.bind(network, registry, committee)?.quote(subject)
    }

    pub fn bind<'a>(
        self,
        network: &Hash32,
        registry: &'a Registry,
        committee: &'a CommitteeChange,
    ) -> Result<RewardTerms<'a>> {
        if self.end_height == 0
            || committee.epoch != self.epoch
            || committee.activation_height != self.start_height
            || roster_id(&committee.encode()) != self.committee_commitment
            || &registry.network_id() != network
            || registry.commitment() != self.registry_commitment
        {
            return Err(Error::Anchor);
        }
        let total_power = committee.members.iter().try_fold(0u64, |total, member| {
            total.checked_add(member.power).ok_or(Error::Overflow)
        })?;
        Ok(RewardTerms {
            record: self,
            registry,
            committee,
            total_power,
        })
    }
}

pub struct RewardTerms<'a> {
    record: EpochRewards,
    registry: &'a Registry,
    committee: &'a CommitteeChange,
    total_power: u64,
}

impl RewardTerms<'_> {
    pub fn quote(&self, subject: RewardSubject<'_>) -> Result<RewardQuote> {
        let validator = match subject {
            RewardSubject::Validator { validator, .. } => validator,
            RewardSubject::Delegation { id, .. } => id.validator,
        };
        let member = self
            .committee
            .members
            .binary_search_by_key(&validator, |entry| entry.key)
            .map(|i| self.committee.members[i])
            .map_err(|_| Error::Committee)?;
        let stake = self.registry.record(&validator).ok_or(Error::Identity)?;
        let gross = portion(self.record.budget, member.power, self.total_power)?;
        let self_reward = portion(gross, stake.bonded_amount, stake.total_bond()?)?;
        let delegated_reward = gross - self_reward;
        let commission = portion(delegated_reward, u64::from(stake.commission_bps), 10_000)?;
        match subject {
            RewardSubject::Validator { metadata, .. } => {
                let cell = StakeCell::decode(metadata)?;
                if cell.commitment() != stake.stake_cell_hash {
                    return Err(Error::Identity);
                }
                Ok(RewardQuote {
                    owner: cell.owner,
                    amount: self_reward + commission,
                    commission,
                })
            }
            RewardSubject::Delegation {
                id,
                principal,
                proof,
            } => {
                if id.registry != self.record.registry || principal > stake.delegated_amount {
                    return Err(Error::Identity);
                }
                delegation::verify(
                    &self.registry.network_id(),
                    id,
                    &stake.delegation_root,
                    principal,
                    proof,
                )?;
                let amount = portion(
                    delegated_reward - commission,
                    principal,
                    stake.delegated_amount,
                )?;
                Ok(RewardQuote {
                    owner: id.owner,
                    amount,
                    commission: 0,
                })
            }
        }
    }
}

pub enum RewardSubject<'a> {
    Validator {
        validator: ValidatorKey,
        metadata: &'a [u8],
    },
    Delegation {
        id: DelegationId,
        principal: u64,
        proof: &'a [u8],
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RewardQuote {
    pub owner: Hash32,
    pub amount: u64,
    pub commission: u64,
}

fn portion(budget: u64, share: u64, total: u64) -> Result<u64> {
    if share > total || total == 0 {
        return Err(Error::Configuration);
    }
    Ok((u128::from(budget) * u128::from(share) / u128::from(total)) as u64)
}
