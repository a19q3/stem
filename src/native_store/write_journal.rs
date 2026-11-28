//! In-memory keys and lookup positions for a prepared block's write journal.

use std::hash::{BuildHasher, RandomState};

use hashbrown::{HashTable, hash_table::Entry};

use super::Write;

// Covers the two-byte namespace and 32-byte content ID of state rows.
const INLINE_KEY_BYTES: usize = 34;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum JournalKey {
    Inline {
        bytes: [u8; INLINE_KEY_BYTES],
        len: u8,
    },
    Owned(Vec<u8>),
}

impl From<&[u8]> for JournalKey {
    fn from(key: &[u8]) -> Self {
        if key.len() > INLINE_KEY_BYTES {
            return Self::Owned(key.to_vec());
        }
        let mut bytes = [0; INLINE_KEY_BYTES];
        bytes[..key.len()].copy_from_slice(key);
        Self::Inline {
            bytes,
            len: key.len() as u8,
        }
    }
}

impl AsRef<[u8]> for JournalKey {
    fn as_ref(&self) -> &[u8] {
        match self {
            Self::Inline { bytes, len } => &bytes[..usize::from(*len)],
            Self::Owned(bytes) => bytes,
        }
    }
}

pub(super) struct JournalIndex {
    hasher: RandomState,
    positions: HashTable<usize>,
}

impl JournalIndex {
    pub(super) fn build(writes: &[Write]) -> Self {
        let hasher = RandomState::new();
        let mut positions = HashTable::<usize>::with_capacity(writes.len());
        for (position, (key, _)) in writes.iter().enumerate() {
            let key = key.as_ref();
            match positions.entry(
                hasher.hash_one(key),
                |&index| writes[index].0.as_ref() == key,
                |&index| hasher.hash_one(writes[index].0.as_ref()),
            ) {
                Entry::Occupied(mut entry) => *entry.get_mut() = position,
                Entry::Vacant(entry) => {
                    entry.insert(position);
                }
            }
        }
        Self { hasher, positions }
    }

    pub(super) fn position(&self, writes: &[Write], key: &[u8]) -> Option<usize> {
        self.positions
            .find(self.hasher.hash_one(key), |&index| {
                writes[index].0.as_ref() == key
            })
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::native_store::Batch;

    #[test]
    fn indexed_journal_agrees_with_replaying_puts_and_tombstones() {
        let mut batch = Batch::default();
        let mut latest = BTreeMap::new();
        let mut ordered = Vec::new();
        for length in [0, 1, 2, INLINE_KEY_BYTES, INLINE_KEY_BYTES + 1, 64] {
            for prefix in [0, 1, 255] {
                let key = vec![prefix; length];
                for value in [Some(vec![1]), None, Some(vec![2])] {
                    match &value {
                        Some(bytes) => batch.put(&key, bytes),
                        None => batch.delete(&key),
                    }
                    latest.insert(key.clone(), value.clone());
                    ordered.push((key.clone(), value));
                }
            }
        }
        let deleted = [255; INLINE_KEY_BYTES];
        batch.delete(deleted);
        latest.insert(deleted.to_vec(), None);
        ordered.push((deleted.to_vec(), None));

        for ((key, value), (expected_key, expected_value)) in batch.writes.iter().zip(&ordered) {
            assert_eq!(key.as_ref(), expected_key);
            assert_eq!(value, expected_value);
            assert_eq!(
                matches!(key, JournalKey::Inline { .. }),
                expected_key.len() <= INLINE_KEY_BYTES
            );
        }
        assert_eq!(batch.writes.len(), ordered.len());
        for (key, expected) in &latest {
            assert_eq!(batch.lookup(key), Some(expected));
        }
        assert!(matches!(batch.lookup(&deleted), Some(None)));
        assert!(batch.lookup(&[255; INLINE_KEY_BYTES - 1]).is_none());
    }
}
