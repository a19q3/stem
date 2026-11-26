//! Strand encodings shared by archive tooling and independently verified stake receipts.
use anyhow::{Result, ensure};
use stem_stake::{
    AcceptedSnapshot, Anchor, Hash32, Member, RegistryAnchor, delegation, l1::DelegationId,
    rewards::EpochRewards,
};

use crate::{strand::prelude::Reader, wire};

const MAX_PATH: usize = 32 + 256 * 32;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StakePrincipalProof {
    pub identity: DelegationId,
    pub principal: u64,
    pub path: Vec<u8>,
}

impl StakePrincipalProof {
    pub fn verify(&self, network: &Hash32, root: &Hash32) -> Result<()> {
        Ok(delegation::verify_position(
            network,
            self.identity,
            root,
            self.principal,
            &self.path,
        )?)
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        ensure!(self.path.len() <= MAX_PATH, "oversized delegation path");
        DelegationId::decode(&self.identity.encode())?;
        let mut out = self.identity.encode().to_vec();
        out.extend_from_slice(&self.principal.to_le_bytes());
        wire::Bytes::write_count(self.path.len(), &mut out)?;
        out.extend_from_slice(&self.path);
        Ok(out)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= DelegationId::SIZE + 12 + MAX_PATH,
            "oversized delegation proof"
        );
        let p = wire::StakePrincipalProofReader::from_slice(bytes)?;
        Ok(Self {
            identity: DelegationId::decode(p.identity().as_slice())?,
            principal: u64::from_le_bytes(p.principal().as_slice().try_into()?),
            path: p.path().raw_data().to_vec(),
        })
    }
}

/// The persisted rewards record and the CKB verifier share the same fixed Strand layout.
pub fn decode_epoch_rewards(data: &[u8]) -> Result<EpochRewards> {
    let view = wire::EpochRewardsReader::from_slice(data)?;
    Ok(EpochRewards::decode(view.as_slice())?)
}

pub fn encode_accepted(accepted: AcceptedSnapshot, members: &[Member]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&accepted.epoch.to_le_bytes());
    bytes.extend_from_slice(&accepted.anchor.block.height.to_le_bytes());
    bytes.extend_from_slice(&accepted.anchor.block.hash);
    bytes.extend_from_slice(&accepted.anchor.sequence.to_le_bytes());
    bytes.extend_from_slice(&accepted.anchor.commitment);
    append_members(&mut bytes, members);
    bytes
}

pub fn encode_members(members: &[Member]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(members.len() * 40);
    append_members(&mut bytes, members);
    bytes
}

fn append_members(bytes: &mut Vec<u8>, members: &[Member]) {
    for member in members {
        bytes.extend_from_slice(&member.key);
        bytes.extend_from_slice(&member.power.to_le_bytes());
    }
}

pub fn decode_accepted(bytes: &[u8]) -> Result<(AcceptedSnapshot, Vec<Member>)> {
    let width = wire::AcceptedStakePrefixReader::span(bytes)?;
    let (prefix, members) = bytes.split_at(width);
    let p = wire::AcceptedStakePrefixReader::from_slice(prefix)?;
    Ok((
        AcceptedSnapshot {
            epoch: u64::from_le_bytes(p.epoch().as_slice().try_into()?),
            anchor: RegistryAnchor {
                block: Anchor {
                    height: u64::from_le_bytes(p.height().as_slice().try_into()?),
                    hash: p.hash().as_slice().try_into()?,
                },
                sequence: u64::from_le_bytes(p.sequence().as_slice().try_into()?),
                commitment: p.commitment().as_slice().try_into()?,
            },
        },
        decode_members(members)?,
    ))
}

pub fn decode_members(bytes: &[u8]) -> Result<Vec<Member>> {
    const WIDTH: usize = 32 + 8;
    ensure!(
        bytes.len().is_multiple_of(WIDTH) && bytes.len() / WIDTH <= stem_stake::MAX_RECORDS,
        "invalid committee record"
    );
    bytes
        .chunks_exact(WIDTH)
        .map(|raw| {
            let member = wire::StakeMemberReader::from_slice(raw)?;
            Ok(Member {
                key: member.key().as_slice().try_into()?,
                power: u64::from_le_bytes(member.power().as_slice().try_into()?),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use stem_stake::delegation::DelegationTree;

    #[test]
    fn strand_principal_membership_and_absence() -> Result<()> {
        let network = [1; 32];
        let id = DelegationId {
            registry: [2; 32],
            validator: [3; 32],
            owner: [4; 32],
            nonce: [5; 32],
        };
        let other = DelegationId {
            nonce: [6; 32],
            ..id
        };
        let mut tree = DelegationTree::new(network, id.registry, id.validator);
        tree.insert(id, 15)?;
        for (identity, principal) in [(id, 15), (other, 0)] {
            let mut proof = StakePrincipalProof {
                identity,
                principal,
                path: tree.prove(&identity.commitment()),
            };
            let bytes = proof.encode()?;
            assert_eq!(StakePrincipalProof::decode(&bytes)?, proof);
            proof.verify(&network, &tree.root())?;
            assert!(proof.verify(&network, &[9; 32]).is_err());
            proof.principal += 1;
            assert!(proof.verify(&network, &tree.root()).is_err());
        }
        Ok(())
    }
}
