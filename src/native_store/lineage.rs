use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use ckb_types::{
    core::ScriptHashType,
    packed::{Byte32, OutPoint, Script},
    prelude::*,
};
use stem_ledger::{
    GroupMessage, LiveCell, ResolvedTransaction, ScriptGroupKind,
    lineage::{Head, REGISTER, REPLACE_EXECUTOR, RETIRE, id},
    patricia::{Namespace, Patricia},
    record,
};

use super::{Header, Proof, Store};

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
        let prior = match stage.get(&key) {
            Some(value) => value.clone(),
            None => store
                .system_record(&record_key(&key))?
                .as_deref()
                .map(Head::decode)
                .transpose()?,
        };
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
