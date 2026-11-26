use crate::{
    stake_proof::{decode_members, encode_members},
    strand::prelude::{Entity as WireEntity, Reader as WireReader},
    wire,
};
use anyhow::{Context, Result, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::*};
use stem_stake::{
    AcceptedSnapshot, Attestation, Authority, MAX_RECORDS, Member, Registry, StakeConfig,
    StakeDomain, StakeSnapshot as RegistrySnapshot, ValidatorKey, l1::guardian_generation,
};

use super::{BlockBudget, Operation, StakeSnapshot};
use crate::{Transaction, hash};

pub const ARRIVING_GUARDIAN: u16 = 1 << 15;

pub fn snapshot_body(
    snapshot: &RegistrySnapshot,
    registry: &Registry,
    arriving: &[Member],
) -> Result<Bytes> {
    ensure!(
        arriving.len() <= MAX_RECORDS,
        "too many arriving Stake guardians"
    );
    let snapshot = snapshot.encode();
    let registry = registry.encode();
    let mut body = Vec::with_capacity(4 + snapshot.len() + 40 * arriving.len() + registry.len());
    body.extend_from_slice(&(snapshot.len() as u16).to_le_bytes());
    body.extend_from_slice(&snapshot);
    body.extend_from_slice(&(arriving.len() as u16).to_le_bytes());
    body.extend_from_slice(&encode_members(arriving));
    body.extend_from_slice(&registry);
    Ok(body.into())
}

fn take_count(data: &[u8]) -> Result<(usize, &[u8])> {
    let (length, rest) = data
        .split_at_checked(2)
        .context("truncated Stake Snapshot count")?;
    Ok((u16::from_le_bytes(length.try_into()?) as usize, rest))
}

/// Read the historical u16 envelope without changing the bytes authenticated by guardians.
fn decode_body(data: &[u8]) -> Result<(RegistrySnapshot, Vec<Member>, Registry, &[u8])> {
    let (length, data) = take_count(data)?;
    let (statement, data) = data
        .split_at_checked(length)
        .context("truncated Stake Snapshot statement")?;
    wire::StakeSnapshotStatementPrefixReader::span(statement)?;
    let statement = RegistrySnapshot::decode(statement)?;
    let (count, data) = take_count(data)?;
    ensure!(count <= MAX_RECORDS, "too many arriving Stake guardians");
    let member_size = wire::StakeMember::TOTAL_SIZE;
    let (members, data) = data
        .split_at_checked(count * member_size)
        .context("truncated arriving Stake guardians")?;
    let members = decode_members(members)?;
    let header_len = wire::StakeRegistryHeaderReader::span(data)?;
    let header = wire::StakeRegistryHeaderReader::from_slice(&data[..header_len])?;
    let records = usize::from(u16::from_le_bytes(header.members().as_slice().try_into()?));
    ensure!(records <= MAX_RECORDS, "too many registry records");
    let size = header_len + records * wire::StakeRegistryEntry::TOTAL_SIZE;
    let (registry, tail) = data
        .split_at_checked(size)
        .context("truncated Stake Registry")?;
    Ok((statement, members, Registry::decode(registry)?, tail))
}

/// Native genesis stores the snapshot body followed by a fixed-width guardian certificate.
pub fn genesis_snapshot(
    snapshot: &RegistrySnapshot,
    registry: &Registry,
    attestations: &[Attestation],
) -> Result<Vec<u8>> {
    let mut out = snapshot_body(snapshot, registry, &[])?.to_vec();
    out.extend_from_slice(&u16::try_from(attestations.len())?.to_le_bytes());
    for a in attestations {
        out.extend_from_slice(&a.guardian.to_le_bytes());
        out.extend_from_slice(&a.signature);
    }
    Ok(out)
}

pub fn read_genesis_snapshot(
    bytes: &[u8],
) -> Result<(RegistrySnapshot, Registry, Vec<Attestation>)> {
    let (statement, arriving, registry, tail) = decode_body(bytes)?;
    ensure!(arriving.is_empty(), "genesis cannot rotate guardians");
    let (count, tail) = take_count(tail)?;
    let width = wire::Attestation::TOTAL_SIZE;
    ensure!(
        count <= MAX_RECORDS && tail.len() == count * width,
        "invalid genesis Stake Snapshot certificate length"
    );
    let votes = tail
        .chunks_exact(width)
        .map(|raw| {
            let vote = wire::AttestationReader::from_slice(raw)?;
            Ok(Attestation {
                guardian: u16::from_le_bytes(vote.signer().as_slice().try_into()?),
                signature: vote.signature().as_slice().try_into()?,
            })
        })
        .collect::<Result<_>>()?;
    Ok((statement, registry, votes))
}

pub fn snapshot_commitment(network_id: &Byte32, body: &[u8]) -> Byte32 {
    hash(b"Stem Stake Snapshot body\0", network_id, &[body])
}

pub fn snapshot_digest(network_id: &Byte32, statement: &StakeSnapshot) -> Byte32 {
    let operation = Operation::StakeSnapshot(statement.clone()).encode();
    hash(
        b"Stem native Stake Snapshot\0",
        network_id,
        &[operation.as_slice()],
    )
}

/// Context read from the committed native ledger and its scheduled signing header.
pub struct SnapshotContext<'a> {
    pub height: u64,
    pub window: u64,
    pub signing_anchor_height: u64,
    pub signing_anchor: &'a Byte32,
    pub domain: &'a StakeDomain,
    pub previous: AcceptedSnapshot,
    pub authority: &'a Authority,
    pub config: &'a StakeConfig,
}

pub struct VerifiedSnapshot {
    pub accepted: AcceptedSnapshot,
    pub eligible: Vec<Member>,
    pub rotation: Option<Vec<Member>>,
    pub retired: Vec<ValidatorKey>,
}

impl SnapshotContext<'_> {
    /// The control transaction pays zero Capacity. Its certificate work is
    /// accounted for before any signature is verified.
    pub fn verify(&self, tx: &Transaction, budget: &mut BlockBudget) -> Result<VerifiedSnapshot> {
        let Some(Operation::StakeSnapshot(statement)) = tx.operation() else {
            anyhow::bail!("expected Stake Snapshot operation");
        };
        ensure!(
            (3..self.config.blocks_per_epoch).contains(&self.window),
            "invalid Stake Snapshot signing window"
        );
        let update = self.config.update_height(statement.epoch)?;
        let opening = self
            .config
            .effective_height(statement.epoch)?
            .checked_sub(self.window)
            .context("invalid Stake Snapshot opening height")?;
        ensure!(
            (opening..=update).contains(&self.height),
            "Stake Snapshot is outside its submission window"
        );
        ensure!(
            opening.checked_sub(1) == Some(self.signing_anchor_height)
                && statement.signing_anchor == *self.signing_anchor,
            "incorrect Stake Snapshot signing anchor"
        );
        budget.charge(tx)?;
        let cert = tx
            .certificate()
            .context("missing Stake Snapshot certificate")?;
        let body = cert.body();
        let network_id: Byte32 = self.domain.stem_network_id.pack();
        ensure!(
            network_id != Byte32::default()
                && snapshot_commitment(&network_id, &body) == statement.commitment,
            "incorrect Stake Snapshot body commitment"
        );
        let (snapshot, members, registry, tail) = decode_body(&body)?;
        ensure!(tail.is_empty(), "unexpected Stake Snapshot body suffix");
        ensure!(
            snapshot.target_epoch == statement.epoch,
            "Stake Snapshot epoch mismatch"
        );
        let mut incumbent = Vec::new();
        let mut arriving = Vec::new();
        for vote in cert.attestations() {
            if vote.guardian & ARRIVING_GUARDIAN == 0 {
                incumbent.push(vote);
            } else {
                arriving.push(Attestation {
                    guardian: vote.guardian & !ARRIVING_GUARDIAN,
                    signature: vote.signature,
                });
            }
        }
        let message = snapshot_digest(&network_id, &statement);
        self.authority.verify(message.as_slice(), &incumbent)?;
        let changed = snapshot.guardian_generation != self.authority.generation();
        let next = if changed {
            let generation = guardian_generation(&members);
            ensure!(
                generation == snapshot.guardian_generation,
                "arriving authority does not match Stake Registry"
            );
            let next = Authority::new(generation, &members)?;
            next.verify(message.as_slice(), &arriving)?;
            Some(next)
        } else {
            ensure!(
                members.is_empty() && arriving.is_empty(),
                "unexpected Stake guardian rotation"
            );
            None
        };
        let (accepted, eligible) = snapshot.validate_statement(
            self.domain,
            Some(self.previous),
            &registry,
            next.as_ref().unwrap_or(self.authority),
            self.config,
        )?;
        Ok(VerifiedSnapshot {
            accepted,
            eligible,
            rotation: changed.then_some(members),
            retired: registry.retired_keys(statement.epoch).collect(),
        })
    }
}
