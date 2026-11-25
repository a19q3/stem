use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use anyhow::{Context, Result, ensure};
use ckb_types::{
    core::ScriptHashType,
    packed::{Byte32, OutPoint, Script},
    prelude::*,
};
use stem_ledger::{
    BlockContext, Cell, CellInput, GroupMessage, LiveCell, ResolvedTransaction, ScriptGroupKind,
    Transaction,
    lineage::{Command, Head, MAX_DATA_BYTES, REGISTER, REPLACE_EXECUTOR, RETIRE, id},
    patricia::{Namespace, Patricia},
    record,
};

use stem_vm::executor;

use super::{Budget, Header, Proof, Store};

const PREFIX: &[u8] = b"lineage/head/";
pub(super) const KEY_BYTES: u64 = (PREFIX.len() + 32) as u64;
pub(super) type Heads = BTreeMap<Byte32, Option<Head>>;

pub struct LineageProof {
    pub head: Option<(Head, LiveCell, Proof)>,
    pub system_path: Proof,
}

impl LineageProof {
    pub fn verify(
        &self,
        network: &Byte32,
        header: &Header,
        id: &Byte32,
        marker_code: &Byte32,
    ) -> bool {
        Store::verify_lineage(
            network,
            header,
            id,
            self.head.as_ref().map(|(head, _, _)| head),
            &self.system_path,
        ) && self.head.as_ref().is_none_or(|(head, cell, path)| {
            matches_head(cell, marker_code, id)
                && record::verify_cell(
                    &Patricia::new(network.clone(), Namespace::Cells),
                    &super::as_key(&header.state.cell_root),
                    &head.out_point,
                    Some(cell),
                    path,
                )
        })
    }
}

pub(super) fn is_marker(code: &Byte32, script: &Script) -> bool {
    &script.code_hash() == code && script.hash_type().as_slice()[0] == ScriptHashType::Data as u8
}

pub(super) fn changes(
    store: &Store,
    code: &Byte32,
    stage: &Heads,
    resolved: &ResolvedTransaction,
    txid: &Byte32,
    omitted: &BTreeSet<usize>,
) -> Result<Heads> {
    let tx = resolved.transaction();
    let mut result = BTreeMap::new();
    let mut inputs = BTreeSet::new();
    let mut outputs = BTreeSet::new();
    for (group_index, group) in resolved.groups().iter().enumerate() {
        if group.kind != ScriptGroupKind::Type || !is_marker(code, &group.script) {
            continue;
        }
        ensure!(
            tx.operation().is_none(),
            "protocol operations cannot write Lineages"
        );
        ensure!(
            group.inputs.len() <= 1 && group.outputs.len() <= 1,
            "a Lineage has one live head"
        );
        let input = group.inputs.first().copied();
        let output = group.outputs.first().copied();
        ensure!(
            input.is_none_or(|index| inputs.insert(index))
                && output.is_none_or(|index| !omitted.contains(&index) && outputs.insert(index)),
            "a Cell can belong to one Lineage and a head must stay live"
        );
        let key =
            Byte32::from_slice(&group.script.args().raw_data()).context("invalid Lineage ID")?;
        let message = resolved
            .group_message(u16::try_from(group_index)?)
            .map(|bytes| GroupMessage::from_slice(bytes))
            .transpose()?;
        let action = if let Some(message) = &message {
            ensure!(
                message.actions.len() == 1,
                "exactly one Lineage lifecycle Action"
            );
            Some(&message.actions[0])
        } else {
            None
        };
        let prior = current_head(store, stage, &key)?;
        let next = if let Some(index) = input {
            let mut prior = prior.context("Lineage input is not registered")?;
            ensure!(
                tx.input(index)
                    .context("missing Lineage input")?
                    .previous_output
                    == prior.out_point,
                "Lineage input is not the current head"
            );
            if let Some(index) = output {
                if let Some(action) = action {
                    ensure!(
                        action.kind == REPLACE_EXECUTOR,
                        "invalid Lineage update Action"
                    );
                    prior.executor = executor(resolved, &action.data)?;
                }
                prior.version = prior
                    .version
                    .checked_add(1)
                    .context("Lineage version exhausted")?;
                prior.out_point = OutPoint::new(txid.clone(), u32::try_from(index)?);
                Some(prior)
            } else {
                ensure!(
                    action.is_some_and(|action| action.kind == RETIRE && action.data.is_empty()),
                    "removing a Lineage head requires an explicit retirement"
                );
                None
            }
        } else {
            let anchor = tx
                .input(0)
                .context("Lineage registration must consume an anchor")?;
            ensure!(
                !resolved.inputs()[0]
                    .cell
                    .type_scripts()
                    .any(|script| is_marker(code, &script)),
                "a Lineage head cannot serve as a registration anchor"
            );
            let index = u32::try_from(output.context("missing registration output")?)?;
            ensure!(
                prior.is_none() && key == id(store.network(), &anchor.previous_output, index),
                "Lineage registration does not match its consumed anchor and output index"
            );
            let action = action
                .filter(|action| action.kind == REGISTER)
                .context("missing Lineage registration Action")?;
            Some(Head {
                out_point: OutPoint::new(txid.clone(), index),
                version: 0,
                executor: executor(resolved, &action.data)?,
            })
        };
        result.insert(key, next);
    }
    Ok(result)
}

fn executor(resolved: &ResolvedTransaction, data: &[u8]) -> Result<Byte32> {
    let code = Byte32::from_slice(data).context("invalid Lineage Executor data hash")?;
    ensure!(code != Byte32::default(), "zero Lineage Executor data hash");
    let script = Script::new_builder()
        .code_hash(code.clone())
        .hash_type(ScriptHashType::Data)
        .build();
    resolved
        .program_index(&script)
        .context("Lineage Executor is absent from resolved dependencies")?;
    Ok(code)
}

fn current_head(store: &Store, stage: &Heads, id: &Byte32) -> Result<Option<Head>> {
    match stage.get(id) {
        Some(value) => Ok(value.clone()),
        None => store
            .system_record(&record_key(id))?
            .as_deref()
            .map(Head::decode)
            .transpose()
            .context(super::StateReadFault),
    }
}

/// Conservative access analysis for one materialization wave. Commands using a
/// Dep Group may expand to arbitrary OutPoints and use the serial path.
pub(super) fn independent(store: &Store, wave: &[Transaction], heads: &Heads) -> Result<bool> {
    let mut writes = BTreeSet::new();
    let mut code_reads = BTreeSet::new();
    for tx in wave {
        let command = tx.lineage_command().expect("command wave");
        let fee = tx.input(0).expect("validated fee input").previous_output;
        if !writes.insert(fee) {
            return Ok(false);
        }
        for target in &command.targets {
            let Some(head) = current_head(store, heads, &target.id)? else {
                return Ok(false);
            };
            if !writes.insert(head.out_point) {
                return Ok(false);
            }
        }
        let id = tx.id(store.network());
        for index in 0..=command.targets.len() as u32 {
            if !writes.insert(OutPoint::new(id.clone(), index)) {
                return Ok(false);
            }
        }
        for dep in tx.cell_deps() {
            if dep.dep_type().as_slice() != [0] {
                return Ok(false);
            }
            code_reads.insert(dep.out_point());
        }
    }
    Ok(writes.is_disjoint(&code_reads))
}

pub(super) struct Materialized {
    pub transaction: Transaction,
    pub fee_cell: Cell,
    pub cycles: u64,
    pub reads: u64,
    pub read_bytes: u64,
    pub executed: bool,
    pub validation_cycles: u64,
    fallback: Option<Transaction>,
}

impl Materialized {
    pub(super) fn rollback(
        mut self,
        spent: u64,
        additional_reads: u64,
        additional_bytes: u64,
    ) -> Result<Self> {
        self.transaction = self
            .fallback
            .take()
            .context("missing verified Lineage refund")?;
        self.executed = false;
        self.cycles = self
            .cycles
            .checked_add(spent)
            .context("Lineage cycles overflow")?;
        self.reads = self
            .reads
            .checked_add(additional_reads)
            .context("Lineage reads overflow")?;
        self.read_bytes = self
            .read_bytes
            .checked_add(additional_bytes)
            .context("Lineage read bytes overflow")?;
        Ok(self)
    }
}

#[derive(Debug)]
pub(super) struct MissingFeeCell;

impl std::fmt::Display for MissingFeeCell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Lineage fee Cell is no longer live")
    }
}

impl std::error::Error for MissingFeeCell {}

pub(super) struct FeeAuthorization {
    input: CellInput,
    cell: Cell,
    refund: Cell,
    transaction: Transaction,
    pub resolved: Arc<ResolvedTransaction>,
}

pub(super) fn resolve_fee(
    store: &Store,
    original: &Transaction,
    command: &Command,
    block: BlockContext<'_>,
    fresh: impl FnMut(&OutPoint) -> Result<LiveCell>,
) -> Result<FeeAuthorization> {
    let mut fresh = fresh;
    let fee_input = original.input(0).context("missing Lineage fee input")?;
    let fee_cell = fresh(&fee_input.previous_output)
        .context(MissingFeeCell)?
        .cell;
    ensure!(
        fee_cell.data().is_empty() && fee_cell.type_scripts().len() == 0,
        "Lineage fee input must hold only CKB Capacity"
    );
    let refund = Cell::new(
        fee_cell
            .capacity()
            .checked_sub(command.fee)
            .context("insufficient Lineage fee input")?,
        fee_cell.lock(),
        vec![],
        Default::default(),
    )?;

    let witness = original.witness(0).context("missing Lineage fee seal")?;
    let initial_fee = Transaction::new(
        original.cell_deps().collect(),
        vec![],
        vec![fee_input.clone()],
        vec![refund.clone()],
        vec![witness.clone()],
    )?;
    let preflight = ResolvedTransaction::resolve(
        store.network.clone(),
        initial_fee.clone(),
        block,
        fresh,
        |_| unreachable!("Lineage fee preflight has no Header Deps"),
    )?
    .authorize_materialized_lineage(original, &fee_cell)?;
    Ok(FeeAuthorization {
        input: fee_input,
        cell: fee_cell,
        refund,
        transaction: initial_fee,
        resolved: Arc::new(preflight),
    })
}

pub(super) fn verify_authorization(
    store: &Store,
    original: &Transaction,
    fee_scope: Arc<ResolvedTransaction>,
    limit: u64,
) -> Result<(u64, Vec<Byte32>)> {
    let actor_cycles = executor::business_signature_cycles(original);
    let fee_limit = limit
        .checked_sub(actor_cycles)
        .context("Lineage business signatures exceed authorization cycles")?;
    let fee_cycles = store.cache.verify(fee_scope, fee_limit, false)?;
    let actors = executor::verify_business_actors(original, &store.network)?;
    Ok((fee_cycles + actor_cycles, actors))
}

fn executor_reads(command: &Command) -> u64 {
    // The Executor code and fee Cell, plus each target's registry record and Live Cell.
    2 + 2 * command.targets.len() as u64
}

pub(super) fn minimum_reads(tx: &Transaction) -> u64 {
    if let Some(members) = tx.lineage_batch() {
        return members.iter().map(minimum_reads).sum();
    }
    tx.lineage_command().map_or(0, |command| {
        // Preflight and final resolution both load the fee input and declared deps.
        // This remains a lower bound when execution leaves only the fee refund or
        // a dep group expands to additional Cells.
        let fee_scope_reads = 1 + tx.cell_deps().len() as u64;
        executor_reads(command) + 2 * fee_scope_reads
    })
}

pub(super) fn materialize(
    store: &Store,
    original: &Transaction,
    command: &Command,
    block: BlockContext<'_>,
    heads: &Heads,
    budget: Budget,
    mut fresh: impl FnMut(&OutPoint) -> Result<LiveCell>,
) -> Result<Materialized> {
    let marker = store
        .lineage
        .as_ref()
        .context("Lineages are not configured")?;
    let FeeAuthorization {
        input: fee_input,
        cell: fee_cell,
        refund,
        transaction: initial_fee,
        resolved: preflight,
    } = resolve_fee(store, original, command, block, &mut fresh)?;
    let executor_code = fresh(&command.executor_dep)?.cell;
    ensure!(
        executor_code.data_hash() == &command.executor,
        "Lineage Executor Cell does not match its signed data hash"
    );
    let mut inputs = Vec::with_capacity(command.targets.len());
    let mut loaded = Vec::with_capacity(command.targets.len());
    let mut read_bytes = (executor_code.as_slice().len() + fee_cell.as_slice().len()) as u64;
    for target in &command.targets {
        let head = current_head(store, heads, &target.id)?.context("Lineage has no live head")?;
        ensure!(
            head.executor == command.executor
                && (target.min_version..=target.max_version).contains(&head.version),
            "Lineage head is outside the signed version or Executor bounds"
        );
        let cell = fresh(&head.out_point)?;
        ensure!(
            matches_head(&cell, marker, &target.id) && cell.cell.data().len() <= MAX_DATA_BYTES,
            "invalid Lineage Executor input"
        );
        ensure!(
            cell.cell.lock() != fee_cell.lock(),
            "Lineage head and fee Lock groups must be distinct"
        );
        read_bytes +=
            KEY_BYTES + Head::MAX_ENCODED_BYTES as u64 + cell.cell.as_slice().len() as u64;
        inputs.push(executor::Input {
            id: target.id.clone(),
            version: head.version,
            capacity: cell.cell.capacity(),
            data: cell.cell.data(),
        });
        loaded.push((head.out_point, cell.cell));
    }
    let reads = executor_reads(command) + preflight.cell_reads() as u64;
    let read_bytes = read_bytes + preflight.resolved_cell_bytes() as u64;
    ensure!(
        reads <= budget.reads && read_bytes <= budget.read_bytes,
        "Lineage resolution exceeds the block read budget"
    );
    let cycle_limit = store.limits.transaction_cycles.min(budget.cycles);
    let authorization_limit = cycle_limit
        .checked_sub(command.executor_cycles)
        .and_then(|limit| limit.checked_sub(command.validation_cycles))
        .context("signed Lineage budgets exceed remaining transaction cycles")?;
    let (fee_cycles, actors) =
        verify_authorization(store, original, preflight, authorization_limit)?;
    let payer_lock = if actors.is_empty() {
        fee_cell.lock().calc_script_hash()
    } else {
        // Sponsorship is chosen after business signing. The Executor must
        // derive effects from the signed actors and terms, not the payer.
        Byte32::default()
    };
    let run = executor::run(
        &executor_code.data(),
        executor::ContextData {
            height: block.candidate.height,
            parent_time_ms: block.parent.context.timestamp_ms,
            payer_lock,
            actors,
            payload: command.payload.clone(),
            inputs,
        },
        command.executor_cycles,
    );
    let run = run.and_then(|(results, cycles)| {
        let successors = successors(&loaded, &command.targets, results)?;
        for cell in &successors {
            block.capacity.verify(cell)?;
        }
        Ok((successors, cycles))
    });
    let (transaction, cycles, executed, fallback) = if let Ok((successors, cycles)) = run {
        let fee_lock = fee_cell.lock().as_bytes();
        let before_fee: BTreeSet<_> = loaded
            .iter()
            .map(|(_, cell)| cell.lock().as_bytes())
            .filter(|key| key < &fee_lock)
            .collect();
        let mut witness = original.witness(0).expect("resolved fee seal");
        witness.group_index = u16::try_from(before_fee.len())?;
        let tx = Transaction::new(
            original.cell_deps().collect(),
            vec![],
            std::iter::once(fee_input)
                .chain(
                    loaded
                        .into_iter()
                        .map(|(point, _)| CellInput::new(point, None)),
                )
                .collect(),
            std::iter::once(refund).chain(successors).collect(),
            vec![witness],
        )?;
        (tx, cycles, true, Some(initial_fee))
    } else {
        // A guest trap, nonzero exit, exhausted budget or missing output cannot
        // stage a head. Charge the signed Executor budget to the fee-only path.
        (initial_fee, command.executor_cycles, false, None)
    };
    Ok(Materialized {
        transaction,
        fee_cell,
        cycles: fee_cycles
            .checked_add(cycles)
            .context("Lineage cycles overflow")?,
        reads,
        read_bytes,
        executed,
        validation_cycles: command.validation_cycles,
        fallback,
    })
}

fn successors(
    heads: &[(OutPoint, Cell)],
    targets: &[stem_ledger::lineage::Target],
    results: Vec<executor::Output>,
) -> Result<Vec<Cell>> {
    let mut before = 0u128;
    let mut after = 0u128;
    let successors = heads
        .iter()
        .zip(targets)
        .zip(results)
        .map(|(((_, cell), target), result)| {
            ensure!(
                result.capacity >= target.min_capacity,
                "Lineage successor is below the signed Capacity floor"
            );
            before = before
                .checked_add(u128::from(cell.capacity()))
                .context("Lineage Capacity overflow")?;
            after = after
                .checked_add(u128::from(result.capacity))
                .context("Lineage Capacity overflow")?;
            Cell::new(
                result.capacity,
                cell.lock(),
                cell.type_scripts().collect(),
                result.data,
            )
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        before == after,
        "Lineage Executor must conserve Head Capacity"
    );
    Ok(successors)
}

pub(super) fn record_key(id: &Byte32) -> Vec<u8> {
    [PREFIX, id.as_slice()].concat()
}

fn matches_head(cell: &LiveCell, code: &Byte32, id: &Byte32) -> bool {
    cell.cell
        .type_scripts()
        .any(|script| is_marker(code, &script) && script.args().raw_data() == id.as_bytes())
}

impl Store {
    pub fn prove_current_lineage(&self, id: &Byte32) -> Result<LineageProof> {
        let code = self
            .lineage
            .as_ref()
            .context("Lineages are not configured")?;
        let (head, system_path) = self.prove_lineage(id)?;
        let head = head
            .map(|head| -> Result<_> {
                let (cell, path) = self.prove_cell(&head.out_point)?;
                let cell = cell.context("registered Lineage head is no longer live")?;
                ensure!(
                    matches_head(&cell, code, id),
                    "registered head does not carry its Lineage marker"
                );
                Ok((head, cell, path))
            })
            .transpose()?;
        Ok(LineageProof { head, system_path })
    }

    pub fn prove_lineage(&self, id: &Byte32) -> Result<(Option<Head>, Proof)> {
        ensure!(self.lineage.is_some(), "Lineages are not configured");
        let (record, proof) = self.prove_system(&record_key(id))?;
        Ok((record.as_deref().map(Head::decode).transpose()?, proof))
    }

    pub fn verify_lineage(
        network: &Byte32,
        header: &Header,
        id: &Byte32,
        head: Option<&Head>,
        proof: &Proof,
    ) -> bool {
        let record = head.map(Head::encode);
        Self::verify_system_record(network, header, &record_key(id), record.as_deref(), proof)
    }
}
