//! Canonical epoch-boundary record keys.

use alloc::vec::Vec;

#[cfg(feature = "ckb-settlement")]
pub use crate::settlement::ckb::evidence::{Equivocation, verify};

pub fn epoch_end_key(epoch: u64) -> Vec<u8> {
    [b"stake/epoch-end/".as_slice(), &epoch.to_le_bytes()].concat()
}
