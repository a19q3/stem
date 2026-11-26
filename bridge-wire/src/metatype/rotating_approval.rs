//! Detached quorum approval against an authenticated CKB registry generation.

use super::{Hash, witness::GroupWitness};
use crate::hash;

pub const ACTION: u16 = 1;
pub const MAX_KEYS: usize = 8;
pub const MAX_PROOF_BYTES: usize = 1 + 64 * MAX_KEYS;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Authority<'a> {
    pub generation: u64,
    pub frozen: bool,
    keys: &'a [Hash],
    threshold: u8,
    record: &'a [u8],
}

impl<'a> Authority<'a> {
    pub const MAX_RECORD_BYTES: usize = 10 + 32 * MAX_KEYS;

    pub fn decode(record: &'a [u8]) -> Option<Self> {
        let (generation, raw) = record.split_first_chunk::<8>()?;
        let (&threshold, raw) = raw.split_first()?;
        let (&mode, raw) = raw.split_last()?;
        let frozen = match mode {
            0 => false,
            1 => true,
            _ => return None,
        };
        let (keys, extra) = raw.as_chunks::<32>();
        if !extra.is_empty()
            || !(1..=MAX_KEYS).contains(&keys.len())
            || !(1..=keys.len()).contains(&usize::from(threshold))
            || keys.contains(&[0; 32])
            || !keys.windows(2).all(|pair| pair[0] < pair[1])
        {
            return None;
        }
        Some(Self {
            generation: u64::from_le_bytes(*generation),
            frozen,
            keys,
            threshold,
            record,
        })
    }

    pub fn keys(&self) -> &'a [Hash] {
        self.keys
    }

    pub fn threshold(&self) -> u8 {
        self.threshold
    }

    pub fn is_successor(&self, previous: &Self) -> bool {
        previous.generation.checked_add(1) == Some(self.generation)
            && (previous.keys != self.keys
                || previous.threshold != self.threshold
                || previous.frozen != self.frozen)
    }

    pub fn digest(
        &self,
        network: &Hash,
        deployment: Option<&Hash>,
        rule: &Hash,
        transaction: &Hash,
    ) -> Hash {
        hash(
            b"Stem Metatype quorum approval\0",
            &[
                network,
                deployment.unwrap_or(&[0; 32]),
                rule,
                self.record,
                transaction,
            ],
        )
    }

    /// Exactly the threshold number of distinct slots sign, in roster order.
    pub fn authorize(
        &self,
        proof: &[u8],
        mut verify: impl FnMut(&Hash, &[u8; 64]) -> bool,
    ) -> bool {
        let Some((&bitmap, raw)) = proof.split_first() else {
            return false;
        };
        let valid = u8::MAX >> (MAX_KEYS - self.keys.len());
        if self.frozen
            || bitmap & !valid != 0
            || bitmap.count_ones() != u32::from(self.threshold)
            || raw.len() != 64 * usize::from(self.threshold)
        {
            return false;
        }
        self.keys
            .iter()
            .enumerate()
            .filter(|(index, _)| bitmap & (1 << index) != 0)
            .zip(raw.as_chunks::<64>().0)
            .all(|((_, key), signature)| verify(key, signature))
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

pub fn witness_proof(row: GroupWitness<'_>) -> Option<&[u8]> {
    (row.action == ACTION && row.message.is_empty() && row.seal.is_empty()).then_some(row.proof)
}

#[cfg(test)]
mod tests {
    use super::super::Vec;
    use super::*;

    fn record(generation: u64, threshold: u8, keys: &[u8], frozen: bool) -> Vec<u8> {
        let mut record = generation.to_le_bytes().to_vec();
        record.push(threshold);
        for key in keys {
            record.extend_from_slice(&[*key; 32]);
        }
        record.push(u8::from(frozen));
        record
    }

    #[test]
    fn authority_updates_bind_generation_roster_threshold_and_context() {
        let initial = record(0, 2, &[1, 2, 3], false);
        let first = Authority::decode(&initial).unwrap();
        assert_eq!(first.threshold(), 2);
        assert_eq!(first.keys(), &[[1; 32], [2; 32], [3; 32]]);
        let second = record(1, 3, &[1, 2, 3], false);
        let second = Authority::decode(&second).unwrap();
        assert!(second.is_successor(&first));
        assert!(!first.is_successor(&second));
        for bytes in [
            record(1, 2, &[1, 2, 3], false),
            record(2, 2, &[2, 3, 4], false),
        ] {
            assert!(!Authority::decode(&bytes).unwrap().is_successor(&first));
        }
        assert!(
            !first
                .is_successor(&Authority::decode(&record(u64::MAX, 2, &[2, 3, 4], false)).unwrap())
        );
        let original = first.digest(&[3; 32], None, &[4; 32], &[5; 32]);
        let changed = record(0, 1, &[1, 2, 3], false);
        let different_roster = record(0, 2, &[1, 2, 4], false);
        for bytes in [&changed, &different_roster] {
            assert_ne!(
                original,
                Authority::decode(bytes)
                    .unwrap()
                    .digest(&[3; 32], None, &[4; 32], &[5; 32],)
            );
        }
        assert_ne!(original, second.digest(&[3; 32], None, &[4; 32], &[5; 32]));
        assert_ne!(original, first.digest(&[7; 32], None, &[4; 32], &[5; 32]));
        assert_ne!(
            original,
            first.digest(&[3; 32], Some(&[6; 32]), &[4; 32], &[5; 32])
        );
        assert_ne!(original, first.digest(&[3; 32], None, &[9; 32], &[5; 32]));
        assert_ne!(original, first.digest(&[3; 32], None, &[4; 32], &[6; 32]));
    }

    #[test]
    fn verifies_exact_distinct_signers_and_rejects_noncanonical_authorities() {
        let active = record(0, 2, &[1, 2, 3], false);
        let active = Authority::decode(&active).unwrap();
        let mut proof = [0u8; 129];
        proof[0] = 0b101;
        proof[1..65].fill(1);
        proof[65..].fill(3);
        let verify = |key: &Hash, seal: &[u8; 64]| seal.iter().all(|byte| byte == &key[0]);
        assert!(active.authorize(&proof, verify));
        assert!(!active.authorize(&proof[..65], verify));
        proof[0] = 0b10001;
        assert!(!active.authorize(&proof, verify));
        proof[0] = 0b101;
        proof[65..].fill(1);
        assert!(!active.authorize(&proof, verify));
        proof[65..].fill(3);
        let frozen = record(1, 2, &[1, 2, 3], true);
        let frozen = Authority::decode(&frozen).unwrap();
        assert!(frozen.is_successor(&active));
        assert!(!frozen.authorize(&proof, verify));
        let thawed = record(2, 2, &[1, 2, 3], false);
        let thawed = Authority::decode(&thawed).unwrap();
        assert!(thawed.is_successor(&frozen));
        assert!(thawed.authorize(&proof, verify));
        for data in [
            record(0, 0, &[1, 2], false),
            record(0, 3, &[1, 2], false),
            record(0, 2, &[2, 1], false),
            record(0, 2, &[1, 1], false),
            record(0, 1, &[0], false),
            record(0, 1, &[1, 2, 3, 4, 5, 6, 7, 8, 9], false),
        ] {
            assert!(Authority::decode(&data).is_none());
        }
        let largest = record(0, 8, &[1, 2, 3, 4, 5, 6, 7, 8], false);
        let largest = Authority::decode(&largest).unwrap();
        let mut all = [255; MAX_PROOF_BYTES];
        for (index, seal) in all[1..].chunks_exact_mut(64).enumerate() {
            seal.fill(index as u8 + 1);
        }
        assert!(largest.authorize(&all, verify));
    }
}
