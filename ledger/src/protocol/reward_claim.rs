use anyhow::{Context, Result, bail, ensure};
use ckb_types::{bytes::Bytes, packed::Script, prelude::Entity};
use stem_stake::{
    Hash32, ValidatorKey,
    l1::{DelegationId, StakeCell},
    rewards::RewardSubject,
};

use super::{bytes, compact64};
use crate::{
    strand::prelude::{Builder as WireBuilder, Entity as WireEntity},
    wire,
};

const MAX_EPOCHS: usize = 8;
const MAX_POSITIONS: usize = 32;
const MIN_POSITION_PROOF: usize = 32;
const MAX_POSITION_PROOF: usize = 32 + 256 * 32;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewardClaim {
    /// Zero accrues the proved shares. A positive payout redeems the full resulting balance.
    pub payout: u64,
    pub recipient: Script,
    pub epochs: Vec<RewardEpochProof>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewardEpochProof {
    pub epoch: u64,
    pub registry: Bytes,
    pub committee: Bytes,
    pub positions: Vec<RewardPosition>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RewardPosition {
    Validator {
        key: ValidatorKey,
        metadata: Bytes,
    },
    Delegation {
        id: DelegationId,
        principal: u64,
        proof: Bytes,
    },
}

impl RewardPosition {
    pub fn subject(&self) -> RewardSubject<'_> {
        match self {
            Self::Validator { key, metadata } => RewardSubject::Validator {
                validator: *key,
                metadata,
            },
            Self::Delegation {
                id,
                principal,
                proof,
            } => RewardSubject::Delegation {
                id: *id,
                principal: *principal,
                proof,
            },
        }
    }

    pub fn id(&self) -> (u8, Hash32) {
        match self {
            Self::Validator { key, .. } => (0, *key),
            Self::Delegation { id, .. } => (1, id.commitment()),
        }
    }

    fn encode(&self) -> wire::RewardPosition {
        let (kind, identity, metadata) = match self {
            Self::Validator { key, metadata } => (0, Bytes::copy_from_slice(key), metadata.clone()),
            Self::Delegation {
                id,
                principal,
                proof,
            } => {
                let mut metadata = principal.to_le_bytes().to_vec();
                metadata.extend_from_slice(proof);
                (1, Bytes::copy_from_slice(&id.encode()), metadata.into())
            }
        };
        wire::RewardPosition::new_builder()
            .kind(kind.into())
            .identity(bytes(identity))
            .metadata(bytes(metadata))
            .build()
    }

    fn decode(value: wire::RewardPosition) -> Result<Self> {
        let identity = value.identity().raw_data();
        let metadata = value.metadata().raw_data();
        Ok(match u8::from(value.kind()) {
            0 => {
                ensure!(
                    metadata.len() == StakeCell::SIZE,
                    "invalid Stake Cell metadata"
                );
                Self::Validator {
                    key: identity.as_ref().try_into()?,
                    metadata,
                }
            }
            1 => {
                let id = DelegationId::decode(&identity)?;
                let (principal, proof) = metadata
                    .split_at_checked(8)
                    .context("missing reward principal")?;
                let principal = u64::from_le_bytes(principal.try_into()?);
                ensure!(
                    principal != 0
                        && (MIN_POSITION_PROOF..=MAX_POSITION_PROOF).contains(&proof.len()),
                    "invalid delegation reward proof"
                );
                Self::Delegation {
                    id,
                    principal,
                    proof: metadata.slice(8..),
                }
            }
            _ => bail!("unknown stake reward position kind"),
        })
    }
}

impl RewardClaim {
    pub(super) fn encode(&self) -> wire::RewardClaim {
        wire::RewardClaim::new_builder()
            .payout(compact64(self.payout))
            .recipient(bytes(self.recipient.as_bytes()))
            .epochs(
                wire::RewardEpochProofVec::new_builder()
                    .set(
                        self.epochs
                            .iter()
                            .map(|epoch| {
                                wire::RewardEpochProof::new_builder()
                                    .epoch(compact64(epoch.epoch))
                                    .registry(bytes(epoch.registry.clone()))
                                    .committee(bytes(epoch.committee.clone()))
                                    .positions(
                                        wire::RewardPositionVec::new_builder()
                                            .set(
                                                epoch
                                                    .positions
                                                    .iter()
                                                    .map(RewardPosition::encode)
                                                    .collect(),
                                            )
                                            .build(),
                                    )
                                    .build()
                            })
                            .collect(),
                    )
                    .build(),
            )
            .build()
    }

    pub(super) fn decode(raw: &[u8]) -> Result<Self> {
        let value = wire::RewardClaim::from_slice(raw)?;
        let payout = value.payout().as_reader().value().try_into()?;
        let recipient = value.recipient().raw_data();
        let epochs = value.epochs();
        ensure!(
            epochs.len() <= MAX_EPOCHS && (payout != 0 || !epochs.is_empty()),
            "reward claim requires positions or a payout"
        );
        let mut remaining = MAX_POSITIONS;
        let epochs = epochs
            .into_iter()
            .map(|raw| {
                let positions = raw.positions();
                ensure!(
                    (1..=remaining).contains(&positions.len()),
                    "invalid reward position count"
                );
                remaining -= positions.len();
                let positions = positions
                    .into_iter()
                    .map(RewardPosition::decode)
                    .collect::<Result<Vec<_>>>()?;
                ensure!(
                    positions.windows(2).all(|pair| pair[0].id() < pair[1].id()),
                    "reward positions must be ordered and unique"
                );
                Ok(RewardEpochProof {
                    epoch: raw.epoch().as_reader().value().try_into()?,
                    registry: raw.registry().raw_data(),
                    committee: raw.committee().raw_data(),
                    positions,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            epochs.windows(2).all(|pair| pair[0].epoch < pair[1].epoch),
            "reward epochs must be ordered and unique"
        );
        Ok(Self {
            payout,
            recipient: Script::from_slice(&recipient)?,
            epochs,
        })
    }
}

/// Protocol-wide replay key for one beneficiary position in one epoch.
pub fn reward_claim_key(epoch: u64, kind: u8, id: &Hash32) -> Vec<u8> {
    [
        b"fees/stake-claims/".as_slice(),
        &epoch.to_le_bytes(),
        &[kind],
        id,
    ]
    .concat()
}

pub fn reward_balance_key(owner: &Hash32) -> Vec<u8> {
    [b"fees/stake-balances/".as_slice(), owner].concat()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Transaction, protocol::Operation};

    #[test]
    fn reward_claims_canonicalize_replay_identity_across_bundles() -> Result<()> {
        let recipient = Script::default();
        let validator = RewardPosition::Validator {
            key: [1; 32],
            metadata: Bytes::from(vec![2; StakeCell::SIZE]),
        };
        let delegation = RewardPosition::Delegation {
            id: DelegationId {
                registry: [3; 32],
                validator: [1; 32],
                owner: [4; 32],
                nonce: [5; 32],
            },
            principal: 100,
            proof: Bytes::from(vec![0; 32]),
        };
        let epoch = RewardEpochProof {
            epoch: 0,
            registry: Bytes::from_static(b"registry"),
            committee: Bytes::from_static(b"committee"),
            positions: vec![validator.clone(), delegation.clone()],
        };
        let claim = RewardClaim {
            payout: 100,
            recipient,
            epochs: vec![epoch.clone()],
        };
        let transaction = Transaction::reward_claim(claim.clone())?;
        assert_eq!(
            Transaction::from_slice(transaction.as_slice())?.operation(),
            Some(Operation::RewardClaim(claim.clone())),
        );
        let mut credit = claim.clone();
        credit.payout = 0;
        assert!(Transaction::reward_claim(credit.clone()).is_ok());
        credit.epochs.clear();
        assert!(Transaction::reward_claim(credit.clone()).is_err());
        credit.payout = 100;
        assert!(Transaction::reward_claim(credit).is_ok());

        let mut invalid = claim.clone();
        invalid.epochs[0].positions.reverse();
        assert!(Transaction::reward_claim(invalid).is_err());

        let mut invalid = claim.clone();
        let RewardPosition::Delegation { proof, .. } = &mut invalid.epochs[0].positions[1] else {
            unreachable!()
        };
        *proof = Bytes::from(vec![0; 64]);
        invalid.epochs[0].positions.insert(1, delegation);
        assert!(Transaction::reward_claim(invalid).is_err());

        let mut invalid = claim.clone();
        invalid.epochs.push(epoch.clone());
        assert!(Transaction::reward_claim(invalid).is_err());

        let mut next = claim;
        next.epochs.push(RewardEpochProof { epoch: 1, ..epoch });
        assert!(Transaction::reward_claim(next).is_ok());
        let (kind, id) = validator.id();
        assert_ne!(
            reward_claim_key(0, kind, &id),
            reward_claim_key(1, kind, &id)
        );
        assert_ne!(reward_claim_key(0, kind, &id), reward_claim_key(0, 1, &id));
        Ok(())
    }
}
