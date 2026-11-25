use anyhow::{Context, Result, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::*};
use fern_stake::{
    AcceptedSnapshot, Authority, Member, Registry, StakeConfig, StakeDomain,
    StakeSnapshot as RegistrySnapshot,
};

use super::{BlockBudget, Operation, StakeSnapshot};
use crate::{Transaction, hash};

pub fn snapshot_body(snapshot: &RegistrySnapshot, registry: &Registry) -> Bytes {
    let snapshot = snapshot.encode();
    let registry = registry.encode();
    let mut body = Vec::with_capacity(2 + snapshot.len() + registry.len());
    body.extend_from_slice(&(snapshot.len() as u16).to_le_bytes());
    body.extend_from_slice(&snapshot);
    body.extend_from_slice(&registry);
    Bytes::from(body)
}

pub fn snapshot_commitment(network_id: &Byte32, body: &[u8]) -> Byte32 {
    hash(b"Fern Stake Snapshot body\0", network_id, &[body])
}

pub fn snapshot_digest(network_id: &Byte32, statement: &StakeSnapshot) -> Byte32 {
    let operation = Operation::StakeSnapshot(statement.clone()).encode();
    hash(
        b"Fern native Stake Snapshot\0",
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

impl SnapshotContext<'_> {
    /// The control transaction pays zero Capacity. Its certificate work is
    /// accounted for before any signature is verified.
    pub fn verify(
        &self,
        tx: &Transaction,
        budget: &mut BlockBudget,
    ) -> Result<(AcceptedSnapshot, Vec<Member>)> {
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
        let network_id: Byte32 = self.domain.fern_network_id.pack();
        ensure!(
            network_id != Byte32::default()
                && snapshot_commitment(&network_id, &body) == statement.commitment,
            "incorrect Stake Snapshot body commitment"
        );
        let (length, data) = body
            .split_at_checked(2)
            .context("truncated Stake Snapshot body")?;
        let (snapshot, registry) = data
            .split_at_checked(u16::from_le_bytes(length.try_into()?) as usize)
            .context("truncated Stake Snapshot statement")?;
        let snapshot = RegistrySnapshot::decode(snapshot)?;
        let registry = Registry::decode(registry)?;
        ensure!(
            snapshot.target_epoch == statement.epoch,
            "Stake Snapshot epoch mismatch"
        );
        self.authority.verify(
            snapshot_digest(&network_id, &statement).as_slice(),
            &cert.attestations().collect::<Vec<_>>(),
        )?;
        Ok(snapshot.validate_statement(
            self.domain,
            Some(self.previous),
            &registry,
            self.authority,
            self.config,
        )?)
    }
}
