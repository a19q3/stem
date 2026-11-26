//! Recover a return proof from the actual witnessed CKB custody chain.

use std::{collections::HashMap, path::Path};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{HeaderView as JsonHeader, JsonBytes, OutPoint as JsonPoint};
use ckb_types::{
    H256,
    bytes::Bytes,
    core::{DepType, HeaderView, ScriptHashType, TransactionView},
    packed::{Byte32, CellOutput, OutPoint, OutPointVec, Script, Transaction},
    prelude::*,
};
use ed25519_dalek::{Signature, VerifyingKey};
use serde::Deserialize;
use serde_json::json;
use stem_bridge_wire::{
    intercell::{Identity, MailboxState, Message},
    metatype::{
        LogicalScript, MAX_MANIFEST_BYTES, MAX_MANIFEST_DEFINITION_BYTES,
        MAX_TRANSACTION_CELLS_PER_SIDE, ManifestDefinition, ManifestReference, MetatypeManifest,
        custody::{Custody, Gate, Principal, Profile, Transfer, manifest_id},
        witness::{GroupWitnesses, MAGIC},
        workflow::{State, Workflow, witness_proof},
    },
};
use stem_ledger::Cell;

use crate::{
    bridge_cli::{print_json, read_json_limited},
    l1::Rpc,
};

const MAX_CUSTODY_TRANSACTIONS: usize = 64;
const MAX_HEADERS: u64 = 4_096;
const MAX_RAW_TRANSACTIONS: usize = 512;
const MAX_RAW_BYTES: usize = 32 * 1024 * 1024;
const MAX_DEPS: usize = 128;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    escrow_cell: JsonBytes,
    workflow_script_hash: H256,
    import: JsonPoint,
    burn: JsonPoint,
    anchor: H256,
}

pub(super) fn recover(request: &Path, l1_rpc: &str) -> Result<()> {
    let request: Request = read_json_limited(request, 128 * 1024)?;
    let escrow = Cell::from_slice(request.escrow_cell.as_bytes())?;
    let rule: Identity = request.workflow_script_hash.0;
    let script = escrow
        .type_scripts()
        .find(|script| script.calc_script_hash().as_slice() == rule)
        .context("workflow Script is absent from the Stem escrow Cell")?;
    let args = script.args().raw_data();
    let workflow = Workflow::decode(&args).context("invalid workflow Script args")?;
    let route = Route::new(&escrow, &workflow)?;
    let mut history = History::open(l1_rpc, request.anchor, &route.genesis)?;
    let import: OutPoint = request.import.into();
    let burn: OutPoint = request.burn.into();
    let (burn_tx, _) = history.included(&burn.tx_hash())?;
    route.check_transaction(&burn_tx)?;
    ensure!(
        route.unique_output(&burn_tx)?.is_none(),
        "burn recreates a Metatype Cell"
    );
    let (mailbox, mailbox_data) = output(&burn_tx, &burn)?;
    ensure!(
        route.is_mailbox(&mailbox),
        "burn output is outside the pinned mailbox"
    );
    let mailbox = MailboxState::decode(&mailbox_data).context("invalid burn mailbox state")?;
    let message =
        Message::decode(mailbox.outgoing).context("burn mailbox has no return message")?;
    ensure!(
        message.source == route.genesis
            && &message.destination == workflow.network
            && route.mailbox.calc_script_hash().as_slice() == message.sender
            && message.receiver == route.gate.stem_escrow,
        "return message is outside the pinned route"
    );
    let transfer = Transfer::decode(Profile::Revisable, message.payload)
        .context("invalid Metatype return payload")?;
    let Principal::Object(returned) = transfer.principal else {
        unreachable!("object route")
    };
    ensure!(
        transfer.manifest == route.gate.custody.manifest
            && transfer.recipient != route.gate.stem_escrow,
        "return payload has a different Manifest or names the escrow as recipient"
    );
    ensure!(
        proof(&burn_tx, &route.gate.metatype, rule)?.is_empty(),
        "burn carries a workflow update"
    );

    let mut point = route
        .predecessor(&mut history, &burn_tx)?
        .context("burn has no Metatype input")?;
    let mut reverse = Vec::new();
    loop {
        ensure!(
            reverse.len() < MAX_CUSTODY_TRANSACTIONS,
            "CKB object history exceeds transaction limit"
        );
        let (tx, height) = history.included(&point.tx_hash())?;
        route.check_transaction(&tx)?;
        ensure!(
            route.unique_output(&tx)? == Some(u32::from(point.index()) as usize),
            "CKB history does not recreate the exact Metatype output"
        );
        let (_, data) = output(&tx, &point)?;
        let data = route.state(&mut history, &tx, data)?;
        let witness = proof(&tx, &route.gate.metatype, rule)?;
        let step = State::decode(&data, &rule).context("invalid CKB workflow state")?;
        ensure!(!step.is_initial(), "unopened workflow in CKB custody");
        reverse.push((point.clone(), height, step, witness, data));
        let previous = route.predecessor(&mut history, &tx)?;
        if point == import {
            ensure!(
                previous.is_none() && route.consumes_mailbox(&mut history, &tx)?,
                "named import does not introduce the pinned object through its mailbox"
            );
            break;
        }
        point = previous.context("CKB custody path did not reach the named import")?;
    }
    let mut steps = reverse.into_iter().rev();
    let (entry, height, mut state, witness, initial) =
        steps.next().context("empty CKB custody history")?;
    ensure!(
        initial == escrow.data() && witness.is_empty(),
        "CKB import does not match the exact escrowed state"
    );
    let mut trace =
        vec![json!({"out_point": JsonPoint::from(entry), "height": height, "step": false})];
    let mut proof_data = Vec::new();
    let mut last = initial;
    for (point, height, next, witness, data) in steps {
        ensure!(
            workflow.verify_successor(&rule, state, next, &witness, 1, verify_signature),
            "invalid witnessed workflow transition at {point:?}"
        );
        trace.push(json!({"out_point": JsonPoint::from(point), "height": height, "step": !witness.is_empty()}));
        proof_data.extend_from_slice(&witness);
        state = next;
        last = data;
    }
    ensure!(
        last.as_ref() == returned,
        "CKB burn does not commit the last witnessed state"
    );
    history.check_anchor()?;
    print_json(&json!({
        "proof_data": JsonBytes::from_vec(proof_data),
        "record": JsonBytes::from_vec(mailbox.outgoing.to_vec()),
        "state": JsonBytes::from_vec(returned.to_vec()),
        "message_id": H256::from(message.id()),
        "recipient": H256::from(transfer.recipient),
        "anchor": history.anchor,
        "burn": JsonPoint::from(burn),
        "trace": trace,
    }))
}

fn verify_signature(key: &Identity, digest: &Identity, signature: &[u8; 64]) -> bool {
    VerifyingKey::from_bytes(key).is_ok_and(|key| {
        key.verify_strict(digest, &Signature::from_bytes(signature))
            .is_ok()
    })
}

fn proof(tx: &TransactionView, deployment: &Identity, rule: Identity) -> Result<Vec<u8>> {
    let extra = tx
        .witnesses()
        .get(tx.inputs().len())
        .map(|raw| raw.raw_data());
    let group = match extra.as_deref() {
        Some(raw) if raw.starts_with(MAGIC) => GroupWitnesses::decode(raw)
            .context("invalid CKB group witness")?
            .for_group(deployment, rule),
        _ => None,
    };
    Ok(witness_proof(group)
        .context("invalid CKB workflow witness row")?
        .to_vec())
}

fn output(tx: &TransactionView, point: &OutPoint) -> Result<(CellOutput, Bytes)> {
    let index = u32::from(point.index()) as usize;
    Ok((
        tx.output(index).context("CKB OutPoint has no output")?,
        tx.outputs_data()
            .get(index)
            .context("CKB OutPoint has no data")?
            .raw_data(),
    ))
}

struct Route {
    genesis: Identity,
    gate: Gate,
    owner: Byte32,
    mailbox: Script,
}

impl Route {
    fn new(escrow: &Cell, workflow: &Workflow<'_>) -> Result<Self> {
        let lock = escrow.lock();
        ensure!(
            lock.code_hash().as_slice() == workflow.escrow_code
                && u8::from(lock.hash_type()) == ScriptHashType::Data as u8,
            "Stem Cell does not use the workflow's pinned escrow Lock code"
        );
        let args = lock.args().raw_data();
        let (route, custody) = args
            .split_at_checked(4 * 32)
            .context("missing escrow route")?;
        let [genesis, metatype, checkpoint_code, checkpoint_id] = route.as_chunks::<32>().0 else {
            unreachable!("fixed route fields");
        };
        ensure!(
            !route.as_chunks::<32>().0.contains(&[0; 32]),
            "invalid escrow route"
        );
        let custody = Custody::decode(custody).context("invalid escrow custody args")?;
        // The Manifest orders Molecule Script bytes. Stem orders its own encoding.
        let mut scripts: Vec<_> = escrow.type_scripts().collect();
        scripts.sort_unstable_by(|left, right| left.as_slice().cmp(right.as_slice()));
        let ids = scripts
            .iter()
            .map(|script| {
                let logical = LogicalScript::decode(script.as_slice())
                    .context("invalid original Type Script")?;
                ensure!(
                    logical.code().1 == 0,
                    "invalid original Type Script addressing"
                );
                Ok(logical.id())
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            custody.profile == Profile::Revisable
                && &custody.data_script == workflow.object
                && ids.contains(&custody.data_script)
                && manifest_id(&ids) == custody.manifest,
            "Stem Cell does not match the workflow custody Manifest"
        );
        let gate = Gate {
            stem_escrow: lock.calc_script_hash().unpack(),
            metatype: *metatype,
            custody,
        };
        let owner = data2(custody.guard_code, &gate.args()).calc_script_hash();
        let args = [
            custody.mailbox_id,
            *genesis,
            *workflow.network,
            owner.unpack(),
            *checkpoint_code,
            *checkpoint_id,
        ];
        let mailbox = data2(custody.mailbox_code, args.as_flattened());
        Ok(Self {
            genesis: *genesis,
            gate,
            owner,
            mailbox,
        })
    }

    fn check_transaction(&self, tx: &TransactionView) -> Result<()> {
        ensure!(
            tx.inputs().len() <= MAX_TRANSACTION_CELLS_PER_SIDE
                && tx.outputs().len() <= MAX_TRANSACTION_CELLS_PER_SIDE
                && tx
                    .header_deps()
                    .get(0)
                    .is_some_and(|genesis| genesis.as_slice() == self.genesis),
            "CKB transaction is outside the deployment bounds or genesis"
        );
        Ok(())
    }

    fn is_mailbox(&self, cell: &CellOutput) -> bool {
        cell.type_().to_opt().as_ref() == Some(&self.mailbox)
            && cell.lock().calc_script_hash() == self.owner
    }

    fn unique_output(&self, tx: &TransactionView) -> Result<Option<usize>> {
        let mut result = None;
        for (index, cell) in tx.outputs().into_iter().enumerate() {
            if has_type(&cell, &self.gate.metatype) {
                ensure!(result.replace(index).is_none(), "multiple Metatype outputs");
            }
        }
        Ok(result)
    }

    fn predecessor(&self, history: &mut History, tx: &TransactionView) -> Result<Option<OutPoint>> {
        let mut result = None;
        for input in tx.inputs() {
            let point = input.previous_output();
            let cell = history.referenced(&point)?.0;
            if has_type(&cell, &self.gate.metatype) {
                ensure!(result.replace(point).is_none(), "multiple Metatype inputs");
            }
        }
        Ok(result)
    }

    fn consumes_mailbox(&self, history: &mut History, tx: &TransactionView) -> Result<bool> {
        for input in tx.inputs() {
            if self.is_mailbox(&history.referenced(&input.previous_output())?.0) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn state(&self, history: &mut History, tx: &TransactionView, data: Bytes) -> Result<Bytes> {
        let state = |manifest: Option<MetatypeManifest<'_>>| {
            let manifest = manifest.context("invalid CKB Metatype Manifest")?;
            self.gate
                .custody
                .data(&manifest)
                .map(Bytes::copy_from_slice)
                .context("CKB state differs from the complete original Manifest")
        };
        if data.first() != Some(&0) {
            return state(MetatypeManifest::decode(&data));
        }
        let reference =
            ManifestReference::decode(&data).context("invalid shared Manifest reference")?;
        let definition = history.definition(tx, &reference.definition_hash)?;
        state(reference.view(&definition, MAX_MANIFEST_BYTES))
    }
}

fn has_type(cell: &CellOutput, hash: &Identity) -> bool {
    cell.type_()
        .to_opt()
        .is_some_and(|ty| ty.calc_script_hash().as_slice() == hash)
}

fn data2(code: Identity, args: &[u8]) -> Script {
    Script::new_builder()
        .code_hash(code.pack())
        .hash_type(ScriptHashType::Data2)
        .args(args.pack())
        .build()
}

#[derive(Clone)]
struct RawTransaction {
    tx: TransactionView,
    height: u64,
    block: H256,
}

struct History {
    rpc: Rpc,
    anchor: H256,
    anchor_height: u64,
    oldest: HeaderView,
    raw: HashMap<Byte32, RawTransaction>,
    raw_bytes: usize,
}

impl History {
    fn open(url: &str, anchor: H256, genesis: &Identity) -> Result<Self> {
        let rpc = Rpc::new(url)?;
        ensure!(
            rpc.block_hash(0)?.as_bytes() == genesis,
            "CKB RPC genesis differs from the route"
        );
        let oldest = Self::header(&rpc, &anchor)?;
        let anchor_height = oldest.number();
        let history = Self {
            rpc,
            anchor,
            anchor_height,
            oldest,
            raw: HashMap::new(),
            raw_bytes: 0,
        };
        history.check_anchor()?;
        Ok(history)
    }

    fn check_anchor(&self) -> Result<()> {
        ensure!(
            self.rpc.block_hash(self.anchor_height)? == self.anchor,
            "CKB RPC chain diverges at the trusted anchor"
        );
        Ok(())
    }

    fn header(rpc: &Rpc, hash: &H256) -> Result<HeaderView> {
        let header: JsonHeader = rpc
            .call::<Option<_>>("get_header", json!([hash]))?
            .context("CKB ancestor Header is unavailable")?;
        let header: HeaderView = header.into();
        ensure!(
            header.data().calc_header_hash().as_slice() == hash.as_bytes(),
            "CKB Header hash differs"
        );
        Ok(header)
    }

    fn authenticate_block(&mut self, height: u64, hash: &H256) -> Result<HeaderView> {
        let depth = self
            .anchor_height
            .checked_sub(height)
            .context("CKB transaction is after the trusted anchor")?;
        ensure!(
            depth <= MAX_HEADERS,
            "CKB history exceeds the supported anchor distance"
        );
        ensure!(
            height <= self.oldest.number(),
            "CKB custody heights are reversed"
        );
        while self.oldest.number() > height {
            let parent = Self::header(&self.rpc, &self.oldest.parent_hash().unpack())?;
            ensure!(
                parent.number().checked_add(1) == Some(self.oldest.number()),
                "CKB ancestor height differs"
            );
            self.oldest = parent;
        }
        ensure!(
            self.oldest.hash().as_slice() == hash.as_bytes(),
            "CKB transaction is outside the trusted anchor ancestry"
        );
        Ok(self.oldest.clone())
    }

    fn raw(&mut self, hash: &Byte32) -> Result<RawTransaction> {
        if let Some(tx) = self.raw.get(hash) {
            return Ok(tx.clone());
        }
        ensure!(
            self.raw.len() < MAX_RAW_TRANSACTIONS,
            "CKB history lookup limit reached"
        );
        let (raw, height, block) = self.rpc.committed(&hash.unpack())?;
        self.raw_bytes = self
            .raw_bytes
            .checked_add(raw.as_bytes().len())
            .context("CKB history size overflow")?;
        ensure!(
            self.raw_bytes <= MAX_RAW_BYTES,
            "CKB history data exceeds lookup limit"
        );
        let tx = Transaction::from_slice(raw.as_bytes())?.into_view();
        let value = RawTransaction { tx, height, block };
        self.raw.insert(hash.clone(), value.clone());
        Ok(value)
    }

    fn referenced(&mut self, point: &OutPoint) -> Result<(CellOutput, Bytes)> {
        output(&self.raw(&point.tx_hash())?.tx, point)
    }

    fn included(&mut self, hash: &Byte32) -> Result<(TransactionView, u64)> {
        let raw = self.raw(hash)?;
        let header = self.authenticate_block(raw.height, &raw.block)?;
        let tx_hash: H256 = hash.unpack();
        let merkle = self
            .rpc
            .call("get_transaction_and_witness_proof", json!([[tx_hash]]))?;
        stem::l1_proof::verify_transaction(&header.hash(), &header, &raw.tx, &merkle)?;
        Ok((raw.tx, raw.height))
    }

    fn definition(&mut self, tx: &TransactionView, hash: &Identity) -> Result<ManifestDefinition> {
        ensure!(tx.cell_deps().len() <= MAX_DEPS, "too many CKB Cell Deps");
        let mut count = 0;
        let correct = |data: &[u8]| {
            data.len() <= MAX_MANIFEST_DEFINITION_BYTES && ckb_hash::blake2b_256(data) == *hash
        };
        for dep in tx.cell_deps() {
            let data = self.referenced(&dep.out_point())?.1;
            match DepType::try_from(dep.dep_type())? {
                DepType::Code => {
                    count += 1;
                    ensure!(count <= MAX_DEPS, "too many resolved CKB Cell Deps");
                    if correct(&data) {
                        return ManifestDefinition::decode(data.to_vec())
                            .context("invalid shared definition");
                    }
                }
                DepType::DepGroup => {
                    let members = OutPointVec::from_slice(&data)?;
                    count += members.len();
                    ensure!(count <= MAX_DEPS, "too many resolved CKB Cell Deps");
                    for point in members {
                        let data = self.referenced(&point)?.1;
                        if correct(&data) {
                            return ManifestDefinition::decode(data.to_vec())
                                .context("invalid shared definition");
                        }
                    }
                }
            }
        }
        anyhow::bail!("CKB Cell Deps contain no matching shared Manifest definition")
    }
}
