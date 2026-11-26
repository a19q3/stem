//! Canonical group witness records carried by the first extra CKB witness.

use super::{Hash, MAX_GROUPS, VirtualTypeGroup, take_bytes};
use crate::hash;
extern crate alloc;

use alloc::vec::Vec;

pub const MAGIC: &[u8] = b"Stem Metatype Witness\0";
pub const MAX_WITNESS_BYTES: usize = 32 * 1024;
pub const MAX_MESSAGE_BYTES: usize = 512;
pub const MAX_PROOF_BYTES: usize = 1024;
pub const MAX_SEAL_BYTES: usize = 128;
pub(super) const MAX_FIELDS_BYTES: usize = 8 + MAX_MESSAGE_BYTES + MAX_PROOF_BYTES + MAX_SEAL_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupWitness<'a> {
    pub action: u16,
    pub message: &'a [u8],
    pub proof: &'a [u8],
    pub seal: &'a [u8],
}

impl<'a> GroupWitness<'a> {
    fn valid(&self) -> bool {
        self.action != 0
            && self.message.len() <= MAX_MESSAGE_BYTES
            && self.proof.len() <= MAX_PROOF_BYTES
            && self.seal.len() <= MAX_SEAL_BYTES
    }

    pub(super) fn write_into(&self, encoded: &mut Vec<u8>) {
        encoded.extend_from_slice(&self.action.to_le_bytes());
        for bytes in [self.message, self.proof, self.seal] {
            encoded.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            encoded.extend_from_slice(bytes);
        }
    }

    pub(super) fn take(raw: &mut &'a [u8]) -> Option<Self> {
        let (action, tail) = raw.split_first_chunk::<2>()?;
        *raw = tail;
        let witness = Self {
            action: u16::from_le_bytes(*action),
            message: take_bytes(raw)?,
            proof: take_bytes(raw)?,
            seal: take_bytes(raw)?,
        };
        witness.valid().then_some(witness)
    }

    /// The CKB raw transaction commits actual input OutPoints, deps and output
    /// bytes. The group and full deployment identity scope this detached seal.
    pub fn approval_digest(&self, deployment: &Hash, group: &Hash, tx_hash: &Hash) -> Hash {
        hash(
            b"Stem Metatype approval\0",
            &[
                deployment,
                group,
                tx_hash,
                &self.action.to_le_bytes(),
                &(self.message.len() as u32).to_le_bytes(),
                self.message,
                &(self.proof.len() as u32).to_le_bytes(),
                self.proof,
            ],
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GroupWitnessEntry<'a> {
    pub deployment: Hash,
    pub group: Hash,
    pub witness: GroupWitness<'a>,
}

impl GroupWitnessEntry<'_> {
    fn key(&self) -> (Hash, Hash) {
        (self.deployment, self.group)
    }

    fn valid(&self) -> bool {
        self.deployment != [0; 32] && self.group != [0; 32] && self.witness.valid()
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
pub struct GroupWitnesses<'a> {
    entries: Vec<GroupWitnessEntry<'a>>,
}

impl<'a> GroupWitnesses<'a> {
    pub fn decode(raw: &'a [u8]) -> Option<Self> {
        if raw.len() > MAX_WITNESS_BYTES {
            return None;
        }
        let (&count, mut raw) = raw.strip_prefix(MAGIC)?.split_first()?;
        let count = usize::from(count);
        if !(1..=MAX_GROUPS).contains(&count) {
            return None;
        }
        let mut entries: Vec<GroupWitnessEntry<'_>> = Vec::with_capacity(count);
        for _ in 0..count {
            let (deployment, tail) = raw.split_first_chunk::<32>()?;
            let (group, tail) = tail.split_first_chunk::<32>()?;
            raw = tail;
            let entry = GroupWitnessEntry {
                deployment: *deployment,
                group: *group,
                witness: GroupWitness::take(&mut raw)?,
            };
            if !entry.valid() || entries.last().is_some_and(|old| old.key() >= entry.key()) {
                return None;
            }
            entries.push(entry);
        }
        raw.is_empty().then_some(Self { entries })
    }

    /// Sorts entries by complete deployment and group hashes, and rejects duplicates.
    pub fn encode(mut entries: Vec<GroupWitnessEntry<'_>>) -> Option<Vec<u8>> {
        if !(1..=MAX_GROUPS).contains(&entries.len()) {
            return None;
        }
        entries.sort_unstable_by_key(GroupWitnessEntry::key);
        if !entries.windows(2).all(|pair| pair[0].key() < pair[1].key()) {
            return None;
        }
        let mut raw = MAGIC.to_vec();
        raw.push(entries.len() as u8);
        for entry in entries {
            if !entry.valid() {
                return None;
            }
            raw.extend_from_slice(&entry.deployment);
            raw.extend_from_slice(&entry.group);
            entry.witness.write_into(&mut raw);
            if raw.len() > MAX_WITNESS_BYTES {
                return None;
            }
        }
        Some(raw)
    }

    pub fn for_group(&self, deployment: &Hash, group: Hash) -> Option<GroupWitness<'a>> {
        self.entries
            .binary_search_by_key(&(*deployment, group), GroupWitnessEntry::key)
            .ok()
            .map(|index| self.entries[index].witness)
    }

    pub fn validate_groups(&self, deployment: &Hash, groups: &[VirtualTypeGroup<'_>]) -> bool {
        self.entries
            .iter()
            .filter(|entry| &entry.deployment == deployment)
            .all(|entry| groups.iter().any(|group| group.script.id() == entry.group))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(deployment: u8, group: u8, seal: &[u8]) -> GroupWitnessEntry<'_> {
        GroupWitnessEntry {
            deployment: [deployment; 32],
            group: [group; 32],
            witness: GroupWitness {
                action: 2,
                message: b"owner",
                proof: b"proof",
                seal,
            },
        }
    }

    #[test]
    fn witness_envelope_distinguishes_deployments_and_rejects_noncanonical_rows() {
        let a = entry(1, 4, b"first");
        let b = entry(2, 4, b"second");
        let mut raw = GroupWitnesses::encode(alloc::vec![b, a]).unwrap();
        let decoded = GroupWitnesses::decode(&raw).unwrap();
        assert_eq!(decoded.for_group(&a.deployment, a.group), Some(a.witness));
        assert_eq!(decoded.for_group(&b.deployment, b.group), Some(b.witness));
        assert!(GroupWitnesses::encode(alloc::vec![a, a]).is_none());
        assert!(!decoded.validate_groups(&a.deployment, &[]));
        assert!(decoded.validate_groups(&[0; 32], &[]));
        raw.push(0);
        assert!(GroupWitnesses::decode(&raw).is_none());
        assert!(GroupWitnesses::decode(&raw[..raw.len() - 2]).is_none());
    }

    #[test]
    fn approval_scopes_every_nonseal_field() {
        let a = entry(1, 4, b"first");
        let tx = [5; 32];
        let digest = a.witness.approval_digest(&a.deployment, &a.group, &tx);
        for witness in [
            GroupWitness {
                action: 1,
                ..a.witness
            },
            GroupWitness {
                message: b"next",
                ..a.witness
            },
            GroupWitness {
                proof: b"next",
                ..a.witness
            },
        ] {
            assert_ne!(
                digest,
                witness.approval_digest(&a.deployment, &a.group, &tx)
            );
        }
        assert_ne!(digest, a.witness.approval_digest(&[9; 32], &a.group, &tx));
        assert_ne!(
            digest,
            a.witness.approval_digest(&a.deployment, &[9; 32], &tx)
        );
        assert_ne!(
            digest,
            a.witness.approval_digest(&a.deployment, &a.group, &[9; 32])
        );
        assert_eq!(
            digest,
            entry(1, 4, b"other")
                .witness
                .approval_digest(&a.deployment, &a.group, &tx)
        );
    }
}
