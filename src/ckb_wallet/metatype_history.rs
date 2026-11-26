//! Shared authentication of Metatype custody history against a trusted CKB anchor.

use std::collections::HashMap;

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::HeaderView as JsonHeader;
use ckb_types::{
    H256,
    bytes::Bytes,
    core::{DepType, HeaderView, ScriptHashType, TransactionView},
    packed::{Byte32, CellOutput, OutPoint, OutPointVec, Script, Transaction},
    prelude::*,
};
use serde_json::json;
use stem_bridge_wire::{
    intercell::{Identity, MailboxState, Message},
    metatype::{
        LogicalScript, MAX_MANIFEST_BYTES, MAX_MANIFEST_DEFINITION_BYTES,
        MAX_TRANSACTION_CELLS_PER_SIDE, ManifestDefinition, ManifestReference, MetatypeManifest,
        custody::{EscrowArgs, Gate, Principal, Profile, Transfer, manifest_id},
    },
};
use stem_ledger::Cell;

use crate::l1::Rpc;

const MAX_HEADERS: u64 = 4_096;
const MAX_RAW_TRANSACTIONS: usize = 512;
const MAX_RAW_BYTES: usize = 32 * 1024 * 1024;
const MAX_DEPS: usize = 128;

pub(super) fn output(tx: &TransactionView, point: &OutPoint) -> Result<(CellOutput, Bytes)> {
    let index = u32::from(point.index()) as usize;
    Ok((
        tx.output(index).context("CKB OutPoint has no output")?,
        tx.outputs_data()
            .get(index)
            .context("CKB OutPoint has no data")?
            .raw_data(),
    ))
}

pub(super) struct Route {
    pub genesis: Identity,
    pub gate: Gate,
    network: Identity,
    owner: Byte32,
    mailbox: Script,
}

impl Route {
    pub(super) fn new(escrow: &Cell, network: &Identity, object: &Identity) -> Result<Self> {
        let lock = escrow.lock();
        ensure!(
            u8::from(lock.hash_type()) == ScriptHashType::Data as u8,
            "Stem escrow Lock must use Data addressing"
        );
        let args = lock.args().raw_data();
        let EscrowArgs {
            ckb: genesis,
            metatype,
            checkpoint_code,
            checkpoint_id,
            custody,
        } = EscrowArgs::decode(&args).context("invalid escrow route and custody args")?;
        ensure!(
            network != &[0; 32] && network != genesis,
            "invalid Stem network identity"
        );
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
                && &custody.data_script == object
                && ids.contains(&custody.data_script)
                && manifest_id(&ids) == custody.manifest,
            "Stem Cell does not match the object custody Manifest"
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
            *network,
            owner.unpack(),
            *checkpoint_code,
            *checkpoint_id,
        ];
        let mailbox = data2(custody.mailbox_code, args.as_flattened());
        Ok(Self {
            genesis: *genesis,
            gate,
            network: *network,
            owner,
            mailbox,
        })
    }

    pub(super) fn burn(
        &self,
        history: &mut History,
        point: &OutPoint,
    ) -> Result<(TransactionView, Bytes)> {
        let (burn, _) = history.included(&point.tx_hash())?;
        self.check_transaction(&burn)?;
        ensure!(
            self.unique_output(&burn)?.is_none(),
            "burn recreates a Metatype Cell"
        );
        let (mailbox, data) = output(&burn, point)?;
        ensure!(
            self.is_mailbox(&mailbox),
            "burn output is outside the pinned mailbox"
        );
        ensure!(
            self.consumes_mailbox(history, &burn)?,
            "burn has no pinned mailbox input"
        );
        Ok((burn, data))
    }

    pub(super) fn returned<'a>(&self, data: &'a [u8]) -> Result<Return<'a>> {
        let mailbox = MailboxState::decode(data).context("invalid burn mailbox state")?;
        let message =
            Message::decode(mailbox.outgoing).context("burn mailbox has no return message")?;
        ensure!(
            message.source == self.genesis
                && message.destination == self.network
                && self.mailbox.calc_script_hash().as_slice() == message.sender
                && message.receiver == self.gate.stem_escrow,
            "return message is outside the pinned route"
        );
        let transfer = Transfer::decode(Profile::Revisable, message.payload)
            .context("invalid Metatype return payload")?;
        let Principal::Object(state) = transfer.principal else {
            unreachable!("object route")
        };
        ensure!(
            transfer.manifest == self.gate.custody.manifest
                && transfer.recipient != self.gate.stem_escrow,
            "return payload has a different Manifest or names the escrow as recipient"
        );
        Ok(Return {
            record: mailbox.outgoing,
            message_id: message.id(),
            recipient: transfer.recipient,
            state,
        })
    }

    pub(super) fn check_transaction(&self, tx: &TransactionView) -> Result<()> {
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

    pub(super) fn unique_output(&self, tx: &TransactionView) -> Result<Option<usize>> {
        let mut result = None;
        for (index, cell) in tx.outputs().into_iter().enumerate() {
            if has_type(&cell, &self.gate.metatype) {
                ensure!(result.replace(index).is_none(), "multiple Metatype outputs");
            }
        }
        Ok(result)
    }

    pub(super) fn predecessor(
        &self,
        history: &mut History,
        tx: &TransactionView,
    ) -> Result<Option<OutPoint>> {
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

    pub(super) fn consumes_mailbox(
        &self,
        history: &mut History,
        tx: &TransactionView,
    ) -> Result<bool> {
        for input in tx.inputs() {
            if self.is_mailbox(&history.referenced(&input.previous_output())?.0) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(super) fn state(
        &self,
        history: &mut History,
        tx: &TransactionView,
        data: Bytes,
    ) -> Result<Bytes> {
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

pub(super) struct Return<'a> {
    pub record: &'a [u8],
    pub message_id: Identity,
    pub recipient: Identity,
    pub state: &'a [u8],
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

pub(super) struct History {
    rpc: Rpc,
    pub anchor: H256,
    anchor_height: u64,
    oldest: HeaderView,
    raw: HashMap<Byte32, RawTransaction>,
    raw_bytes: usize,
}

impl History {
    pub(super) fn open(url: &str, anchor: H256, genesis: &Identity) -> Result<Self> {
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

    pub(super) fn check_anchor(&self) -> Result<()> {
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

    pub(super) fn included(&mut self, hash: &Byte32) -> Result<(TransactionView, u64)> {
        let raw = self.raw(hash)?;
        let header = self.authenticate_block(raw.height, &raw.block)?;
        let tx_hash: H256 = hash.unpack();
        let merkle = self
            .rpc
            .call("get_transaction_and_witness_proof", json!([[tx_hash]]))?;
        stem::l1_proof::verify_transaction(&header.hash(), &header, &raw.tx, &merkle)?;
        Ok((raw.tx, raw.height))
    }

    pub(super) fn dependencies(
        &mut self,
        tx: &TransactionView,
    ) -> Result<Vec<(CellOutput, Bytes)>> {
        ensure!(tx.cell_deps().len() <= MAX_DEPS, "too many CKB Cell Deps");
        let mut dependencies = Vec::new();
        for dep in tx.cell_deps() {
            let cell = self.referenced(&dep.out_point())?;
            match DepType::try_from(dep.dep_type())? {
                DepType::Code => {
                    ensure!(
                        dependencies.len() < MAX_DEPS,
                        "too many resolved CKB Cell Deps"
                    );
                    dependencies.push(cell);
                }
                DepType::DepGroup => {
                    let members = OutPointVec::from_slice(&cell.1)?;
                    ensure!(
                        members.len() <= MAX_DEPS - dependencies.len(),
                        "too many resolved CKB Cell Deps"
                    );
                    for point in members {
                        dependencies.push(self.referenced(&point)?);
                    }
                }
            }
        }
        Ok(dependencies)
    }

    fn definition(&mut self, tx: &TransactionView, hash: &Identity) -> Result<ManifestDefinition> {
        let (_, data) = self
            .dependencies(tx)?
            .into_iter()
            .find(|(_, data)| {
                data.len() <= MAX_MANIFEST_DEFINITION_BYTES && ckb_hash::blake2b_256(data) == *hash
            })
            .context("CKB Cell Deps contain no matching shared Manifest definition")?;
        ManifestDefinition::decode(data.to_vec()).context("invalid shared definition")
    }
}
