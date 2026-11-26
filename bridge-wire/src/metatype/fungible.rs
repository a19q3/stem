//! Rules shared by a cap-bound native UDT and its portable adapters.

extern crate alloc;

use alloc::vec::Vec;

use super::{Hash, LogicalScript, custody::positive_amount};

pub const MAX_ASSETS: usize = 8;
pub const MAX_DATA: usize = 1 + 48 * MAX_ASSETS;

/// Distinct, canonical asset identities with independently measured amounts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Amounts(Vec<(Hash, u128)>);

impl Amounts {
    pub fn decode(data: &[u8]) -> Option<Self> {
        let (&count, data) = data.split_first()?;
        let (fields, tail) = data.as_chunks::<48>();
        if !tail.is_empty() || fields.len() != usize::from(count) {
            return None;
        }
        Self::from_sorted(fields.iter().map(|field| {
            let (id, amount) = field.split_first_chunk::<32>().expect("48 bytes");
            (*id, amount)
        }))
    }

    pub(super) fn from_states<'a>(states: impl Iterator<Item = (Hash, &'a [u8])>) -> Option<Self> {
        let mut entries = states.take(MAX_ASSETS + 1).collect::<Vec<_>>();
        entries.sort_unstable_by_key(|(id, _)| *id);
        Self::from_sorted(entries.into_iter())
    }

    fn from_sorted<'a>(entries: impl Iterator<Item = (Hash, &'a [u8])>) -> Option<Self> {
        let mut amounts = Vec::new();
        for (id, raw) in entries {
            if id == [0; 32]
                || amounts.len() == MAX_ASSETS
                || amounts.last().is_some_and(|(last, _)| *last >= id)
            {
                return None;
            }
            amounts.push((id, crate::amount(raw)?));
        }
        (amounts.len() >= 2 && amounts.iter().any(|(_, amount)| *amount != 0))
            .then_some(Self(amounts))
    }

    pub fn ids(&self) -> impl Iterator<Item = &Hash> {
        self.0.iter().map(|(id, _)| id)
    }

    pub fn amount(&self, asset: &Hash) -> Option<u128> {
        let index = self.0.binary_search_by_key(asset, |(id, _)| *id).ok()?;
        Some(self.0[index].1)
    }

    pub fn layout_id(&self) -> Hash {
        let mut hasher = ckb_hash::new_blake2b();
        hasher.update(b"Stem Metatype amount keys\0");
        for id in self.ids() {
            hasher.update(id);
        }
        let mut digest = [0; 32];
        hasher.finalize(&mut digest);
        digest
    }

    pub fn zeroed(&self) -> Self {
        Self(self.ids().map(|id| (*id, 0)).collect())
    }

    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|(_, quantity)| *quantity == 0)
    }

    pub fn checked_add(&self, other: &Self) -> Option<Self> {
        self.combine(other, u128::checked_add)
    }

    pub fn decrease(&self, other: &Self) -> Option<Self> {
        let delta = self.combine(other, u128::checked_sub)?;
        (!delta.is_zero()).then_some(delta)
    }

    fn combine(&self, other: &Self, op: impl Fn(u128, u128) -> Option<u128>) -> Option<Self> {
        if self.ids().ne(other.ids()) {
            return None;
        }
        self.0
            .iter()
            .zip(&other.0)
            .map(|((id, a), (_, b))| Some((*id, op(*a, *b)?)))
            .collect::<Option<Vec<_>>>()
            .map(Self)
    }
}

pub struct Udt<'a> {
    pub issuer: &'a Hash,
    pub policy: Option<Vec<u8>>,
}

impl<'a> Udt<'a> {
    pub fn decode(args: &'a [u8], asset: &Hash) -> Option<Self> {
        let (issuer, rest) = args.split_first_chunk::<32>()?;
        let (series, tail) = rest.split_first_chunk::<32>()?;
        if *issuer == [0; 32] || *series == [0; 32] {
            return None;
        }
        let policy = if tail.is_empty() {
            None
        } else {
            let (code, max) = tail.split_first_chunk::<32>()?;
            positive_amount(max)?;
            let mut args = [0; 48];
            args[..32].copy_from_slice(asset);
            args[32..].copy_from_slice(max);
            Some(LogicalScript::encode(code, 0, &args)?)
        };
        Some(Self { issuer, policy })
    }
}

pub struct AmountCap<'a> {
    pub asset: &'a Hash,
    pub ceiling: u128,
}

impl<'a> AmountCap<'a> {
    pub fn decode(args: &'a [u8]) -> Option<Self> {
        let (asset, limit) = args.split_first_chunk::<32>()?;
        (*asset != [0; 32]).then_some(Self {
            asset,
            ceiling: positive_amount(limit)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(first: (u8, u128), second: (u8, u128)) -> Vec<u8> {
        let mut wire = alloc::vec![2];
        for (id, quantity) in [first, second] {
            wire.extend_from_slice(&[id; 32]);
            wire.extend_from_slice(&quantity.to_le_bytes());
        }
        wire
    }

    #[test]
    fn amounts_bind_keys_and_check_independent_deltas() {
        let before = Amounts::decode(&pair((1, 125), (2, 80))).unwrap();
        let after = Amounts::decode(&pair((1, 90), (2, 35))).unwrap();
        let withdrawn = Amounts::decode(&pair((1, 35), (2, 45))).unwrap();
        assert_eq!(before.decrease(&after), Some(withdrawn.clone()));
        assert_eq!(after.checked_add(&withdrawn), Some(before.clone()));
        assert_eq!(before.layout_id(), withdrawn.layout_id());
        let different = Amounts::decode(&pair((1, 90), (3, 35))).unwrap();
        assert!(before.decrease(&different).is_none());
        let selective = Amounts::decode(&pair((1, 0), (2, 45))).unwrap();
        let selective_remainder = Amounts::decode(&pair((1, 125), (2, 35))).unwrap();
        assert_eq!(before.decrease(&selective_remainder), Some(selective));
        assert!(before.decrease(&before).is_none());
        assert!(selective_remainder.decrease(&before).is_none());
        let max = Amounts::decode(&pair((1, u128::MAX), (2, 1))).unwrap();
        assert!(before.checked_add(&max).is_none());
        for malformed in [
            pair((2, 80), (1, 125)),
            pair((1, 125), (1, 80)),
            pair((1, 0), (2, 0)),
        ] {
            assert!(Amounts::decode(&malformed).is_none());
        }
    }
}
