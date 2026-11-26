//! One-shot authorization of a specific CKB transaction's actual miner fee.
use crate::intercell::Identity;

pub const MAX_EXPENSE_INPUTS: usize = 64;
pub const MAX_GRANT_WITNESS: usize = 256 * 1024;

pub struct Grant {
    pub expense: Identity,
    pub payer: Identity,
    pub refund: Identity,
    pub max_expense_fee: u64,
    pub max_settlement_fee: u64,
}

impl Grant {
    pub const BYTES: usize = 113;

    pub fn decode(raw: &[u8]) -> Option<Self> {
        if raw.len() != Self::BYTES || raw[0] != 1 {
            return None;
        }
        let grant = Self {
            expense: raw[1..33].try_into().ok()?,
            payer: raw[33..65].try_into().ok()?,
            refund: raw[65..97].try_into().ok()?,
            max_expense_fee: u64::from_le_bytes(raw[97..105].try_into().ok()?),
            max_settlement_fee: u64::from_le_bytes(raw[105..].try_into().ok()?),
        };
        (grant.expense != [0; 32]
            && grant.payer != [0; 32]
            && grant.refund != [0; 32]
            && grant.refund != grant.payer
            && grant.max_expense_fee > 0)
            .then_some(grant)
    }

    pub fn encode(&self) -> [u8; Self::BYTES] {
        let mut raw = [0; Self::BYTES];
        raw[0] = 1;
        raw[1..33].copy_from_slice(&self.expense);
        raw[33..65].copy_from_slice(&self.payer);
        raw[65..97].copy_from_slice(&self.refund);
        raw[97..105].copy_from_slice(&self.max_expense_fee.to_le_bytes());
        raw[105..].copy_from_slice(&self.max_settlement_fee.to_le_bytes());
        raw
    }
}
