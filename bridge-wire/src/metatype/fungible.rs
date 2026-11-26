//! Rules shared by a cap-bound native UDT and its portable adapters.

extern crate alloc;

use alloc::vec::Vec;

use super::{Hash, LogicalScript, custody::positive_amount};

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

    pub fn permits(&self, data: &[u8]) -> bool {
        positive_amount(data).is_some_and(|value| value <= self.ceiling)
    }
}
