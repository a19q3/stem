use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_types::{
    bytes::Bytes,
    core::{DepType, ScriptHashType},
    packed::{Byte32, CellDep, OutPoint, Script},
    prelude::*,
};
use stem_ledger::{
    Cell, CellInput, GroupAction, GroupMessage, GroupWitness, Since, Transaction,
    light::VerifiedHeader, record,
};
use wasm_bindgen::prelude::*;

use crate::{AuthenticatedHeader, js, verify_cell};

#[wasm_bindgen(js_name = packScript)]
pub fn pack_script(code_hash: &[u8], by_type: bool, args: &[u8]) -> Result<Vec<u8>, JsError> {
    js((|| {
        let script = Script::new_builder()
            .code_hash(Byte32::from_slice(code_hash)?)
            .hash_type(if by_type {
                ScriptHashType::Type
            } else {
                ScriptHashType::Data
            })
            .args(args.pack())
            .build();
        Ok(script.as_slice().to_vec())
    })())
}

#[wasm_bindgen]
pub struct CellBuilder {
    capacity: u64,
    lock: Script,
    types: Vec<Script>,
    data: Bytes,
}

#[wasm_bindgen]
impl CellBuilder {
    #[wasm_bindgen(constructor)]
    pub fn new(capacity: u64, lock: &[u8], data: &[u8]) -> Result<CellBuilder, JsError> {
        js((|| {
            Ok(Self {
                capacity,
                lock: Script::from_slice(lock)?,
                types: Vec::new(),
                data: Bytes::copy_from_slice(data),
            })
        })())
    }

    #[wasm_bindgen(js_name = addTypeScript)]
    pub fn add_type_script(&mut self, script: &[u8]) -> Result<(), JsError> {
        js((|| {
            self.types.push(Script::from_slice(script)?);
            Ok(())
        })())
    }

    pub fn build(&self) -> Result<DecodedCell, JsError> {
        js(Cell::new(
            self.capacity,
            self.lock.clone(),
            self.types.clone(),
            self.data.clone(),
        )
        .map(DecodedCell))
    }
}

#[wasm_bindgen(js_name = decodeCell)]
pub fn decode_cell(raw: &[u8]) -> Result<DecodedCell, JsError> {
    js(Cell::from_slice(raw).map(DecodedCell))
}

#[wasm_bindgen(js_name = decodeLiveCell)]
pub fn decode_live_cell(raw: &[u8]) -> Result<DecodedCell, JsError> {
    js(record::decode_cell(raw).map(|(_, record)| DecodedCell(record.cell)))
}

#[wasm_bindgen]
pub struct DecodedCell(Cell);

#[wasm_bindgen]
impl DecodedCell {
    #[wasm_bindgen(getter)]
    pub fn capacity(&self) -> u64 {
        self.0.capacity()
    }

    #[wasm_bindgen(getter, js_name = typeScriptCount)]
    pub fn type_script_count(&self) -> usize {
        self.0.type_scripts().len()
    }

    pub fn encode(&self) -> Vec<u8> {
        self.0.as_slice().to_vec()
    }

    pub fn lock(&self) -> Vec<u8> {
        self.0.lock().as_slice().to_vec()
    }

    pub fn data(&self) -> Vec<u8> {
        self.0.data().to_vec()
    }

    #[wasm_bindgen(js_name = typeScript)]
    pub fn type_script(&self, index: usize) -> Option<Vec<u8>> {
        self.0
            .type_scripts()
            .nth(index)
            .map(|script| script.as_slice().to_vec())
    }
}

#[wasm_bindgen]
pub enum SinceKind {
    AbsoluteHeight,
    RelativeBlocks,
    AbsoluteTime,
    RelativeTime,
}

#[derive(Default)]
#[wasm_bindgen]
pub struct TransactionBuilder {
    inputs: Vec<CellInput>,
    outputs: Vec<Cell>,
    deps: Vec<CellDep>,
    headers: Vec<Byte32>,
    messages: BTreeMap<u16, Vec<GroupAction>>,
}

#[wasm_bindgen]
impl TransactionBuilder {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    #[wasm_bindgen(js_name = addInput)]
    pub fn add_input(&mut self, point: &[u8]) -> Result<(), JsError> {
        js(self.push_input(point, None))
    }

    #[wasm_bindgen(js_name = addLockedInput)]
    pub fn add_locked_input(
        &mut self,
        point: &[u8],
        kind: SinceKind,
        value: u64,
    ) -> Result<(), JsError> {
        let since = match kind {
            SinceKind::AbsoluteHeight => Since::AbsoluteHeight(value),
            SinceKind::RelativeBlocks => Since::RelativeBlocks(value),
            SinceKind::AbsoluteTime => Since::AbsoluteTime(value),
            SinceKind::RelativeTime => Since::RelativeTime(value),
        };
        js(self.push_input(point, Some(since)))
    }

    #[wasm_bindgen(js_name = addOutput)]
    pub fn add_output(&mut self, cell: &DecodedCell) {
        self.outputs.push(cell.0.clone());
    }

    #[wasm_bindgen(js_name = addCellDep)]
    pub fn add_cell_dep(&mut self, point: &[u8], dep_group: bool) -> Result<(), JsError> {
        js((|| {
            self.deps.push(
                CellDep::new_builder()
                    .out_point(OutPoint::from_slice(point)?)
                    .dep_type(if dep_group {
                        DepType::DepGroup
                    } else {
                        DepType::Code
                    })
                    .build(),
            );
            Ok(())
        })())
    }

    #[wasm_bindgen(js_name = addHeaderDep)]
    pub fn add_header_dep(&mut self, id: &[u8]) -> Result<(), JsError> {
        js((|| {
            self.headers.push(Byte32::from_slice(id)?);
            Ok(())
        })())
    }

    #[wasm_bindgen(js_name = addAction)]
    pub fn add_action(&mut self, group_index: u16, kind: u16, data: &[u8]) {
        self.messages
            .entry(group_index)
            .or_default()
            .push(GroupAction {
                kind,
                data: Bytes::copy_from_slice(data),
            });
    }

    pub fn build(&self, valid_before_height: Option<u64>) -> Result<DecodedTransaction, JsError> {
        js((|| {
            let messages = self
                .messages
                .iter()
                .map(|(&group_index, actions)| GroupMessage {
                    group_index,
                    actions: actions.clone(),
                })
                .collect::<Vec<_>>();
            let mut tx = Transaction::new(
                self.deps.clone(),
                self.headers.clone(),
                self.inputs.clone(),
                self.outputs.clone(),
                Vec::new(),
            )?;
            if !messages.is_empty() {
                tx = tx.with_messages(messages)?;
            }
            if let Some(height) = valid_before_height {
                tx = tx.with_valid_before_height(height)?;
            }
            Ok(DecodedTransaction(tx))
        })())
    }
}

impl TransactionBuilder {
    fn push_input(&mut self, point: &[u8], since: Option<Since>) -> Result<()> {
        self.inputs
            .push(CellInput::new(OutPoint::from_slice(point)?, since));
        Ok(())
    }
}

#[wasm_bindgen(js_name = decodeTransaction)]
pub fn decode_transaction(raw: &[u8]) -> Result<DecodedTransaction, JsError> {
    js(Transaction::from_slice(raw).map(DecodedTransaction))
}

#[wasm_bindgen]
pub struct DecodedTransaction(Transaction);

#[wasm_bindgen]
impl DecodedTransaction {
    #[wasm_bindgen(getter, js_name = inputCount)]
    pub fn input_count(&self) -> usize {
        self.0.inputs().len()
    }

    #[wasm_bindgen(getter, js_name = outputCount)]
    pub fn output_count(&self) -> usize {
        self.0.outputs().len()
    }

    pub fn input(&self, index: usize) -> Option<Vec<u8>> {
        self.0
            .input(index)
            .map(|input| input.previous_output.as_slice().to_vec())
    }

    pub fn output(&self, index: usize) -> Option<DecodedCell> {
        self.0.outputs().nth(index).map(DecodedCell)
    }

    pub fn encode(&self) -> Vec<u8> {
        self.0.as_slice().to_vec()
    }

    pub fn id(&self, network: &[u8]) -> Result<Vec<u8>, JsError> {
        js((|| {
            Ok(self.0.id(&Byte32::from_slice(network)?).as_slice().to_vec())
        })())
    }

    #[wasm_bindgen(js_name = withProofData)]
    pub fn with_proof_data(
        &self,
        group_index: u16,
        proof: &[u8],
    ) -> Result<DecodedTransaction, JsError> {
        js((|| {
            ensure!(
                self.0.witnesses().all(|witness| witness.seal.is_empty()),
                "set proof data before collecting seals"
            );
            self.with_witness(GroupWitness {
                group_index,
                proof_data: Bytes::copy_from_slice(proof),
                seal: Bytes::new(),
            })
        })())
    }

    pub fn seal(&self, group_index: u16) -> Option<Vec<u8>> {
        self.0
            .witness(group_index)
            .map(|witness| witness.seal.to_vec())
    }

    #[wasm_bindgen(js_name = withSeal)]
    pub fn with_seal(&self, group_index: u16, seal: &[u8]) -> Result<DecodedTransaction, JsError> {
        js(self.with_witness(GroupWitness {
            group_index,
            proof_data: self
                .0
                .witness(group_index)
                .map_or_else(Bytes::new, |witness| witness.proof_data),
            seal: Bytes::copy_from_slice(seal),
        }))
    }
}

impl DecodedTransaction {
    fn with_witness(&self, witness: GroupWitness) -> Result<Self> {
        let mut witnesses: BTreeMap<_, _> =
            self.0.witnesses().map(|w| (w.group_index, w)).collect();
        if witness.proof_data.is_empty() && witness.seal.is_empty() {
            witnesses.remove(&witness.group_index);
        } else {
            witnesses.insert(witness.group_index, witness);
        }
        Ok(Self(
            self.0.with_witnesses(witnesses.into_values().collect())?,
        ))
    }
}

#[wasm_bindgen]
pub struct SigningContext {
    tip: VerifiedHeader,
    cells: BTreeMap<OutPoint, Cell>,
    headers: BTreeMap<Byte32, VerifiedHeader>,
}

#[wasm_bindgen]
impl SigningContext {
    #[wasm_bindgen(constructor)]
    pub fn new(tip: &AuthenticatedHeader) -> Self {
        let tip = tip.0.clone();
        Self {
            headers: BTreeMap::from([(tip.header().id(tip.network()), tip.clone())]),
            tip,
            cells: BTreeMap::new(),
        }
    }

    #[wasm_bindgen(js_name = addLiveCell)]
    pub fn add_live_cell(&mut self, raw: &[u8], proof: &[u8]) -> Result<(), JsError> {
        js((|| {
            let (point, cell) = record::decode_cell(raw)?;
            ensure!(
                verify_cell(&self.tip, point.as_slice(), raw, proof)?,
                "signing input must be live"
            );
            self.cells.insert(point, cell.cell);
            Ok(())
        })())
    }

    #[wasm_bindgen(js_name = addHeader)]
    pub fn add_header(&mut self, header: &AuthenticatedHeader) -> Result<(), JsError> {
        js((|| {
            ensure!(
                self.tip.same_trust_anchor(&header.0)
                    && header.height() <= self.tip.header().context.height,
                "Header Dep belongs to a different trust anchor or follows the signing state"
            );
            self.headers
                .insert(header.0.header().id(self.tip.network()), header.0.clone());
            Ok(())
        })())
    }

    #[wasm_bindgen(js_name = signingDigest)]
    pub fn signing_digest(
        &self,
        tx: &DecodedTransaction,
        index: u16,
        lock: &[u8],
    ) -> Result<Vec<u8>, JsError> {
        js((|| {
            let lock = Script::from_slice(lock)?;
            let digest = tx.0.signing_digest(
                self.tip.network(),
                index,
                &lock,
                |point| {
                    self.cells
                        .get(point)
                        .cloned()
                        .context("missing proven signing Cell")
                },
                |id| {
                    self.headers
                        .get(id)
                        .map(|header| header.header().clone())
                        .context("missing authenticated signing Header")
                },
            )?;
            Ok(digest.as_slice().to_vec())
        })())
    }
}
