use anyhow::{Context, Result, ensure};
use ckb_types::packed::{Byte32, OutPoint};
use fern_stake::Authority;

use super::{BlockBudget, L1Anchor, Operation, SettlementAction, VaultUpdate, verify_settlement};
use crate::Transaction;

/// Counters derived from the authenticated L1 vault and pending receipts.
/// The vault Capacity is liquid Capacity, excluding its L1 storage collateral.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Lane {
    pub vault: OutPoint,
    pub next_nonce: u64,
    pub capacity: u64,
    pub units: u128,
    pub reserved_capacity: u64,
    pub reserved_units: u128,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingWithdrawal {
    pub receipt: Byte32,
    pub capacity: u64,
    pub units: u128,
}

/// Required reads from the committed or candidate SystemRoot.
/// An implementation exposes values only for the specified logical key.
pub trait BridgeLedger {
    fn lane(&self, id: &Byte32) -> Result<Lane>;
    fn pending(&self, id: &Byte32, nonce: u64) -> Result<PendingWithdrawal>;
}

/// Apply these changes in the same commit as all other protocol state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettlementChanges {
    pub previous_anchor: L1Anchor,
    pub anchor: L1Anchor,
    pub lanes: Vec<(Byte32, Lane)>,
    pub paid: Option<(Byte32, u64)>,
}

impl SettlementChanges {
    /// Authenticate the observation, consume its work budget, and prepare an atomic update.
    pub fn verify(
        tx: &Transaction,
        network_id: &Byte32,
        l1_genesis: &Byte32,
        anchor: &L1Anchor,
        authority: &Authority,
        state: &impl BridgeLedger,
        budget: &mut BlockBudget,
    ) -> Result<Self> {
        let Some(Operation::BridgeSettlement(statement)) = tx.operation() else {
            anyhow::bail!("expected Bridge Settlement operation");
        };
        ensure!(
            statement.l1_genesis == *l1_genesis,
            "settlement names a different L1 chain"
        );
        ensure!(
            statement.previous_anchor == *anchor,
            "settlement names a stale L1 anchor"
        );
        budget.charge(tx)?;
        let certificate = tx
            .certificate()
            .context("missing bridge settlement certificate")?;
        verify_settlement(network_id, &statement, certificate, authority)?;

        let (mut lanes, paid) = match statement.action {
            SettlementAction::WithdrawalPaid {
                vault,
                nonce,
                receipt,
            } => {
                let mut lane = updated_lane(state, &vault)?;
                ensure!(
                    lane.next_nonce == nonce,
                    "withdrawal is outside the current lane nonce"
                );
                let pending = state.pending(&vault.lane, nonce)?;
                ensure!(pending.receipt == receipt, "incorrect withdrawal receipt");
                ensure!(pending.capacity > 0, "withdrawal has zero Capacity");
                lane.capacity = lane
                    .capacity
                    .checked_sub(pending.capacity)
                    .context("insufficient lane capacity")?;
                lane.units = lane
                    .units
                    .checked_sub(pending.units)
                    .context("insufficient lane units")?;
                lane.reserved_capacity = lane
                    .reserved_capacity
                    .checked_sub(pending.capacity)
                    .context("withdrawal capacity was not reserved")?;
                lane.reserved_units = lane
                    .reserved_units
                    .checked_sub(pending.units)
                    .context("withdrawal units were not reserved")?;
                lane.next_nonce = nonce.checked_add(1).context("withdrawal nonce exhausted")?;
                ensure_liquid(&lane)?;
                let paid = Some((vault.lane.clone(), nonce));
                (vec![(vault.lane, lane)], paid)
            }
            SettlementAction::VaultRotated { vault } => (
                vec![(vault.lane.clone(), updated_lane(state, &vault)?)],
                None,
            ),
            SettlementAction::ReserveMoved {
                source,
                destination,
                capacity,
            } => {
                let mut send = updated_lane(state, &source)?;
                let mut receive = updated_lane(state, &destination)?;
                send.capacity = send
                    .capacity
                    .checked_sub(capacity)
                    .context("insufficient source reserve")?;
                receive.capacity = receive
                    .capacity
                    .checked_add(capacity)
                    .context("destination reserve overflow")?;
                ensure_liquid(&send)?;
                ensure_liquid(&receive)?;
                (vec![(source.lane, send), (destination.lane, receive)], None)
            }
        };
        lanes.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        Ok(Self {
            previous_anchor: statement.previous_anchor,
            anchor: statement.anchor,
            lanes,
            paid,
        })
    }
}

fn updated_lane(state: &impl BridgeLedger, vault: &VaultUpdate) -> Result<Lane> {
    let mut lane = state.lane(&vault.lane)?;
    ensure!(
        lane.vault == vault.input,
        "settlement names a spent or foreign vault"
    );
    ensure_liquid(&lane)?;
    lane.vault = vault.output.clone();
    Ok(lane)
}

fn ensure_liquid(lane: &Lane) -> Result<()> {
    ensure!(
        lane.capacity >= lane.reserved_capacity && lane.units >= lane.reserved_units,
        "settlement would spend reserved funds"
    );
    Ok(())
}
