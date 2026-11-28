//! Compact resident records retain canonical data and share exact Script bytes.

use std::{
    hash::{Hash, RandomState},
    mem::size_of,
    sync::Arc,
};

use anyhow::Result;
use bytes::Bytes;
use hashbrown::{HashMap, hash_map::EntryRef};
use stem_ledger::{Cell, LiveCell, Origin};

use super::ResidentKey;

type Table<K, V> = HashMap<K, V, RandomState>;

#[derive(Clone)]
pub(super) struct Row {
    body: Bytes,
    prefix_len: usize,
    lock: Arc<[u8]>,
    types: Option<Arc<[u8]>>,
    origin: Origin,
}

impl Row {
    fn cost(body_len: usize) -> usize {
        // Key, origin and body allocations with their ownership metadata.
        body_len + super::ROW_PAYLOAD_OVERHEAD
    }

    #[cfg(feature = "native-live-cell-parent-index")]
    pub(super) fn retained_bytes(&self) -> usize {
        Self::cost(self.body.len())
            .saturating_add(Descriptors::cost(&self.lock))
            .saturating_add(self.types.as_ref().map_or(0, |types| Descriptors::cost(types)))
    }

    pub(super) fn into_cell(self) -> Result<LiveCell> {
        let (prefix, data) = self.body.split_at(self.prefix_len);
        let types = self.types.as_deref().unwrap_or_default();
        let parts = [prefix, self.lock.as_ref(), types, data];
        let mut encoded = Vec::with_capacity(parts.iter().map(|part| part.len()).sum());
        for part in parts {
            encoded.extend_from_slice(part);
        }
        Ok(LiveCell {
            cell: Cell::from_bytes(encoded.into())?,
            origin: self.origin,
        })
    }
}

#[derive(Default)]
struct Descriptors {
    values: Table<Arc<[u8]>, usize>,
    payload: usize,
}

impl Descriptors {
    fn cost(bytes: &[u8]) -> usize {
        bytes.len() + 64
    }

    fn retain(&mut self, bytes: &[u8]) -> Arc<[u8]> {
        match self.values.entry_ref(bytes) {
            EntryRef::Occupied(mut entry) => {
                *entry.get_mut() += 1;
                Arc::clone(entry.key())
            }
            EntryRef::Vacant(entry) => {
                self.payload += Self::cost(bytes);
                let owned = Arc::<[u8]>::from(bytes);
                entry.insert_entry_with_key(Arc::clone(&owned), 1);
                owned
            }
        }
    }

    fn release(&mut self, bytes: &[u8]) {
        let owners = self.values.get_mut(bytes).expect("resident Script owner");
        *owners -= 1;
        if *owners == 0 {
            self.payload -= Self::cost(bytes);
            self.values.remove(bytes);
        }
    }
}

#[derive(Default)]
pub(super) struct InternedRecords {
    rows: Table<ResidentKey, Row>,
    descriptors: Descriptors,
    payload: usize,
}

impl InternedRecords {
    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn distinct_descriptors(&self) -> usize {
        self.descriptors.values.len()
    }

    pub(super) fn bytes(&self) -> usize {
        self.payload
            .saturating_add(self.descriptors.payload)
            .saturating_add(self.rows.allocation_size())
            .saturating_add(self.descriptors.values.allocation_size())
    }

    pub(super) fn get(&self, point: &ResidentKey) -> Option<Row> {
        self.rows.get(point).cloned()
    }

    pub(super) fn remove(&mut self, point: &ResidentKey) -> Option<Row> {
        let row = self.rows.remove(point)?;
        self.payload -= Row::cost(row.body.len());
        self.descriptors.release(&row.lock);
        if let Some(types) = &row.types {
            self.descriptors.release(types);
        }
        Some(row)
    }

    pub(super) fn insert(&mut self, point: ResidentKey, cell: &LiveCell, limit: usize) -> bool {
        let [prefix, lock, types, data] = cell.cell.canonical_parts();
        let row_cost = Row::cost(prefix.len() + data.len());
        let mut descriptors = 0;
        let mut descriptor_cost = 0;
        if !self.descriptors.values.contains_key(lock) {
            descriptors += 1;
            descriptor_cost += Descriptors::cost(lock);
        }
        if !types.is_empty() && types != lock && !self.descriptors.values.contains_key(types) {
            descriptors += 1;
            descriptor_cost += Descriptors::cost(types);
        }
        let row_growth = peak(&self.rows, 1);
        let catalog_growth = peak(&self.descriptors.values, descriptors);
        if self
            .payload
            .saturating_add(row_cost)
            .saturating_add(self.descriptors.payload)
            .saturating_add(descriptor_cost)
            .saturating_add(row_growth)
            .saturating_add(catalog_growth)
            > limit
            || self.rows.try_reserve(1).is_err()
            || self.descriptors.values.try_reserve(descriptors).is_err()
            || self
                .bytes()
                .saturating_add(row_cost)
                .saturating_add(descriptor_cost)
                > limit
        {
            return false;
        }
        let mut body = Vec::with_capacity(prefix.len() + data.len());
        body.extend_from_slice(prefix);
        body.extend_from_slice(data);
        let row = Row {
            body: body.into(),
            prefix_len: prefix.len(),
            lock: self.descriptors.retain(lock),
            types: (!types.is_empty()).then(|| self.descriptors.retain(types)),
            origin: cell.origin.clone(),
        };
        let previous = self.rows.insert(point, row);
        debug_assert!(
            previous.is_none(),
            "resident record must be removed before insertion"
        );
        self.payload += row_cost;
        true
    }
}

fn peak<K: Eq + Hash, V>(table: &Table<K, V>, additional: usize) -> usize {
    let current = table.allocation_size();
    if table.len().saturating_add(additional) <= table.capacity() {
        return current;
    }
    // Count both tables during growth, even when a resize occurs in place.
    current
        .saturating_mul(3)
        .max(8 * (size_of::<(K, V)>() + 1) + 64)
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use ckb_types::{
        packed::{Byte32, OutPoint, Script},
        prelude::*,
    };

    use super::{super::own_resident_key, *};

    #[test]
    fn releases_unowned_descriptors_and_preserves_cloned_views() -> Result<()> {
        let lock = Script::new_builder().code_hash([1; 32]).build();
        let types = vec![
            Script::new_builder().code_hash([2; 32]).build(),
            Script::new_builder().code_hash([3; 32]).build(),
        ];
        let origin = Origin {
            height: 1,
            timestamp_ms: 2,
            context_id: Byte32::default(),
        };
        let make = |data| -> Result<LiveCell> {
            Ok(LiveCell {
                cell: Cell::new(1_000, lock.clone(), types.clone(), data)?,
                origin: origin.clone(),
            })
        };
        let first = || own_resident_key(Cow::Owned(OutPoint::new([4; 32].pack(), 0)));
        let second = || own_resident_key(Cow::Owned(OutPoint::new([4; 32].pack(), 256)));
        let first_cell = make(Bytes::new())?;
        let second_cell = make(Bytes::from_static(b"separate data"))?;
        let mut records = InternedRecords::default();
        assert!(records.insert(first(), &first_cell, 1 << 20));
        assert!(records.insert(second(), &second_cell, 1 << 20));
        assert_eq!(records.distinct_descriptors(), 2);
        let retained = records.get(&second()).expect("resident row");
        drop(records.remove(&first()));
        assert_eq!(records.distinct_descriptors(), 2);
        drop(records.remove(&second()));
        assert_eq!(records.distinct_descriptors(), 0);
        assert_eq!(records.descriptors.payload, 0);
        assert!(records.get(&second()).is_none());

        let decoded = retained.into_cell()?;
        assert_eq!(decoded.cell.as_slice(), second_cell.cell.as_slice());
        assert_eq!(decoded.origin, origin);
        assert!(!records.insert(first(), &first_cell, records.bytes()));
        assert_eq!(records.len(), 0);
        assert_eq!(records.distinct_descriptors(), 0);
        Ok(())
    }
}
