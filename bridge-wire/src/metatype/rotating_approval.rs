//! A detached approval against an authenticated CKB key generation.

use super::{Hash, witness::GroupWitness};
use crate::hash;

pub const ACTION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Authority {
    pub generation: u64,
    pub public_key: Hash,
    pub frozen: bool,
}

impl Authority {
    pub const SIZE: usize = 41;

    pub fn decode(raw: &[u8]) -> Option<Self> {
        let (generation, raw) = raw.split_first_chunk::<8>()?;
        let (key, mode) = raw.split_first_chunk::<32>()?;
        let frozen = match mode {
            [0] => false,
            [1] => true,
            _ => return None,
        };
        (*key != [0; 32]).then_some(Self {
            generation: u64::from_le_bytes(*generation),
            public_key: *key,
            frozen,
        })
    }

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut record = [0; Self::SIZE];
        record[..8].copy_from_slice(&self.generation.to_le_bytes());
        record[8..40].copy_from_slice(&self.public_key);
        record[40] = u8::from(self.frozen);
        record
    }

    pub fn is_successor(&self, previous: &Self) -> bool {
        previous.generation.checked_add(1) == Some(self.generation)
            && (previous.public_key != self.public_key || previous.frozen != self.frozen)
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
                &self.encode(),
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
        let first = Authority {
            generation: 0,
            public_key: [1; 32],
            frozen: false,
        };
        let second = Authority {
            generation: 1,
            public_key: [2; 32],
            frozen: false,
        };
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

    #[test]
    fn freezing_and_thawing_keep_the_key_and_advance_the_bound_record() {
        let active = Authority {
            generation: 1,
            public_key: [3; 32],
            frozen: false,
        };
        let frozen = Authority {
            generation: 2,
            frozen: true,
            ..active
        };
        let thawed = Authority {
            generation: 3,
            ..active
        };
        assert!(frozen.is_successor(&active));
        assert!(thawed.is_successor(&frozen));
        assert!(!thawed.is_successor(&active));
        for authority in [active, frozen, thawed] {
            assert_eq!(Authority::decode(&authority.encode()), Some(authority));
        }
        let mut malformed = active.encode();
        malformed[40] = 2;
        assert!(Authority::decode(&malformed).is_none());
        assert!(Authority::decode(&active.encode()[..40]).is_none());
        let digest = |authority: Authority| authority.digest(&[4; 32], None, &[5; 32], &[6; 32]);
        assert_ne!(digest(active), digest(thawed));
        assert_ne!(
            digest(frozen),
            digest(Authority {
                frozen: false,
                ..frozen
            })
        );
    }
}
