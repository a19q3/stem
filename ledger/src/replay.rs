//! Permanent replay protection. The caller keeps these records in SystemRoot.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_types::{
    packed::{Byte32, OutPoint},
    prelude::*,
};

use crate::hash;

#[derive(Clone, Debug)]
pub enum Key {
    Deposit {
        l1_genesis: Byte32,
        deposit: OutPoint,
    },
    Retirement {
        registry: Byte32,
        validator: Byte32,
        generation: u64,
    },
}

impl Key {
    pub fn commitment(&self, network_id: &Byte32) -> Byte32 {
        match self {
            Self::Deposit {
                l1_genesis,
                deposit,
            } => hash(
                b"Fern consumed CKB deposit\0",
                network_id,
                &[l1_genesis.as_slice(), deposit.as_slice()],
            ),
            Self::Retirement {
                registry,
                validator,
                generation,
            } => hash(
                b"Fern retired stake generation\0",
                network_id,
                &[
                    registry.as_slice(),
                    validator.as_slice(),
                    &generation.to_le_bytes(),
                ],
            ),
        }
    }
}

pub const LOGICAL_RECORD_BYTES: u64 = 32 + 8;

pub fn logical_bytes(records: u64) -> Result<u64> {
    records
        .checked_mul(LOGICAL_RECORD_BYTES)
        .context("permanent replay storage accounting overflow")
}

/// Staged permanent records. Keep the cumulative count and all returned writes
/// in the same SystemRoot batch as the operations that consumed their keys.
#[derive(Clone, Debug)]
pub struct InsertionBudget {
    deposits_remaining: u32,
    retirements_remaining: u32,
    total: u64,
    writes: BTreeMap<Byte32, [u8; 8]>,
}

impl InsertionBudget {
    pub fn new(deposits: u32, retirements: u32, committed_records: u64) -> Result<Self> {
        logical_bytes(committed_records)?;
        Ok(Self {
            deposits_remaining: deposits,
            retirements_remaining: retirements,
            total: committed_records,
            writes: BTreeMap::new(),
        })
    }

    /// Reserve a key after verifying its bridge or retirement certificate.
    /// Committed reads are by key, and staging also rejects in-block duplicates.
    pub fn reserve(
        &mut self,
        network: &Byte32,
        key: &Key,
        value: u64,
        exists: impl FnOnce(&Byte32) -> Result<bool>,
    ) -> Result<()> {
        ensure!(*network != Byte32::default(), "zero Fern network identity");
        let id = key.commitment(network);
        ensure!(
            !self.writes.contains_key(&id) && !exists(&id)?,
            "protocol record already applied"
        );
        let remaining = match key {
            Key::Deposit { .. } => &mut self.deposits_remaining,
            Key::Retirement { .. } => &mut self.retirements_remaining,
        };
        let available = remaining
            .checked_sub(1)
            .context("permanent replay insertion limit exceeded")?;
        let total = self
            .total
            .checked_add(1)
            .context("permanent replay count overflow")?;
        logical_bytes(total)?;

        *remaining = available;
        self.total = total;
        self.writes.insert(id, value.to_le_bytes());
        Ok(())
    }

    pub fn into_writes(self) -> (u64, BTreeMap<Byte32, [u8; 8]>) {
        (self.total, self.writes)
    }
}
