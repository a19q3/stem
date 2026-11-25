use anyhow::{Context, Result, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::*};
use stem_stake::Authority;

use super::{
    BlockBudget, BridgeMint, L1Anchor, Operation, VaultUpdate, byte32, hash32, number, uint64,
};
use crate::{Transaction, hash, wire};

pub(super) const MAX_DEPOSITS: usize = 128;

/// Authenticated increase in liquid backing at a specific CKB vault transition.
/// Guardians verify every deposit in the committed CKB transaction and sign the
/// exact Stem outputs, origins and amounts through the native transaction ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MintObservation {
    pub authority: Byte32,
    pub previous_anchor: L1Anchor,
    pub anchor: L1Anchor,
    pub vault: VaultUpdate,
    pub capacity: u64,
    pub units: u128,
}

impl MintObservation {
    pub fn encode(&self) -> Bytes {
        wire::MintObservation::new_builder()
            .authority(byte32(&self.authority))
            .previous_anchor(self.previous_anchor.encode())
            .anchor(self.anchor.encode())
            .vault(self.vault.encode())
            .capacity(uint64(self.capacity))
            .units(wire::Uint128::new_unchecked(Bytes::copy_from_slice(
                &self.units.to_le_bytes(),
            )))
            .build()
            .as_bytes()
    }

    pub(super) fn decode(bytes: &[u8]) -> Result<Self> {
        let encoded = wire::MintObservation::from_slice(bytes)?;
        let observation = Self {
            authority: hash32(encoded.authority()),
            previous_anchor: L1Anchor::decode(encoded.previous_anchor()),
            anchor: L1Anchor::decode(encoded.anchor()),
            vault: VaultUpdate::decode(encoded.vault()),
            capacity: number(encoded.capacity()),
            units: u128::from_le_bytes(encoded.units().as_slice().try_into()?),
        };
        ensure!(
            observation.authority != Byte32::default()
                && observation.anchor.hash != Byte32::default()
                && observation.previous_anchor.hash != Byte32::default()
                && observation.anchor.height > observation.previous_anchor.height
                && observation.anchor.hash != observation.previous_anchor.hash
                && observation.vault.lane != Byte32::default()
                && !observation.vault.input.is_null()
                && !observation.vault.output.is_null()
                && observation.vault.input.tx_hash() != observation.vault.output.tx_hash()
                && observation.capacity != 0,
            "invalid CKB mint observation"
        );
        Ok(observation)
    }
}

pub fn mint_digest(
    network: &Byte32,
    transaction_id: &Byte32,
    observation: &MintObservation,
) -> Byte32 {
    hash(
        b"Stem native Bridge Mint\0",
        network,
        &[transaction_id.as_slice(), &observation.encode()],
    )
}

/// An in-memory grant bound to the complete authenticated transaction.
/// The block transition still checks reserve and deposit replay state.
#[derive(Debug)]
pub struct VerifiedMint {
    statement: BridgeMint,
    observation: MintObservation,
    transaction: Byte32,
}

impl VerifiedMint {
    pub fn verify(
        tx: &Transaction,
        network: &Byte32,
        l1_genesis: &Byte32,
        anchor: &L1Anchor,
        authority: &Authority,
        budget: &mut BlockBudget,
    ) -> Result<Self> {
        let Some(Operation::BridgeMint(statement)) = tx.operation() else {
            anyhow::bail!("expected Bridge Mint operation");
        };
        ensure!(
            *network != Byte32::default() && statement.l1_genesis == *l1_genesis,
            "Bridge Mint names a different network or L1 genesis"
        );
        budget.charge(tx)?;
        let certificate = tx
            .certificate()
            .context("missing Bridge Mint certificate")?;
        let observation = MintObservation::decode(&certificate.body())?;
        ensure!(
            observation.previous_anchor == *anchor
                && observation.authority.as_slice() == authority.generation(),
            "Bridge Mint uses a stale CKB anchor or authority"
        );
        authority.verify(
            mint_digest(network, &tx.id(network), &observation).as_slice(),
            &certificate.attestations().collect::<Vec<_>>(),
        )?;
        Ok(Self {
            statement,
            observation,
            transaction: tx.commitment(network),
        })
    }

    pub fn statement(&self) -> &BridgeMint {
        &self.statement
    }

    pub fn observation(&self) -> &MintObservation {
        &self.observation
    }

    pub(crate) fn capacity(&self, network: &Byte32, tx: &Transaction) -> Result<u64> {
        ensure!(
            tx.commitment(network) == self.transaction,
            "incorrect Bridge Mint grant"
        );
        Ok(self.observation.capacity)
    }
}
