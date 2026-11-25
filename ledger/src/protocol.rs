//! Protocol operations share the native Transaction encoding and have separate block budgets.

use crate::strand::prelude::{Builder as WireBuilder, Entity as WireEntity};
use anyhow::{Context, Result, bail, ensure};
use ckb_types::{
    bytes::Bytes,
    packed::{Byte32, OutPoint},
    prelude::*,
};
use stem_stake::{Attestation, Authority, MAX_RECORDS};

use crate::{Transaction, hash, wire};

mod backing;
mod fees;
mod mint;
mod settlement;
mod snapshot;
pub use backing::BridgeBacking;
pub use fees::{FeeClaim, FeeFund, FeeFundDestination, FeeFundPolicy, FeePayout, FeePayouts};
pub use mint::{MintObservation, VerifiedMint, mint_digest};
pub use settlement::{BridgeLedger, Lane, PendingWithdrawal, SettlementChanges};
pub use snapshot::{
    ARRIVING_GUARDIAN, SnapshotContext, VerifiedSnapshot, snapshot_body, snapshot_commitment,
    snapshot_digest,
};
const MAX_ATTESTATIONS: usize = 2 * MAX_RECORDS;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeMint {
    pub l1_genesis: Byte32,
    pub deposits: Vec<OutPoint>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StakeSnapshot {
    pub epoch: u64,
    pub signing_anchor: Byte32,
    pub commitment: Byte32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VaultUpdate {
    pub lane: Byte32,
    pub input: OutPoint,
    pub output: OutPoint,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettlementAction {
    WithdrawalPaid {
        vault: VaultUpdate,
        nonce: u64,
        receipt: Byte32,
    },
    VaultRotated {
        vault: VaultUpdate,
    },
    ReserveMoved {
        source: VaultUpdate,
        destination: VaultUpdate,
        capacity: u64,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct L1Anchor {
    pub height: u64,
    pub hash: Byte32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BridgeSettlement {
    pub l1_genesis: Byte32,
    pub authority: Byte32,
    pub previous_anchor: L1Anchor,
    pub anchor: L1Anchor,
    pub action: SettlementAction,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Operation {
    BridgeMint(BridgeMint),
    StakeSnapshot(StakeSnapshot),
    BridgeSettlement(BridgeSettlement),
    FeeClaim(FeeClaim),
}

impl Operation {
    pub(crate) fn encode(&self) -> wire::ProtocolOperation {
        let (kind, payload): (u8, Bytes) = match self {
            Self::BridgeMint(mint) => (
                0,
                wire::BridgeMint::new_builder()
                    .l1_genesis(byte32(&mint.l1_genesis))
                    .deposits(
                        wire::OutPointVec::new_builder()
                            .set(mint.deposits.iter().map(out_point).collect())
                            .build(),
                    )
                    .build()
                    .as_bytes(),
            ),
            Self::StakeSnapshot(snapshot) => (
                1,
                wire::StakeSnapshot::new_builder()
                    .epoch(uint64(snapshot.epoch))
                    .signing_anchor(byte32(&snapshot.signing_anchor))
                    .commitment(byte32(&snapshot.commitment))
                    .build()
                    .as_bytes(),
            ),
            Self::BridgeSettlement(settlement) => {
                let (kind, action) = match &settlement.action {
                    SettlementAction::WithdrawalPaid {
                        vault,
                        nonce,
                        receipt,
                    } => (
                        2,
                        wire::WithdrawalPaid::new_builder()
                            .vault(vault.encode())
                            .nonce(uint64(*nonce))
                            .receipt(byte32(receipt))
                            .build()
                            .as_bytes(),
                    ),
                    SettlementAction::VaultRotated { vault } => (4, vault.encode().as_bytes()),
                    SettlementAction::ReserveMoved {
                        source,
                        destination,
                        capacity,
                    } => (
                        3,
                        wire::ReserveMoved::new_builder()
                            .source(source.encode())
                            .destination(destination.encode())
                            .capacity(uint64(*capacity))
                            .build()
                            .as_bytes(),
                    ),
                };
                let payload = wire::BridgeSettlement::new_builder()
                    .l1_genesis(byte32(&settlement.l1_genesis))
                    .authority(byte32(&settlement.authority))
                    .previous_anchor(settlement.previous_anchor.encode())
                    .anchor(settlement.anchor.encode())
                    .action(bytes(action))
                    .build()
                    .as_bytes();
                (kind, payload)
            }
            Self::FeeClaim(claim) => (5, claim.encode().as_bytes()),
        };
        wire::ProtocolOperation::new_builder()
            .kind(kind.into())
            .payload(bytes(payload))
            .build()
    }

    pub(crate) fn decode(operation: wire::ProtocolOperation) -> Result<Self> {
        let payload = operation.payload().raw_data();
        match u8::from(operation.kind()) {
            0 => {
                let value = wire::BridgeMint::from_slice(&payload)?;
                Ok(Self::BridgeMint(BridgeMint {
                    l1_genesis: hash32(value.l1_genesis()),
                    deposits: value
                        .deposits()
                        .into_iter()
                        .map(|point| OutPoint::new_unchecked(point.as_bytes()))
                        .collect(),
                }))
            }
            1 => {
                let value = wire::StakeSnapshot::from_slice(&payload)?;
                Ok(Self::StakeSnapshot(StakeSnapshot {
                    epoch: number(value.epoch()),
                    signing_anchor: hash32(value.signing_anchor()),
                    commitment: hash32(value.commitment()),
                }))
            }
            kind @ (2..=4) => {
                let value = wire::BridgeSettlement::from_slice(&payload)?;
                let action = value.action().raw_data();
                let action = if kind == 2 {
                    let paid = wire::WithdrawalPaid::from_slice(&action)?;
                    SettlementAction::WithdrawalPaid {
                        vault: VaultUpdate::decode(paid.vault()),
                        nonce: number(paid.nonce()),
                        receipt: hash32(paid.receipt()),
                    }
                } else if kind == 4 {
                    SettlementAction::VaultRotated {
                        vault: VaultUpdate::decode(wire::VaultUpdate::from_slice(&action)?),
                    }
                } else {
                    let moved = wire::ReserveMoved::from_slice(&action)?;
                    SettlementAction::ReserveMoved {
                        source: VaultUpdate::decode(moved.source()),
                        destination: VaultUpdate::decode(moved.destination()),
                        capacity: number(moved.capacity()),
                    }
                };
                Ok(Self::BridgeSettlement(BridgeSettlement {
                    l1_genesis: hash32(value.l1_genesis()),
                    authority: hash32(value.authority()),
                    previous_anchor: L1Anchor::decode(value.previous_anchor()),
                    anchor: L1Anchor::decode(value.anchor()),
                    action,
                }))
            }
            5 => Ok(Self::FeeClaim(FeeClaim::decode(&payload)?)),
            _ => bail!("unknown protocol operation"),
        }
    }

    pub(crate) fn validate(&self, certificate: &Certificate) -> Result<()> {
        match self {
            Self::BridgeMint(mint) => ensure!(
                mint.l1_genesis != Byte32::default()
                    && !mint.deposits.is_empty()
                    && mint.deposits.len() <= mint::MAX_DEPOSITS
                    && mint.deposits.iter().all(|point| !point.is_null())
                    && mint
                        .deposits
                        .windows(2)
                        .all(|pair| pair[0].as_slice() < pair[1].as_slice())
                    && MintObservation::decode(&certificate.body()).is_ok(),
                "invalid Bridge Mint certificate"
            ),
            Self::StakeSnapshot(snapshot) => ensure!(
                snapshot.commitment != Byte32::default()
                    && snapshot.signing_anchor != Byte32::default()
                    && snapshot.epoch != 0
                    && !certificate.body().is_empty(),
                "invalid Stake Snapshot certificate"
            ),
            Self::BridgeSettlement(settlement) => {
                settlement.validate()?;
                ensure!(
                    certificate.body().is_empty(),
                    "settlement certificate has unexpected body"
                );
            }
            Self::FeeClaim(_) => bail!("fee claims do not use certificates"),
        }
        Ok(())
    }

    fn class(&self) -> Result<Class> {
        Ok(match self {
            Self::BridgeMint(_) => Class::Mint,
            Self::StakeSnapshot(_) => Class::StakeSnapshot,
            Self::BridgeSettlement(_) => Class::BridgeSettlement,
            Self::FeeClaim(_) => bail!("fee claims have a separate block allowance"),
        })
    }
}

impl BridgeSettlement {
    pub fn encode(&self) -> Bytes {
        Operation::BridgeSettlement(self.clone())
            .encode()
            .as_bytes()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let Operation::BridgeSettlement(statement) =
            Operation::decode(wire::ProtocolOperation::from_slice(bytes)?)?
        else {
            bail!("expected a Bridge Settlement statement");
        };
        statement.validate()?;
        Ok(statement)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            [
                &self.l1_genesis,
                &self.authority,
                &self.anchor.hash,
                &self.previous_anchor.hash
            ]
            .into_iter()
            .all(|value| *value != Byte32::default()),
            "invalid L1 settlement identity"
        );
        ensure!(
            self.anchor.height > self.previous_anchor.height
                && self.anchor.hash != self.previous_anchor.hash,
            "L1 settlement anchor must advance"
        );
        match &self.action {
            SettlementAction::WithdrawalPaid { vault, receipt, .. } => {
                vault.validate()?;
                ensure!(*receipt != Byte32::default(), "missing withdrawal receipt");
            }
            SettlementAction::VaultRotated { vault } => vault.validate()?,
            SettlementAction::ReserveMoved {
                source,
                destination,
                capacity,
            } => {
                source.validate()?;
                destination.validate()?;
                ensure!(
                    *capacity > 0
                        && source.lane != destination.lane
                        && source.input != destination.input
                        && source.output != destination.output
                        && source.output.tx_hash() == destination.output.tx_hash()
                        && source.output.tx_hash() != destination.input.tx_hash()
                        && destination.output.tx_hash() != source.input.tx_hash(),
                    "reserve transfer must atomically update two distinct vaults"
                );
            }
        }
        Ok(())
    }
}

impl VaultUpdate {
    fn encode(&self) -> wire::VaultUpdate {
        wire::VaultUpdate::new_builder()
            .lane(byte32(&self.lane))
            .input(out_point(&self.input))
            .output(out_point(&self.output))
            .build()
    }

    fn decode(vault: wire::VaultUpdate) -> Self {
        Self {
            lane: hash32(vault.lane()),
            input: OutPoint::new_unchecked(vault.input().as_bytes()),
            output: OutPoint::new_unchecked(vault.output().as_bytes()),
        }
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.lane != Byte32::default()
                && !self.input.is_null()
                && !self.output.is_null()
                && self.input.tx_hash() != Byte32::default()
                && self.output.tx_hash() != Byte32::default()
                && self.input.tx_hash() != self.output.tx_hash(),
            "invalid L1 vault lineage"
        );
        Ok(())
    }
}

impl L1Anchor {
    fn encode(&self) -> wire::L1Anchor {
        wire::L1Anchor::new_builder()
            .height(uint64(self.height))
            .hash(byte32(&self.hash))
            .build()
    }

    fn decode(anchor: wire::L1Anchor) -> Self {
        Self {
            height: number(anchor.height()),
            hash: hash32(anchor.hash()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Certificate {
    encoded: wire::Certificate,
}

impl Certificate {
    pub fn new(body: Bytes, attestations: Vec<Attestation>) -> Result<Self> {
        ensure!(
            attestations.len() <= MAX_ATTESTATIONS,
            "too many attestations"
        );
        ensure!(
            body.len() <= crate::MAX_TRANSACTION_BYTES,
            "certificate body exceeds transaction size limit"
        );
        let attestations = attestations
            .into_iter()
            .map(|vote| {
                wire::Attestation::new_builder()
                    .signer(wire::Uint16::new_unchecked(Bytes::copy_from_slice(
                        &vote.guardian.to_le_bytes(),
                    )))
                    .signature(wire::Byte64::new_unchecked(Bytes::copy_from_slice(
                        &vote.signature,
                    )))
                    .build()
            })
            .collect();
        Self::decode(
            wire::Certificate::new_builder()
                .body(bytes(body))
                .attestations(
                    wire::AttestationVec::new_builder()
                        .set(attestations)
                        .build(),
                )
                .build(),
        )
    }

    pub(crate) fn decode(encoded: wire::Certificate) -> Result<Self> {
        let certificate = Self { encoded };
        let attestations = certificate.attestations();
        ensure!(
            (1..=MAX_ATTESTATIONS).contains(&attestations.len()),
            "invalid attestation count"
        );
        let mut last = None;
        for vote in attestations {
            ensure!(
                last.is_none_or(|prior| prior < vote.guardian),
                "attestations must be ordered and unique"
            );
            last = Some(vote.guardian);
        }
        Ok(certificate)
    }

    pub(crate) fn validated_entity(encoded: wire::Certificate) -> Self {
        Self { encoded }
    }

    pub(crate) fn encode(&self) -> wire::Certificate {
        self.encoded.clone()
    }

    pub(crate) fn encoded_len(&self) -> usize {
        self.encoded.as_slice().len()
    }

    pub fn body(&self) -> Bytes {
        self.encoded.body().raw_data()
    }

    pub fn attestations(&self) -> impl ExactSizeIterator<Item = Attestation> {
        self.encoded
            .attestations()
            .into_iter()
            .map(|vote| Attestation {
                guardian: u16::from_le_bytes(vote.signer().as_slice().try_into().expect("Uint16")),
                signature: vote.signature().as_slice().try_into().expect("Byte64"),
            })
    }
}

enum Class {
    Mint,
    StakeSnapshot,
    BridgeSettlement,
}

/// Resource caps apply even when the sender owes zero fees.
/// Stake Registry entries have an additional bound in the Stake Registry decoder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub operations: u32,
    pub bytes: u64,
    pub signatures: u32,
}

#[derive(Clone, Debug)]
pub struct BlockBudget {
    mint: Limits,
    stake_snapshot: Limits,
    settlement: Limits,
}

impl BlockBudget {
    pub fn new(mint: Limits, stake_snapshot: Limits, settlement: Limits) -> Self {
        Self {
            mint,
            stake_snapshot,
            settlement,
        }
    }

    /// Reserve work from exactly one operation class. Invoke before any certificate verification.
    pub fn charge(&mut self, tx: &Transaction) -> Result<()> {
        let operation = tx
            .operation()
            .context("ordinary transactions have their own resource budget")?;
        let certificate = tx.certificate().context("missing certificate")?;
        let available = match operation.class()? {
            Class::Mint => &mut self.mint,
            Class::StakeSnapshot => &mut self.stake_snapshot,
            Class::BridgeSettlement => &mut self.settlement,
        };
        let updated = Limits {
            operations: available
                .operations
                .checked_sub(1)
                .context("protocol operation count limit exceeded")?,
            bytes: available
                .bytes
                .checked_sub(tx.as_slice().len() as u64)
                .context("protocol operation byte limit exceeded")?,
            signatures: available
                .signatures
                .checked_sub(certificate.attestations().len() as u32)
                .context("protocol operation signature limit exceeded")?,
        };
        *available = updated;
        Ok(())
    }
}

/// The settlement observer signs a canonical operation after auditing the L1
/// vault transaction and its confirmation history. No L1 RPC is used in consensus.
pub fn settlement_digest(network_id: &Byte32, statement: &BridgeSettlement) -> Byte32 {
    let operation = Operation::BridgeSettlement(statement.clone()).encode();
    hash(
        b"Stem Bridge Settlement\0",
        network_id,
        &[operation.as_slice()],
    )
}

fn verify_settlement(
    network_id: &Byte32,
    statement: &BridgeSettlement,
    certificate: Certificate,
    authority: &Authority,
) -> Result<()> {
    ensure!(
        *network_id != Byte32::default(),
        "zero Stem network identity"
    );
    ensure!(
        statement.authority.as_slice() == authority.generation(),
        "outdated settlement authority"
    );
    authority.verify(
        settlement_digest(network_id, statement).as_slice(),
        &certificate.attestations().collect::<Vec<_>>(),
    )?;
    Ok(())
}

fn byte32(value: &Byte32) -> wire::Byte32 {
    wire::Byte32::new_unchecked(value.as_bytes())
}

fn hash32(value: wire::Byte32) -> Byte32 {
    Byte32::new_unchecked(value.as_bytes())
}

fn out_point(value: &OutPoint) -> wire::OutPoint {
    wire::OutPoint::new_unchecked(value.as_bytes())
}

fn uint64(value: u64) -> wire::Uint64 {
    wire::Uint64::new_unchecked(Bytes::copy_from_slice(&value.to_le_bytes()))
}

fn compact64(value: u64) -> wire::CompactUint64 {
    wire::CompactUint64::new_builder().set(value.into()).build()
}

fn number(value: wire::Uint64) -> u64 {
    u64::from_le_bytes(value.as_slice().try_into().expect("Uint64"))
}

fn bytes(value: Bytes) -> wire::Bytes {
    wire::Bytes::new_unchecked(value.pack().as_bytes())
}
