//! Authenticated, height-indexed one-shot Lineage scheduling.

use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_types::{packed::OutPoint, prelude::*};
use stem_ledger::{
    BlockContext, Origin, ResolvedTransaction, Transaction,
    block::{HeaderContext, Receipt},
    keeper::{self, Action},
};

use super::{Budget, Limits, Overlay, Proof, StateReadFault, Store, charge, lineage};

const CONFIG: &[u8] = b"keeper/policy";
const QUEUE: &[u8] = b"keeper/due/";
const FEE: &[u8] = b"keeper/fee/";

#[derive(Clone, Copy, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct KeeperPolicy {
    pub max_delay: u16,
    pub per_height: u16,
    /// Nonrefundable reservation fee in shannons, in addition to resource fees.
    pub reservation_fee: u64,
}

impl KeeperPolicy {
    pub(super) fn validate(self, limits: Limits) -> Result<()> {
        ensure!(
            self.max_delay > 0
                && self.reservation_fee > 0
                && (1..=keeper::MAX_PER_HEIGHT).contains(&usize::from(self.per_height))
                && u32::from(self.per_height) < limits.transaction_count,
            "invalid Keeper policy"
        );
        let slot = self.slot(limits);
        ensure!(
            slot.bytes >= keeper::MAX_COMMAND_BYTES as u64
                && slot.reads >= 4
                && slot.read_bytes >= keeper::MAX_COMMAND_BYTES as u64 + 128
                && slot.write_bytes >= 128
                && slot.cycles > 0,
            "block limits cannot reserve Keeper slots"
        );
        Ok(())
    }

    pub(super) fn record(self) -> (&'static [u8], [u8; 12]) {
        let mut record = [0; 12];
        record[..2].copy_from_slice(&self.max_delay.to_le_bytes());
        record[2..4].copy_from_slice(&self.per_height.to_le_bytes());
        record[4..].copy_from_slice(&self.reservation_fee.to_le_bytes());
        (CONFIG, record)
    }

    fn slot(self, limits: Limits) -> Budget {
        // Keep one share of each block resource for submitted transactions.
        let shares = u64::from(self.per_height) + 1;
        Budget {
            bytes: limits.transaction_bytes / shares,
            cycles: limits.transaction_cycles.min(limits.block_cycles / shares),
            reads: limits.cell_reads / shares,
            read_bytes: limits.cell_read_bytes / shares,
            write_bytes: limits.cell_write_bytes / shares,
        }
    }
}

#[derive(Default)]
pub(super) struct Stage {
    queues: BTreeMap<u64, Vec<Transaction>>,
    reservations: BTreeMap<OutPoint, Option<u64>>,
    pub running: bool,
}

pub(super) struct Change {
    height: u64,
    queue: Vec<Transaction>,
    point: OutPoint,
    reservation: Option<u64>,
    pub fee: u64,
    pub cycles: u64,
}

impl Stage {
    fn queue(&self, store: &Store, height: u64) -> Result<(Vec<Transaction>, u64)> {
        if let Some(queue) = self.queues.get(&height) {
            return Ok((queue.clone(), queue_size(queue)));
        }
        let Some(record) = store.system_record(&queue_key(height))? else {
            return Ok((vec![], 0));
        };
        let queue = keeper::decode_queue(&record, height).context(StateReadFault)?;
        Ok((queue, record.len() as u64))
    }

    fn reservation(&self, store: &Store, point: &OutPoint) -> Result<Option<u64>> {
        match self.reservations.get(point) {
            Some(height) => Ok(*height),
            None => store
                .system_record(&reservation_key(point))?
                .map(|value| -> Result<_> { Ok(u64::from_le_bytes(value.as_slice().try_into()?)) })
                .transpose()
                .context(StateReadFault),
        }
    }

    pub(super) fn apply(&mut self, change: Change) {
        self.queues.insert(change.height, change.queue);
        self.reservations.insert(change.point, change.reservation);
    }

    pub(super) fn finish(self) -> impl Iterator<Item = (Vec<u8>, Option<Vec<u8>>)> {
        self.queues
            .into_iter()
            .map(|(height, queue)| {
                (
                    queue_key(height),
                    (!queue.is_empty()).then(|| keeper::encode_queue(&queue).to_vec()),
                )
            })
            .chain(self.reservations.into_iter().map(|(point, height)| {
                (
                    reservation_key(&point),
                    height.map(|height| height.to_le_bytes().to_vec()),
                )
            }))
    }
}

pub(super) fn change(
    store: &Store,
    stage: &Stage,
    carrier: &ResolvedTransaction,
    block: BlockContext<'_>,
    budget: &mut Budget,
    fresh: impl FnMut(&OutPoint) -> Result<stem_ledger::LiveCell>,
) -> Result<Option<Change>> {
    let tx = carrier.transaction();
    let Some(action) = tx.keeper_action() else {
        return Ok(None);
    };
    let policy = store.keeper.context("Keeper is not configured")?;
    let (height, point, command) = match action {
        Action::Schedule(raw) => {
            let command = keeper::decode_command(raw)?;
            (
                command.execute_at().expect("bound command"),
                fee_point(&command),
                Some(command),
            )
        }
        Action::Cancel { height, fee_input } => (*height, fee_input.clone(), None),
    };
    let delay = height
        .checked_sub(block.candidate.height)
        .context("Keeper deadline passed")?;
    ensure!(
        delay != 0 && delay <= u64::from(policy.max_delay),
        "Keeper deadline outside the reservation horizon"
    );

    let (mut queue, old_bytes) = stage.queue(store, height)?;
    let reserved = stage.reservation(store, &point)?;
    let entry = queue.binary_search_by_key(&point, fee_point);
    let consumed = tx.inputs().any(|input| input.previous_output == point);
    charge(&mut budget.reads, 2, "Keeper queue reads")?;
    charge(
        &mut budget.read_bytes,
        record_bytes(&point, old_bytes),
        "Keeper queue read bytes",
    )?;
    let (reservation, fee, cycles) = if let Some(command) = command {
        ensure!(
            !consumed && reserved.is_none() && entry.is_err(),
            "Keeper fee input already used or reserved"
        );
        ensure!(
            queue.len() < usize::from(policy.per_height),
            "Keeper height is full"
        );
        ensure!(
            carrier.fee() >= policy.reservation_fee,
            "insufficient Keeper reservation fee"
        );
        let terms = command.lineage_command().expect("Keeper command terms");
        let slot = policy.slot(store.limits);
        let escrow = lineage::resolve_fee(store, &command, terms, block, fresh)?.resolved;
        let authorization_limit = slot
            .cycles
            .checked_sub(terms.executor_cycles)
            .and_then(|left| left.checked_sub(terms.validation_cycles))
            .context("signed Lineage budgets exceed a Keeper slot")?;
        // The future fee Cell authorizes this particular command now. The
        // Keeper revalidates the actual fee and all business scripts when due.
        charge(
            &mut budget.reads,
            escrow.cell_reads() as u64 + 1,
            "Keeper authorization reads",
        )?;
        charge(
            &mut budget.read_bytes,
            escrow.resolved_cell_bytes() as u64 + escrow.inputs()[0].cell.as_slice().len() as u64,
            "Keeper authorization read bytes",
        )?;
        let cycles = store
            .cache
            .verify(escrow, authorization_limit.min(budget.cycles), false)?;
        charge(&mut budget.cycles, cycles, "Keeper authorization cycles")?;
        queue.insert(entry.expect_err("new reservation"), command);
        (Some(height), policy.reservation_fee, cycles)
    } else {
        ensure!(
            consumed && reserved == Some(height),
            "Keeper cancellation must consume the reserved fee Cell"
        );
        queue.remove(entry.ok().context("reserved Keeper command is absent")?);
        (None, 0, 0)
    };
    charge(
        &mut budget.write_bytes,
        record_bytes(&point, queue_size(&queue)),
        "Keeper queue write bytes",
    )?;
    Ok(Some(Change {
        height,
        queue,
        point,
        reservation,
        fee,
        cycles,
    }))
}

impl Store {
    pub fn prove_keeper_queue(&self, height: u64) -> Result<(Option<Vec<u8>>, Proof)> {
        self.prove_system(&queue_key(height))
    }

    pub fn verify_keeper_queue(
        network: &ckb_types::packed::Byte32,
        header: &stem_ledger::block::Header,
        height: u64,
        record: Option<&[u8]>,
        proof: &Proof,
    ) -> bool {
        let tree = stem_ledger::patricia::Patricia::new(
            network.clone(),
            stem_ledger::patricia::Namespace::System,
        );
        tree.verify(
            &super::as_key(&header.state.system_root),
            &tree.record_key(&queue_key(height)),
            record.map(|bytes| tree.value_hash(bytes)).as_ref(),
            proof,
        )
    }

    pub(super) fn due(&self, height: u64) -> Result<Vec<Transaction>> {
        let Some(policy) = self.keeper else {
            return Ok(vec![]);
        };
        let (queue, _) = Stage::default().queue(self, height)?;
        ensure!(
            queue.len() <= usize::from(policy.per_height),
            StateReadFault
        );
        Ok(queue)
    }

    pub(super) fn run_keeper(
        &self,
        due: &[Transaction],
        context: &HeaderContext,
        origin: &Origin,
        budget: &mut Budget,
        overlay: &mut Overlay,
    ) -> Result<Vec<Receipt>> {
        if due.is_empty() {
            return Ok(vec![]);
        }
        let slot = self.keeper.expect("Keeper is enabled").slot(self.limits);
        let mut receipts = Vec::with_capacity(due.len());
        overlay.keeper.running = true;
        for command in due {
            let point = fee_point(command);
            ensure!(
                overlay.keeper.reservation(self, &point)? == Some(context.height),
                StateReadFault
            );
            let mut remaining = slot;
            charge(&mut remaining.reads, 2, "Keeper slot reads")?;
            charge(
                &mut remaining.read_bytes,
                record_bytes(&point, command.as_slice().len() as u64 + 8),
                "Keeper slot read bytes",
            )?;
            charge(
                &mut remaining.write_bytes,
                record_bytes(&point, 0),
                "Keeper slot write bytes",
            )?;
            let receipt = match self.stage(
                command,
                context,
                origin,
                &mut remaining,
                overlay,
                None,
                |resolved, limit| self.cache.verify(resolved, limit, false),
            ) {
                Ok(receipt) => receipt,
                Err(error) if error.is::<StateReadFault>() => return Err(error),
                Err(_) => {
                    // The prepaid reservation purchased the attempt. A stale
                    // reference or unaffordable candidate never changes a Cell.
                    remaining = Budget::empty();
                    Receipt {
                        fee: 0,
                        cycles: slot.cycles,
                        protocol_result: stem_ledger::lineage::Outcome::Skipped.encode(),
                    }
                }
            };
            budget.charge_slot(slot, remaining)?;
            overlay.keeper.reservations.insert(point, None);
            receipts.push(receipt);
        }
        overlay.keeper.running = false;
        overlay.keeper.queues.insert(context.height, vec![]);
        Ok(receipts)
    }
}

fn fee_point(tx: &Transaction) -> OutPoint {
    tx.input(0)
        .expect("validated Keeper fee input")
        .previous_output
}

fn queue_size(queue: &[Transaction]) -> u64 {
    if queue.is_empty() {
        0
    } else {
        keeper::encode_queue(queue).len() as u64
    }
}

fn record_bytes(point: &OutPoint, queue_bytes: u64) -> u64 {
    (QUEUE.len() + 8 + FEE.len() + point.as_slice().len() + 8) as u64 + queue_bytes
}

fn queue_key(height: u64) -> Vec<u8> {
    [QUEUE, height.to_be_bytes().as_slice()].concat()
}

fn reservation_key(point: &OutPoint) -> Vec<u8> {
    [FEE, point.as_slice()].concat()
}
