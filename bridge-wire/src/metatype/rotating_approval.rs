//! A detached approval against an authenticated CKB key generation.

use super::{Hash, witness::GroupWitness};
use crate::hash;

pub const ACTION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Authority {
    pub generation: u64,
    pub public_key: Hash,
}

impl Authority {
    pub const SIZE: usize = 40;

    pub fn decode(raw: &[u8]) -> Option<Self> {
        let (generation, key) = raw.split_first_chunk::<8>()?;
        let public_key: Hash = key.try_into().ok()?;
        (public_key != [0; 32]).then_some(Self {
            generation: u64::from_le_bytes(*generation),
            public_key,
        })
    }

    pub fn is_successor(&self, previous: &Self) -> bool {
        previous.generation.checked_add(1) == Some(self.generation)
            && previous.public_key != self.public_key
    }

    pub fn digest(
        &self,
        network: &Hash,
        deployment: Option<&Hash>,
        rule: &Hash,
        transaction: &Hash,
    ) -> Hash {
        hash(
            b"Stem Metatype rotating approval\0",
            &[
                network,
                deployment.unwrap_or(&[0; 32]),
                rule,
                &self.generation.to_le_bytes(),
                &self.public_key,
                transaction,
            ],
        )
    }
}

pub struct Approval<'a> {
    pub object: &'a Hash,
    pub registry: &'a Hash,
}

impl<'a> Approval<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        let (object, registry) = raw.split_first_chunk::<32>()?;
        let registry: &Hash = registry.try_into().ok()?;
        (![object, registry].contains(&&[0; 32])).then_some(Self { object, registry })
    }
}

pub fn witness_signature<'a>(row: GroupWitness<'a>) -> Option<&'a [u8; 64]> {
    if row.action != ACTION || !row.message.is_empty() || !row.proof.is_empty() {
        return None;
    }
    row.seal.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authority_rotation_binds_generation_deployment_and_transaction() {
        let first = Authority::decode(&[[0; 8].as_slice(), &[1; 32]].concat()).unwrap();
        let second =
            Authority::decode(&[1u64.to_le_bytes().as_slice(), &[2; 32]].concat()).unwrap();
        assert!(second.is_successor(&first));
        assert!(!first.is_successor(&second));
        assert!(
            !Authority {
                public_key: first.public_key,
                ..second
            }
            .is_successor(&first)
        );
        assert!(
            !Authority {
                generation: 2,
                ..second
            }
            .is_successor(&first)
        );
        assert!(!first.is_successor(&Authority {
            generation: u64::MAX,
            ..second
        }));
        let original = first.digest(&[3; 32], None, &[4; 32], &[5; 32]);
        assert_ne!(original, second.digest(&[3; 32], None, &[4; 32], &[5; 32]));
        assert_ne!(original, first.digest(&[7; 32], None, &[4; 32], &[5; 32]));
        assert_ne!(
            original,
            first.digest(&[3; 32], Some(&[6; 32]), &[4; 32], &[5; 32])
        );
        assert_ne!(original, first.digest(&[3; 32], None, &[4; 32], &[6; 32]));
    }
}
