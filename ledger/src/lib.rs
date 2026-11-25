use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::{Arc, OnceLock},
};

use anyhow::{Context, Result, bail, ensure};

use crate::protocol::{Certificate, Operation};
use ckb_hash::{blake2b_256, new_blake2b};
use ckb_types::{
    bytes::Bytes,
    core::{DepType, ScriptHashType},
    packed::{Byte32, CellDep, OutPoint, Script},
    prelude::*,
};

#[cfg(test)]
mod encoding_tests;

pub mod block;
pub mod history;
pub mod patricia;
pub mod proof;
pub mod protocol;
pub mod record;
pub mod replay;
pub mod time;
mod witness;

pub use witness::GroupWitness;

use block::Header;
pub use time::{BlockContext, CellInput, Origin, Since};

mod wire {
    #![allow(dead_code)]
    include!(concat!(env!("OUT_DIR"), "/ledger.rs"));
}

pub const SHANNONS_PER_BYTE: u64 = 100_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacityPolicy {
    pub bond_per_byte: u64,
    pub state_overhead: u64,
}

impl CapacityPolicy {
    pub fn bond(self, encoded_len: usize) -> Result<u64> {
        ensure!(self.bond_per_byte > 0, "zero Storage Bond price");
        u64::try_from(encoded_len)?
            .checked_add(self.state_overhead)
            .and_then(|bytes| bytes.checked_mul(self.bond_per_byte))
            .context("occupied Capacity overflow")
    }

    pub fn verify(self, cell: &Cell) -> Result<()> {
        ensure!(
            cell.capacity() >= self.bond(cell.as_slice().len())?,
            "insufficient occupied Capacity"
        );
        Ok(())
    }
}

impl Default for CapacityPolicy {
    fn default() -> Self {
        Self {
            bond_per_byte: SHANNONS_PER_BYTE,
            state_overhead: 0,
        }
    }
}
pub const MAX_TYPE_SCRIPTS: usize = 32;
pub const MAX_SCRIPT_GROUPS: usize = 8_192;
pub const MAX_SCRIPT_BYTES: usize = 16 * 1024;
pub const MAX_TYPE_SCRIPT_BYTES: usize = 64 * 1024;
pub const MAX_TRANSACTION_BYTES: usize = 512 * 1024;
// Genesis may carry the 1 MiB secp256k1 precomputation table plus Cell overhead.
// An ordinary transaction still has to fit the transaction-wide byte limit.
pub const MAX_CELL_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_CELL_REFERENCES: usize = 8_192;
pub const MAX_RESOLVED_CELL_BYTES: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Cell {
    encoded: wire::Cell,
    hashes: Arc<OnceLock<CellHashes>>,
}

#[derive(Debug)]
struct CellHashes {
    content: Byte32,
    data: Byte32,
}

impl Cell {
    pub fn new(
        capacity: u64,
        lock: Script,
        mut type_scripts: Vec<Script>,
        data: Bytes,
    ) -> Result<Self> {
        ensure!(
            type_scripts.len() <= MAX_TYPE_SCRIPTS,
            "too many Type Scripts"
        );
        validate_script(&lock)?;
        let mut script_bytes = 0;
        for script in &type_scripts {
            validate_script(script)?;
            script_bytes += script.as_slice().len();
        }
        ensure!(
            script_bytes <= MAX_TYPE_SCRIPT_BYTES,
            "Type Script set exceeds byte limit"
        );
        ensure!(data.len() <= MAX_CELL_BYTES, "Cell Data exceeds byte limit");
        type_scripts.sort_unstable_by(|left, right| left.as_slice().cmp(right.as_slice()));

        let encoded = wire::Cell::new_builder()
            .capacity(wire::Uint64::new_unchecked(Bytes::copy_from_slice(
                &capacity.to_le_bytes(),
            )))
            .lock(wire::Script::new_unchecked(lock.as_bytes()))
            .type_scripts(
                wire::ScriptVec::new_builder()
                    .set(
                        type_scripts
                            .into_iter()
                            .map(|script| wire::Script::new_unchecked(script.as_bytes()))
                            .collect(),
                    )
                    .build(),
            )
            .data(wire::Bytes::new_unchecked(data.pack().as_bytes()))
            .build();
        Self::from_entity(encoded)
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_CELL_BYTES, "Cell exceeds byte limit");
        Self::from_entity(wire::Cell::from_slice(bytes)?)
    }

    fn validated_entity(encoded: wire::Cell) -> Self {
        Self {
            encoded,
            hashes: Arc::default(),
        }
    }

    fn from_entity(encoded: wire::Cell) -> Result<Self> {
        let cell = Self::validated_entity(encoded);
        ensure!(
            cell.as_slice().len() <= MAX_CELL_BYTES,
            "Cell exceeds byte limit"
        );
        validate_script(&cell.lock())?;
        let types = cell.encoded.type_scripts();
        ensure!(types.len() <= MAX_TYPE_SCRIPTS, "too many Type Scripts");
        let mut previous: Option<wire::Script> = None;
        let mut script_bytes = 0;
        for current in types {
            validate_script(&Script::new_unchecked(current.as_bytes()))?;
            script_bytes += current.as_slice().len();
            ensure!(
                previous
                    .as_ref()
                    .is_none_or(|last| last.as_slice() < current.as_slice()),
                "Type Script set is out of order or contains a duplicate"
            );
            previous = Some(current);
        }
        ensure!(
            script_bytes <= MAX_TYPE_SCRIPT_BYTES,
            "Type Script set exceeds byte limit"
        );
        Ok(cell)
    }

    pub fn as_slice(&self) -> &[u8] {
        self.encoded.as_slice()
    }

    pub fn capacity(&self) -> u64 {
        u64::from_le_bytes(
            self.encoded
                .capacity()
                .as_slice()
                .try_into()
                .expect("Uint64"),
        )
    }

    pub fn lock(&self) -> Script {
        Script::new_unchecked(self.encoded.lock().as_bytes())
    }

    pub fn type_scripts(&self) -> impl ExactSizeIterator<Item = Script> {
        self.encoded
            .type_scripts()
            .into_iter()
            .map(|item| Script::new_unchecked(item.as_bytes()))
    }

    pub fn type_script(&self, index: usize) -> Option<Script> {
        self.encoded
            .type_scripts()
            .get(index)
            .map(|script| Script::new_unchecked(script.as_bytes()))
    }

    pub fn type_scripts_bytes(&self) -> Bytes {
        self.encoded.type_scripts().as_bytes()
    }

    pub fn data(&self) -> Bytes {
        self.encoded.data().raw_data()
    }

    fn hashes(&self) -> &CellHashes {
        self.hashes.get_or_init(|| CellHashes {
            content: blake2b_256(self.as_slice()).pack(),
            data: blake2b_256(self.data()).pack(),
        })
    }

    pub fn data_hash(&self) -> &Byte32 {
        &self.hashes().data
    }

    pub fn occupied_capacity(&self) -> Result<u64> {
        CapacityPolicy::default().bond(self.as_slice().len())
    }

    pub fn minimum_capacity(
        lock: &Script,
        type_scripts: &[Script],
        data_len: usize,
    ) -> Result<u64> {
        // Four field offsets, one table size, and the fixed-width Capacity.
        let mut bytes = 5 * 4 + 8;
        bytes += lock.as_slice().len();
        // One vector size for the scripts, plus each item offset and encoded script.
        bytes += 4;
        for script in type_scripts {
            bytes = bytes
                .checked_add(4 + script.as_slice().len())
                .context("Cell size overflow")?;
        }
        // The Bytes vector has a length prefix followed by the raw data.
        CapacityPolicy::default().bond(
            bytes
                .checked_add(4)
                .and_then(|size| size.checked_add(data_len))
                .context("Cell size overflow")?,
        )
    }
}

fn validate_script(script: &Script) -> Result<()> {
    ensure!(
        script.as_slice().len() <= MAX_SCRIPT_BYTES,
        "Script exceeds byte limit"
    );
    match ScriptHashType::try_from(script.hash_type())? {
        ScriptHashType::Data | ScriptHashType::Type => Ok(()),
        _ => bail!("unsupported Script hash type"),
    }
}

#[derive(Clone, Debug)]
pub struct Transaction {
    encoded: wire::Transaction,
}

impl Transaction {
    pub fn new(
        cell_deps: Vec<CellDep>,
        header_deps: Vec<Byte32>,
        inputs: Vec<CellInput>,
        outputs: Vec<Cell>,
        witnesses: Vec<GroupWitness>,
    ) -> Result<Self> {
        Self::build(cell_deps, header_deps, inputs, outputs, witnesses, None)
    }

    /// Attach the mint statement before signing any Cell inputs.
    pub fn with_bridge_mint(
        &self,
        mint: protocol::BridgeMint,
        certificate: Certificate,
    ) -> Result<Self> {
        ensure!(
            self.operation().is_none() && self.witnesses().len() == 0,
            "mint requires an unsigned ordinary transaction"
        );
        Self::build(
            self.cell_deps().collect(),
            self.header_deps().collect(),
            self.inputs().collect(),
            self.outputs().collect(),
            self.witnesses().collect(),
            Some((Operation::BridgeMint(mint), certificate)),
        )
    }

    pub fn bridge_mint(
        mint: protocol::BridgeMint,
        certificate: Certificate,
        cell_deps: Vec<CellDep>,
        outputs: Vec<Cell>,
    ) -> Result<Self> {
        Self::build(
            cell_deps,
            Vec::new(),
            Vec::new(),
            outputs,
            Vec::new(),
            Some((Operation::BridgeMint(mint), certificate)),
        )
    }

    /// Construct a certificate-only control transaction. It creates and spends no Cells.
    pub fn control(operation: Operation, certificate: Certificate) -> Result<Self> {
        Self::build(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some((operation, certificate)),
        )
    }

    fn build(
        cell_deps: Vec<CellDep>,
        header_deps: Vec<Byte32>,
        inputs: Vec<CellInput>,
        outputs: Vec<Cell>,
        witnesses: Vec<GroupWitness>,
        protocol: Option<(Operation, Certificate)>,
    ) -> Result<Self> {
        check_count(
            cell_deps.len(),
            header_deps.len(),
            inputs.len(),
            outputs.len(),
            witnesses.len(),
        )?;
        let variable_size = outputs
            .iter()
            .map(|cell| cell.as_slice().len())
            .chain(
                protocol
                    .iter()
                    .map(|(_, certificate)| certificate.encoded_len()),
            )
            .try_fold(0usize, |size, next| size.checked_add(next))
            .context("Transaction size overflow")?;
        check_witness_bytes(&witnesses, variable_size)?;
        let (operation, certificate) = match protocol {
            Some((operation, certificate)) => {
                (Some(operation.encode()), Some(certificate.encode()))
            }
            None => (None, None),
        };
        let raw = wire::RawTransaction::new_builder()
            .cell_deps(
                wire::CellDepVec::new_builder()
                    .set(
                        cell_deps
                            .into_iter()
                            .map(|dep| wire::CellDep::new_unchecked(dep.as_bytes()))
                            .collect(),
                    )
                    .build(),
            )
            .header_deps(
                wire::Byte32Vec::new_builder()
                    .set(
                        header_deps
                            .into_iter()
                            .map(|header| wire::Byte32::new_unchecked(header.as_bytes()))
                            .collect(),
                    )
                    .build(),
            )
            .inputs(
                wire::CellInputVec::new_builder()
                    .set(inputs.into_iter().map(|input| input.encode()).collect())
                    .build(),
            )
            .protocol(
                wire::ProtocolOperationOpt::new_builder()
                    .set(operation)
                    .build(),
            )
            .outputs(
                wire::CellVec::new_builder()
                    .set(outputs.into_iter().map(|cell| cell.encoded).collect())
                    .build(),
            )
            .build();
        let witnesses = wire::GroupWitnessVec::new_builder()
            .set(witnesses.iter().map(GroupWitness::encode).collect())
            .build();
        Self::from_entity(
            wire::Transaction::new_builder()
                .raw(raw)
                .witnesses(witnesses)
                .certificate(wire::CertificateOpt::new_builder().set(certificate).build())
                .build(),
        )
    }

    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_TRANSACTION_BYTES,
            "Transaction exceeds byte limit"
        );
        Self::from_entity(wire::Transaction::from_slice(bytes)?)
    }

    fn from_entity(encoded: wire::Transaction) -> Result<Self> {
        ensure!(
            encoded.as_slice().len() <= MAX_TRANSACTION_BYTES,
            "Transaction exceeds byte limit"
        );
        let raw = encoded.raw();
        check_count(
            raw.cell_deps().len(),
            raw.header_deps().len(),
            raw.inputs().len(),
            raw.outputs().len(),
            encoded.witnesses().len(),
        )?;
        let tx = Self { encoded };
        ensure!(
            tx.valid_before_height() != Some(0),
            "expiry height must be positive"
        );
        match (raw.protocol().to_opt(), tx.encoded.certificate().to_opt()) {
            (None, None) => ensure!(
                !raw.inputs().is_empty(),
                "ordinary Transaction requires an input"
            ),
            (Some(operation), Some(certificate)) => {
                ensure!(
                    tx.valid_before_height().is_none(),
                    "protocol transaction cannot include an expiry"
                );
                let operation = Operation::decode(operation)?;
                let certificate = Certificate::decode(certificate)?;
                operation.validate(&certificate)?;
                if matches!(operation, Operation::BridgeMint(_)) {
                    ensure!(!raw.outputs().is_empty(), "Bridge Mint requires outputs");
                } else {
                    ensure!(
                        raw.inputs().is_empty()
                            && raw.outputs().is_empty()
                            && raw.cell_deps().is_empty()
                            && raw.header_deps().is_empty()
                            && tx.encoded.witnesses().is_empty(),
                        "control transaction must contain only its operation and certificate"
                    );
                }
            }
            _ => bail!("protocol operation and certificate must occur together"),
        }
        let mut previous = None;
        for witness in tx.witnesses() {
            witness.validate()?;
            ensure!(
                previous.is_none_or(|last| last < witness.group_index),
                "Group Witness entries must be strictly increasing"
            );
            ensure!(
                usize::from(witness.group_index) < MAX_SCRIPT_GROUPS,
                "Group Witness index exceeds limit"
            );
            previous = Some(witness.group_index);
        }
        let mut spent = HashSet::new();
        for input in raw.inputs() {
            let input = CellInput::decode(input)?;
            ensure!(!input.previous_output.is_null(), "null input OutPoint");
            ensure!(
                spent.insert(input.previous_output),
                "duplicate input OutPoint"
            );
        }
        let mut dep_keys = HashSet::new();
        for dep in tx.cell_deps() {
            DepType::try_from(dep.dep_type())?;
            ensure!(!dep.out_point().is_null(), "null Cell Dep OutPoint");
            ensure!(
                dep_keys.insert(dep.out_point()),
                "duplicate Cell Dep OutPoint"
            );
        }
        let mut headers = HashSet::new();
        ensure!(
            tx.header_deps().all(|header| headers.insert(header)),
            "duplicate header dependency"
        );
        for output in raw.outputs() {
            Cell::from_entity(output)?;
        }
        Ok(tx)
    }

    pub fn as_slice(&self) -> &[u8] {
        self.encoded.as_slice()
    }

    pub fn id(&self, network_id: &Byte32) -> Byte32 {
        hash(
            b"Fern transaction ID\0",
            network_id,
            &[self.encoded.raw().as_slice()],
        )
    }

    pub fn commitment(&self, network_id: &Byte32) -> Byte32 {
        hash(
            b"Fern transaction commitment\0",
            network_id,
            &[self.as_slice()],
        )
    }

    pub fn valid_before_height(&self) -> Option<u64> {
        self.encoded
            .raw()
            .valid_before_height()
            .to_opt()
            .map(|value| u64::from_le_bytes(value.as_slice().try_into().expect("Uint64")))
    }

    /// Set the exclusive height bound before signing the transaction.
    pub fn with_valid_before_height(&self, height: u64) -> Result<Self> {
        ensure!(height != 0, "expiry height must be positive");
        ensure!(
            self.operation().is_none() && self.witnesses().len() == 0,
            "expiry requires an unsigned ordinary transaction"
        );
        let raw = self
            .encoded
            .raw()
            .as_builder()
            .valid_before_height(
                wire::Uint64Opt::new_builder()
                    .set(Some(wire::Uint64::new_unchecked(
                        height.to_le_bytes().to_vec().into(),
                    )))
                    .build(),
            )
            .build();
        Self::from_entity(self.encoded.clone().as_builder().raw(raw).build())
    }

    pub fn operation(&self) -> Option<Operation> {
        self.encoded
            .raw()
            .protocol()
            .to_opt()
            .map(|operation| Operation::decode(operation).expect("validated operation"))
    }

    pub fn certificate(&self) -> Option<Certificate> {
        self.encoded
            .certificate()
            .to_opt()
            .map(Certificate::validated_entity)
    }

    pub fn cell_deps(&self) -> impl ExactSizeIterator<Item = CellDep> {
        self.encoded
            .raw()
            .cell_deps()
            .into_iter()
            .map(|dep| CellDep::new_unchecked(dep.as_bytes()))
    }

    pub fn header_deps(&self) -> impl ExactSizeIterator<Item = Byte32> {
        self.encoded
            .raw()
            .header_deps()
            .into_iter()
            .map(|header| Byte32::new_unchecked(header.as_bytes()))
    }

    pub fn inputs(&self) -> impl ExactSizeIterator<Item = CellInput> {
        self.encoded
            .raw()
            .inputs()
            .into_iter()
            .map(|input| CellInput::decode(input).expect("validated CellInput"))
    }

    pub fn input(&self, index: usize) -> Option<CellInput> {
        self.encoded
            .raw()
            .inputs()
            .get(index)
            .map(|input| CellInput::decode(input).expect("validated CellInput"))
    }

    pub fn outputs(&self) -> impl ExactSizeIterator<Item = Cell> {
        self.encoded
            .raw()
            .outputs()
            .into_iter()
            .map(Cell::validated_entity)
    }

    pub fn witnesses(&self) -> impl ExactSizeIterator<Item = GroupWitness> {
        self.encoded
            .witnesses()
            .into_iter()
            .map(GroupWitness::decode)
    }

    pub fn witness(&self, group_index: u16) -> Option<GroupWitness> {
        let entries = self.encoded.witnesses();
        let mut lo = 0;
        let mut hi = entries.len();
        while lo < hi {
            let middle = lo + (hi - lo) / 2;
            let witness = GroupWitness::decode(entries.get(middle)?);
            match witness.group_index.cmp(&group_index) {
                std::cmp::Ordering::Less => lo = middle + 1,
                std::cmp::Ordering::Greater => hi = middle,
                std::cmp::Ordering::Equal => return Some(witness),
            }
        }
        None
    }

    /// Standard Lock Script authorization binds the full group identity and
    /// all proof data. Seals can be collected independently by multiple signers.
    pub fn lock_digest(&self, network_id: &Byte32, lock: &Script) -> Byte32 {
        let mut proof = new_blake2b();
        proof.update(b"Fern group proofs\0");
        for witness in self
            .witnesses()
            .filter(|witness| !witness.proof_data.is_empty())
        {
            proof.update(&witness.group_index.to_le_bytes());
            proof.update(&(witness.proof_data.len() as u32).to_le_bytes());
            proof.update(&witness.proof_data);
        }
        let mut proof_root = [0; 32];
        proof.finalize(&mut proof_root);
        hash(
            b"Fern group lock authorization\0",
            network_id,
            &[
                self.id(network_id).as_slice(),
                &[ScriptGroupKind::Lock as u8],
                lock.calc_script_hash().as_slice(),
                &proof_root,
            ],
        )
    }

    pub fn with_witnesses(&self, witnesses: Vec<GroupWitness>) -> Result<Self> {
        check_witness_bytes(&witnesses, self.encoded.raw().as_slice().len())?;
        let witnesses = wire::GroupWitnessVec::new_builder()
            .set(witnesses.iter().map(GroupWitness::encode).collect())
            .build();
        Self::from_entity(
            self.encoded
                .clone()
                .as_builder()
                .witnesses(witnesses)
                .build(),
        )
    }
}

fn check_witness_bytes(witnesses: &[GroupWitness], base: usize) -> Result<()> {
    ensure!(
        witnesses.len() <= MAX_SCRIPT_GROUPS,
        "too many Group Witness entries"
    );
    let size = witnesses
        .iter()
        .try_fold(base, |size, witness| size.checked_add(witness.bytes()?))
        .context("Transaction size overflow")?;
    ensure!(
        size <= MAX_TRANSACTION_BYTES,
        "Transaction exceeds byte limit"
    );
    Ok(())
}

fn check_count(
    deps: usize,
    headers: usize,
    inputs: usize,
    outputs: usize,
    witnesses: usize,
) -> Result<()> {
    ensure!(
        deps + headers + inputs + outputs <= MAX_CELL_REFERENCES
            && witnesses <= MAX_CELL_REFERENCES,
        "Transaction exceeds reference count limit"
    );
    Ok(())
}

fn hash(domain: &[u8], network_id: &Byte32, items: &[&[u8]]) -> Byte32 {
    let mut hasher = new_blake2b();
    hasher.update(domain);
    hasher.update(network_id.as_slice());
    for item in items {
        hasher.update(item);
    }
    let mut digest = [0; 32];
    hasher.finalize(&mut digest);
    digest.pack()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
#[repr(u8)]
pub enum ScriptGroupKind {
    Lock = 0,
    Type = 1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScriptGroup {
    pub kind: ScriptGroupKind,
    pub script: Script,
    pub inputs: Vec<usize>,
    pub outputs: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct LiveCell {
    pub cell: Cell,
    pub origin: Origin,
}

#[derive(Clone, Debug)]
pub struct ResolvedTransaction {
    network_id: Byte32,
    transaction: Transaction,
    inputs: Vec<LiveCell>,
    outputs: Vec<Cell>,
    cell_deps: Vec<LiveCell>,
    headers: Vec<Header>,
    headers_by_origin: HashMap<Byte32, usize>,
    cache_key: OnceLock<Byte32>,
    groups: Vec<ScriptGroup>,
    programs_by_data: HashMap<Byte32, usize>,
    programs_by_type: HashMap<Byte32, CodeMatch>,
    fee: u64,
    capacity: CapacityPolicy,
    resolved_cell_bytes: usize,
    cell_reads: usize,
}

impl ResolvedTransaction {
    /// Resolve references in protocol order. Dep Groups use the CKB OutPointVec encoding.
    /// A Cell Dep may not be consumed by this transaction.
    pub fn resolve(
        network_id: Byte32,
        transaction: Transaction,
        block: BlockContext<'_>,
        mut live_cell: impl FnMut(&OutPoint) -> Result<LiveCell>,
        mut committed_header: impl FnMut(&Byte32) -> Result<Header>,
    ) -> Result<Self> {
        ensure!(
            network_id != Byte32::default(),
            "zero Fern network identity"
        );
        ensure!(
            transaction.operation().is_none(),
            "protocol transaction requires its authenticated execution path"
        );
        ensure!(
            transaction
                .valid_before_height()
                .is_none_or(|height| block.candidate.height < height),
            "Transaction has expired"
        );
        let mut budget = ResolutionBudget::default();
        let mut headers = Vec::new();
        let mut headers_by_origin = HashMap::new();
        for hash in transaction.header_deps() {
            let header = committed_header(&hash)?;
            ensure!(
                header.id(&network_id) == hash && header.context.height < block.candidate.height,
                "header dependency is not a committed ancestor"
            );
            headers_by_origin.insert(header.context.id(&network_id), headers.len());
            headers.push(header);
        }
        let mut consumed = HashSet::new();
        let mut inputs = Vec::new();
        for input in transaction.inputs() {
            let point = input.previous_output;
            consumed.insert(point.clone());
            let record = budget.load(&point, &mut live_cell)?;
            block.verify_since(input.since, &record.origin)?;
            inputs.push(record);
        }
        let mut loaded_deps = HashSet::new();
        let mut cell_deps = Vec::new();
        for dep in transaction.cell_deps() {
            let point = dep.out_point();
            ensure!(
                !consumed.contains(&point),
                "Cell Dep is consumed by this transaction"
            );
            let cell = budget.load(&point, &mut live_cell)?;
            match DepType::try_from(dep.dep_type())? {
                DepType::Code => {
                    ensure!(loaded_deps.insert(point), "duplicate resolved Cell Dep");
                    cell_deps.push(cell);
                }
                DepType::DepGroup => {
                    let data = cell.cell.data();
                    let members = ckb_types::packed::OutPointVecReader::from_slice(&data)?;
                    ensure!(!members.is_empty(), "empty Dep Group");
                    for member in members.iter() {
                        let member = member.to_entity();
                        ensure!(
                            !consumed.contains(&member),
                            "Dep Group member is consumed by this transaction"
                        );
                        ensure!(
                            loaded_deps.insert(member.clone()),
                            "duplicate resolved Cell Dep"
                        );
                        cell_deps.push(budget.load(&member, &mut live_cell)?);
                    }
                }
            }
        }
        let outputs: Vec<_> = transaction.outputs().collect();
        for cell in &outputs {
            block.capacity.verify(cell)?;
        }
        let fee = sum_capacity(inputs.iter().map(|input| &input.cell))?
            .checked_sub(sum_capacity(outputs.iter())?)
            .context("outputs exceed input capacity")?;

        let mut groups = BTreeMap::new();
        for (index, input) in inputs.iter().enumerate() {
            get_group(&mut groups, ScriptGroupKind::Lock, input.cell.lock())
                .inputs
                .push(index);
            for script in input.cell.type_scripts() {
                get_group(&mut groups, ScriptGroupKind::Type, script)
                    .inputs
                    .push(index);
            }
        }
        for (index, output) in outputs.iter().enumerate() {
            for script in output.type_scripts() {
                get_group(&mut groups, ScriptGroupKind::Type, script)
                    .outputs
                    .push(index);
            }
        }
        ensure!(groups.len() <= MAX_SCRIPT_GROUPS, "too many Script Groups");
        ensure!(
            transaction
                .witnesses()
                .all(|witness| usize::from(witness.group_index) < groups.len()),
            "Group Witness refers to an absent Script Group"
        );
        let mut programs_by_data = HashMap::new();
        let mut programs_by_type = HashMap::new();
        for (index, cell) in cell_deps.iter().enumerate() {
            let cell = &cell.cell;
            let data_hash = cell.data_hash();

            programs_by_data.entry(data_hash.clone()).or_insert(index);
            for script in cell.type_scripts() {
                programs_by_type
                    .entry(script.calc_script_hash())
                    .and_modify(|entry| match entry {
                        CodeMatch::Unique(previous, _) if previous != data_hash => {
                            *entry = CodeMatch::Ambiguous;
                        }
                        _ => {}
                    })
                    .or_insert_with(|| CodeMatch::Unique(data_hash.clone(), index));
            }
        }

        Ok(Self {
            network_id,
            transaction,
            inputs,
            outputs,
            cell_deps,
            headers,
            headers_by_origin,
            cache_key: OnceLock::new(),
            fee,
            groups: groups.into_values().collect(),
            programs_by_data,
            programs_by_type,
            capacity: block.capacity,
            resolved_cell_bytes: budget.bytes,
            cell_reads: budget.references,
        })
    }

    pub fn network_id(&self) -> &Byte32 {
        &self.network_id
    }
    pub fn transaction(&self) -> &Transaction {
        &self.transaction
    }
    pub fn inputs(&self) -> &[LiveCell] {
        &self.inputs
    }
    pub fn outputs(&self) -> &[Cell] {
        &self.outputs
    }
    pub fn cell_deps(&self) -> &[LiveCell] {
        &self.cell_deps
    }
    pub fn header(&self, index: usize) -> Option<&Header> {
        self.headers.get(index)
    }

    pub fn header_by_origin(&self, hash: &Byte32) -> Option<&Header> {
        self.header(*self.headers_by_origin.get(hash)?)
    }

    /// All VM-visible data and the full witnessed transaction, in resolution order.
    /// This key is valid only after resolving against the block's current state.
    pub fn script_cache_key(&self) -> &Byte32 {
        self.cache_key.get_or_init(|| {
            let mut hash = new_blake2b();
            hash.update(b"Fern VM execution context\0");
            hash.update(self.network_id.as_slice());
            hash.update(&self.capacity.bond_per_byte.to_le_bytes());
            hash.update(&self.capacity.state_overhead.to_le_bytes());
            hash.update(self.transaction.commitment(&self.network_id).as_slice());
            for header in &self.headers {
                hash.update(&header.encode());
            }
            for cells in [&self.inputs, &self.cell_deps] {
                hash.update(&(cells.len() as u64).to_le_bytes());
                for live in cells {
                    hash.update(live.cell.hashes().content.as_slice());
                    hash.update(&live.origin.encode());
                }
            }
            let mut digest = [0; 32];
            hash.finalize(&mut digest);
            digest.pack()
        })
    }

    pub fn groups(&self) -> &[ScriptGroup] {
        &self.groups
    }
    pub fn fee(&self) -> u64 {
        self.fee
    }

    pub fn capacity_policy(&self) -> CapacityPolicy {
        self.capacity
    }

    pub fn resolved_cell_bytes(&self) -> usize {
        self.resolved_cell_bytes
    }

    pub fn cell_reads(&self) -> usize {
        self.cell_reads
    }

    pub fn program_index(&self, script: &Script) -> Result<usize> {
        let hash_type = ScriptHashType::try_from(script.hash_type())?;
        let expected = script.code_hash();
        match hash_type {
            ScriptHashType::Type => match self.programs_by_type.get(&expected) {
                Some(CodeMatch::Unique(_, index)) => Ok(*index),
                Some(CodeMatch::Ambiguous) => bail!("ambiguous Script code dependency"),
                None => bail!("missing Script code dependency"),
            },
            _ => self
                .programs_by_data
                .get(&expected)
                .copied()
                .context("missing Script code dependency"),
        }
    }
}

#[derive(Clone, Debug)]
enum CodeMatch {
    Unique(Byte32, usize),
    Ambiguous,
}

fn get_group(
    groups: &mut BTreeMap<(ScriptGroupKind, Bytes), ScriptGroup>,
    kind: ScriptGroupKind,
    script: Script,
) -> &mut ScriptGroup {
    groups
        .entry((kind, script.as_bytes()))
        .or_insert(ScriptGroup {
            kind,
            script,
            inputs: Vec::new(),
            outputs: Vec::new(),
        })
}

fn sum_capacity<'a>(mut cells: impl Iterator<Item = &'a Cell>) -> Result<u64> {
    cells.try_fold(0u64, |sum, cell| {
        sum.checked_add(cell.capacity())
            .context("aggregate capacity overflow")
    })
}

#[derive(Default)]
struct ResolutionBudget {
    references: usize,
    bytes: usize,
}

impl ResolutionBudget {
    fn load(
        &mut self,
        point: &OutPoint,
        live_cell: &mut impl FnMut(&OutPoint) -> Result<LiveCell>,
    ) -> Result<LiveCell> {
        self.references += 1;
        ensure!(
            self.references <= MAX_CELL_REFERENCES,
            "resolved reference count exceeds limit"
        );
        let cell = live_cell(point)?;
        self.bytes += cell.cell.as_slice().len();
        ensure!(
            self.bytes <= MAX_RESOLVED_CELL_BYTES,
            "resolved Cell bytes exceed limit"
        );
        Ok(cell)
    }
}

#[cfg(test)]
mod tests {
    use crate::block::{HeaderContext, StateCommitment};

    use super::*;

    fn script(key: u8) -> Script {
        Script::new_builder()
            .code_hash([key; 32])
            .hash_type(ScriptHashType::Data)
            .args(Bytes::from_static(b"args"))
            .build()
    }

    #[test]
    fn rejects_noncanonical_cells_and_covers_every_script_in_capacity_and_commitments() -> Result<()>
    {
        let lock = script(7);
        let types = vec![script(2), script(1)];
        let data = Bytes::from_static(b"cell body");
        let capacity = Cell::minimum_capacity(&lock, &types, data.len())?;
        let cell = Cell::new(capacity, lock.clone(), types.clone(), data.clone())?;
        assert_eq!(cell.occupied_capacity()?, capacity);
        assert_eq!(
            cell.type_scripts().collect::<Vec<_>>(),
            vec![script(1), script(2)]
        );
        let underfunded = Cell::new(capacity - 1, lock.clone(), types, data.clone())?;
        assert!(CapacityPolicy::default().verify(&underfunded).is_err());
        assert!(
            CapacityPolicy {
                bond_per_byte: 1_000_000,
                state_overhead: 80
            }
            .verify(&underfunded)
            .is_ok()
        );
        assert!(Cell::new(capacity, lock.clone(), vec![script(1), script(1)], data).is_err());

        let switched = wire::ScriptVec::new_builder()
            .push(cell.encoded.type_scripts().get(1).unwrap())
            .push(cell.encoded.type_scripts().get(0).unwrap())
            .build();
        let invalid = cell
            .encoded
            .clone()
            .as_builder()
            .type_scripts(switched)
            .build();
        assert!(Cell::from_slice(invalid.as_slice()).is_err());
        assert!(Cell::from_slice(&[cell.as_slice(), &[0]].concat()).is_err());

        let ckb = ckb_types::packed::Transaction::default();
        assert!(Transaction::from_slice(ckb.as_slice()).is_err());
        Ok(())
    }

    #[test]
    fn proves_transaction_and_live_cell_with_full_type_set() -> Result<()> {
        let network = [6; 32].pack();
        let point = OutPoint::new([1; 32].pack(), 0);
        let cell = Cell::new(
            100_000_000_000,
            script(0),
            vec![script(1), script(2)],
            Bytes::new(),
        )?;
        let missing_type = Cell::new(cell.capacity(), cell.lock(), vec![script(1)], Bytes::new())?;
        let tx = Transaction::new(
            vec![],
            vec![],
            vec![CellInput::new(point.clone(), None)],
            vec![cell.clone()],
            vec![GroupWitness {
                group_index: 0,
                proof_data: Bytes::new(),
                seal: Bytes::from_static(b"witness"),
            }],
        )?;
        let raw = tx.with_witnesses(vec![GroupWitness {
            group_index: 0,
            proof_data: Bytes::new(),
            seal: Bytes::from_static(b"forged"),
        }])?;
        let origin = Origin {
            height: 7,
            timestamp_ms: 11,
            context_id: [8; 32].pack(),
        };
        let record = LiveCell {
            cell,
            origin: origin.clone(),
        };
        let tree = patricia::Patricia::new(network.clone(), patricia::Namespace::Cells);
        let key = record::cell_key(&tree, &point);
        let mut edit = tree.edit(tree.empty_root(), |_| Ok(None));
        edit.set(key, Some(record::cell_value(&tree, &point, &record)))?;
        let (_, proof) = edit.prove(&key)?;
        assert!(record::verify_cell(
            &tree,
            &edit.root(),
            &point,
            Some(&record),
            &proof
        ));
        let tampered = LiveCell {
            cell: missing_type,
            origin,
        };
        assert!(!record::verify_cell(
            &tree,
            &edit.root(),
            &point,
            Some(&tampered),
            &proof
        ));
        let transactions = proof::Tree::transactions(network.clone(), &[raw.clone(), tx.clone()])?;
        let proof = transactions.prove(1)?;
        assert!(proof.verify_transaction(&network, &transactions.root(), &tx));
        assert!(!proof.verify_transaction(&network, &transactions.root(), &raw));
        assert!(!proof.verify_transaction(&[7; 32].pack(), &transactions.root(), &tx));
        assert!(transactions.prove(2).is_err());
        Ok(())
    }

    #[test]
    fn same_script_identity_in_two_roles_and_since_are_independent() -> Result<()> {
        let script = script(1);
        let capacity = 100_000_000_000;
        let cell = Cell::new(capacity, script.clone(), vec![script.clone()], Bytes::new())?;
        let output = Cell::new(capacity, script.clone(), Vec::new(), Bytes::new())?;
        let point = OutPoint::new([1; 32].pack(), 0);
        let network = [7; 32].pack();
        let origin = Header {
            context: HeaderContext {
                height: 10,
                timestamp_ms: 123,
                parent: Byte32::default(),
                transaction_root: Byte32::default(),
            },
            state: StateCommitment {
                cell_root: Byte32::default(),
                system_root: Byte32::default(),
                history_root: Byte32::default(),
            },
            receipt_root: Byte32::default(),
        };
        let candidate = HeaderContext {
            height: 12,
            timestamp_ms: 124,
            ..origin.context.clone()
        };
        let resolve = |increment| {
            let tx = Transaction::new(
                Vec::new(),
                vec![origin.id(&network)],
                vec![CellInput::new(
                    point.clone(),
                    Some(Since::RelativeBlocks(increment)),
                )],
                vec![output.clone()],
                Vec::new(),
            )?;
            ResolvedTransaction::resolve(
                network.clone(),
                tx,
                BlockContext {
                    candidate: &candidate,
                    parent_timestamp_ms: 0,
                    capacity: Default::default(),
                },
                |_| {
                    Ok(LiveCell {
                        cell: cell.clone(),
                        origin: Origin::new(&network, &origin.context),
                    })
                },
                |_| Ok(origin.clone()),
            )
        };
        let verified = resolve(2)?;
        assert_eq!(verified.groups().len(), 2);
        assert_eq!(verified.groups()[0].kind, ScriptGroupKind::Lock);
        assert_eq!(verified.groups()[1].kind, ScriptGroupKind::Type);
        assert_eq!(verified.groups()[1].inputs, [0]);
        assert!(verified.groups()[1].outputs.is_empty());
        assert_eq!(verified.groups()[0].script, verified.groups()[1].script);
        assert!(resolve(3).is_err());
        Ok(())
    }
}
