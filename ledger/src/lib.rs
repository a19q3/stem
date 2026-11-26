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
#[allow(dead_code)]
mod strand;
use strand::prelude::{Builder as WireBuilder, Entity as WireEntity, Reader as WireReader};

mod authorization;
pub mod block;
pub mod history;
pub mod keeper;
pub mod light;
pub mod lineage;
mod message;
pub mod patricia;
pub mod proof;
pub mod protocol;
pub mod record;
pub mod replay;
pub mod time;
mod witness;

pub use message::{
    GroupAction, GroupMessage, IntentMessage, bind_intent_messages, intent_message_commitment,
    resolve_intent_messages,
};
pub use stem_intent as intent;
pub use witness::GroupWitness;

use block::Header;
pub use time::{BlockContext, CellInput, Origin, Since};

mod wire {
    #![allow(dead_code, unused_parens)]
    include!(concat!(env!("OUT_DIR"), "/strand_wire.rs"));
}

#[cfg(test)]
mod baseline {
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
const CKB_SCRIPT_TABLE_HEADER_BYTES: usize = 4 * (1 + Script::FIELD_COUNT);
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
    pub fn content_hash(&self) -> &Byte32 {
        &self.hashes().content
    }

    pub fn new(
        capacity: u64,
        lock: Script,
        type_scripts: Vec<Script>,
        data: Bytes,
    ) -> Result<Self> {
        ensure!(
            type_scripts.len() <= MAX_TYPE_SCRIPTS,
            "too many Type Scripts"
        );
        validate_script(lock.as_slice().len(), lock.hash_type().as_slice()[0])?;
        let mut script_bytes = 0;
        for script in &type_scripts {
            validate_script(script.as_slice().len(), script.hash_type().as_slice()[0])?;
            script_bytes += script.as_slice().len();
        }
        ensure!(
            script_bytes <= MAX_TYPE_SCRIPT_BYTES,
            "Type Script set exceeds byte limit"
        );
        ensure!(data.len() <= MAX_CELL_BYTES, "Cell Data exceeds byte limit");
        let mut types: Vec<_> = type_scripts.into_iter().map(encode_script).collect();
        types.sort_unstable_by(|left, right| left.as_slice().cmp(right.as_slice()));

        let encoded = wire::Cell::new_builder()
            .capacity(wire::Uint64::new_unchecked(Bytes::copy_from_slice(
                &capacity.to_le_bytes(),
            )))
            .lock(encode_script(lock))
            .type_scripts(wire::ScriptVec::new_builder().set(types).build())
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
        Self::validate(encoded.as_reader())?;
        Ok(Self::validated_entity(encoded))
    }

    fn validate(cell: wire::CellReader<'_>) -> Result<()> {
        ensure!(
            cell.as_slice().len() <= MAX_CELL_BYTES,
            "Cell exceeds byte limit"
        );
        validate_wire_script(cell.lock())?;
        let types = cell.type_scripts();
        ensure!(types.len() <= MAX_TYPE_SCRIPTS, "too many Type Scripts");
        let mut previous = None;
        let mut script_bytes = 0;
        for current in types.iter() {
            validate_wire_script(current)?;
            let bytes = current.as_slice();
            script_bytes += bytes.len();
            ensure!(
                previous.is_none_or(|last| last < bytes),
                "Type Script set is out of order or contains a duplicate"
            );
            previous = Some(bytes);
        }
        ensure!(
            script_bytes <= MAX_TYPE_SCRIPT_BYTES,
            "Type Script set exceeds byte limit"
        );
        Ok(())
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
        decode_script(&self.encoded.lock())
    }

    pub fn type_scripts(&self) -> impl ExactSizeIterator<Item = Script> {
        self.encoded
            .type_scripts()
            .into_iter()
            .map(|item| decode_script(&item))
    }

    pub fn type_script(&self, index: usize) -> Option<Script> {
        self.encoded
            .type_scripts()
            .get(index)
            .map(|script| decode_script(&script))
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
        // A presence byte, eight-byte capacity, lock and length-prefixed Data.
        let mut bytes = 1 + 8 + (37 + lock.args().raw_data().len()) + 4;
        if !type_scripts.is_empty() {
            bytes += 4;
        }
        for script in type_scripts {
            bytes = bytes
                .checked_add(37 + script.args().raw_data().len())
                .context("Cell size overflow")?;
        }
        // The Bytes vector has a length prefix followed by the raw data.
        CapacityPolicy::default().bond(bytes.checked_add(data_len).context("Cell size overflow")?)
    }
}

fn encode_script(script: Script) -> wire::Script {
    wire::Script::new_builder()
        .code_hash(wire::Byte32::new_unchecked(script.code_hash().as_bytes()))
        .hash_type(wire::ScriptAddress::from(script.hash_type().as_slice()[0]))
        .args(wire::Bytes::new_unchecked(script.args().as_bytes()))
        .build()
}

fn decode_script(script: &wire::Script) -> Script {
    Script::new_builder()
        .code_hash(Byte32::new_unchecked(script.code_hash().as_bytes()))
        .hash_type(script.hash_type().as_slice()[0])
        .args(ckb_types::packed::Bytes::new_unchecked(
            script.args().as_bytes(),
        ))
        .build()
}

fn validate_wire_script(script: wire::ScriptReader<'_>) -> Result<()> {
    // CKB packs the same fields with an additional Molecule table header.
    validate_script(
        script.as_slice().len() + CKB_SCRIPT_TABLE_HEADER_BYTES,
        script.hash_type().as_slice()[0],
    )
}

fn validate_script(packed_len: usize, hash_type: u8) -> Result<()> {
    ensure!(packed_len <= MAX_SCRIPT_BYTES, "Script exceeds byte limit");
    match ScriptHashType::try_from(hash_type)? {
        ScriptHashType::Data | ScriptHashType::Type => Ok(()),
        _ => bail!("unsupported Script hash type"),
    }
}

#[derive(Clone, Debug)]
pub struct Transaction {
    encoded: wire::Transaction,
    lineage_command: Option<Arc<lineage::Command>>,
    lineage_intent: Option<Arc<lineage::Intent>>,
    lineage_batch: Option<Arc<[Transaction]>>,
    keeper_schedule: Option<keeper::Schedule>,
    keeper: Option<keeper::Action>,
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

    pub fn bridge_mint_signing_digest(
        network: &Byte32,
        mint: &protocol::BridgeMint,
        observation: &protocol::MintObservation,
        cell_deps: Vec<CellDep>,
        outputs: Vec<Cell>,
    ) -> Result<Byte32> {
        check_count(cell_deps.len(), 0, 0, outputs.len(), 0)?;
        let raw = raw_transaction(
            cell_deps,
            vec![],
            vec![],
            outputs,
            Some(Operation::BridgeMint(mint.clone()).encode()),
        );
        ensure!(
            raw.as_slice().len() <= MAX_TRANSACTION_BYTES,
            "mint exceeds transaction size limit"
        );
        let id = hash(b"Stem transaction ID\0", network, &[raw.as_slice()]);
        Ok(protocol::mint_digest(network, &id, observation))
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
            Some((Operation::BridgeMint(mint), Some(certificate))),
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
            Some((operation, Some(certificate))),
        )
    }

    pub fn fee_claim(claim: protocol::FeeClaim) -> Result<Self> {
        Self::without_certificate(Operation::FeeClaim(claim))
    }

    pub fn reward_claim(claim: protocol::RewardClaim) -> Result<Self> {
        Self::without_certificate(Operation::RewardClaim(claim))
    }

    fn without_certificate(operation: Operation) -> Result<Self> {
        Self::build(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Some((operation, None)),
        )
    }

    fn build(
        cell_deps: Vec<CellDep>,
        header_deps: Vec<Byte32>,
        inputs: Vec<CellInput>,
        outputs: Vec<Cell>,
        witnesses: Vec<GroupWitness>,
        protocol: Option<(Operation, Option<Certificate>)>,
    ) -> Result<Self> {
        check_count(
            cell_deps.len(),
            header_deps.len(),
            inputs.len(),
            outputs.len(),
            witnesses.len(),
        )?;
        let variable_size =
            outputs
                .iter()
                .map(|cell| cell.as_slice().len())
                .chain(protocol.iter().filter_map(|(_, certificate)| {
                    certificate.as_ref().map(Certificate::encoded_len)
                }))
                .try_fold(0usize, |size, next| size.checked_add(next))
                .context("Transaction size overflow")?;
        check_witness_bytes(&witnesses, variable_size)?;
        let (operation, certificate) = match protocol {
            Some((operation, certificate)) => (
                Some(operation.encode()),
                certificate.map(|certificate| certificate.encode()),
            ),
            None => (None, None),
        };
        let raw = raw_transaction(cell_deps, header_deps, inputs, outputs, operation);
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
        if let Some(certificate) = encoded.l1_reference_certificate().to_opt() {
            ensure!(
                raw.protocol().to_opt().is_none() && raw.lineage_command().to_opt().is_none(),
                "L1 references require an ordinary Cell transaction"
            );
            let certificate = Certificate::decode(certificate)?;
            protocol::l1_reference::decode_reference_set(&certificate.body())?;
        }
        check_count(
            raw.cell_deps().len(),
            raw.header_deps().len(),
            raw.inputs().len(),
            raw.outputs().len(),
            encoded.witnesses().len(),
        )?;
        let lineage_command = raw
            .lineage_command()
            .to_opt()
            .map(lineage::Command::decode)
            .transpose()?
            .map(Arc::new);
        let keeper_schedule = raw
            .keeper_schedule()
            .to_opt()
            .map(keeper::Schedule::decode)
            .transpose()?;
        let keeper = raw
            .keeper()
            .to_opt()
            .map(keeper::Action::decode)
            .transpose()?;
        let lineage_intent = raw
            .lineage_intent()
            .to_opt()
            .map(lineage::Intent::decode)
            .transpose()?
            .map(Arc::new);
        let mut tx = Self {
            encoded,
            lineage_command,
            lineage_intent,
            lineage_batch: None,
            keeper_schedule,
            keeper,
        };
        if let Some(schedule) = tx.keeper_schedule {
            ensure!(
                tx.has_lineage_command()
                    && tx.valid_before_height().is_some_and(|expiry| {
                        match schedule.trigger {
                            keeper::Trigger::Height(height) => height < expiry,
                            keeper::Trigger::Time(_) => expiry > 1,
                        }
                    }),
                "invalid Keeper trigger or expiry"
            );
        }
        ensure!(
            tx.keeper.is_none() || (raw.protocol().to_opt().is_none() && !tx.has_lineage_command()),
            "Keeper actions belong to ordinary Cell transactions"
        );
        GroupMessage::validate_all(&tx.messages().collect::<Vec<_>>())?;
        ensure!(
            tx.valid_before_height() != Some(0),
            "expiry height must be positive"
        );
        match (raw.protocol().to_opt(), tx.encoded.certificate().to_opt()) {
            (None, None) => ensure!(
                !raw.inputs().is_empty(),
                "ordinary Transaction requires an input"
            ),
            (Some(operation), certificate) => {
                ensure!(
                    tx.valid_before_height().is_none() && tx.messages().len() == 0,
                    "protocol transaction cannot include an expiry or Group Messages"
                );
                let operation = Operation::decode(operation)?;
                match &operation {
                    Operation::LineageBatch(members) => {
                        ensure!(
                            certificate.is_none(),
                            "Lineage batch cannot carry a certificate"
                        );
                        tx.lineage_batch = Some(Self::decode_lineage_batch(members)?);
                    }
                    Operation::FeeClaim(_) | Operation::RewardClaim(_) => {
                        ensure!(
                            certificate.is_none(),
                            "fee claims cannot carry a certificate"
                        );
                    }
                    _ => {
                        let certificate =
                            certificate.context("protocol operation requires a certificate")?;
                        operation.validate(&Certificate::decode(certificate)?)?;
                    }
                }
                if matches!(operation, Operation::BridgeMint(_)) {
                    ensure!(
                        !raw.outputs().is_empty()
                            && raw.inputs().is_empty()
                            && raw.header_deps().is_empty(),
                        "Bridge Mint requires outputs and no Cell inputs or Header Deps"
                    );
                } else {
                    ensure!(
                        raw.inputs().is_empty()
                            && raw.outputs().is_empty()
                            && raw.cell_deps().is_empty()
                            && raw.header_deps().is_empty()
                            && tx.encoded.witnesses().is_empty(),
                        "control transaction cannot contain Cells, dependencies or witnesses"
                    );
                }
            }
            (None, Some(_)) => bail!("certificate without a protocol operation"),
        }
        if let Some(command) = tx.lineage_command() {
            ensure!(
                raw.protocol().to_opt().is_none()
                    && raw.inputs().len() == 1
                    && tx.input(0).is_some_and(|input| input.since.is_none())
                    && raw.outputs().is_empty()
                    && raw.header_deps().is_empty()
                    && raw.messages().is_empty()
                    && tx.valid_before_height().is_some(),
                "invalid Lineage command envelope"
            );
            ensure!(
                tx.cell_deps().any(|dep| {
                    dep.out_point() == command.executor_dep && dep.dep_type().as_slice() == [0]
                }),
                "Lineage Executor must be a direct Cell Dep"
            );
            ensure!(
                tx.witnesses().len() <= 1
                    && tx.witnesses().all(|witness| {
                        witness.group_index == 0
                            && witness.proof_data.is_empty()
                            && witness.seal.len() == 65
                    }),
                "Lineage command requires one fee seal at index zero"
            );
        }
        let lineage_seals = tx.lineage_seals();
        if let Some(intent) = tx.lineage_intent() {
            let command = tx
                .lineage_command()
                .context("business authorization requires a Lineage command")?;
            ensure!(
                command.fee <= intent.max_fee
                    && command
                        .targets
                        .iter()
                        .any(|target| target.min_version == target.max_version),
                "Lineage intent requires its fee cap and an exact target version"
            );
            ensure!(
                lineage_seals.len() == intent.actors.len() * lineage::SEAL_LEN
                    || (lineage_seals.is_empty() && tx.witnesses().len() == 0),
                "incomplete Lineage business seals"
            );
        } else {
            ensure!(
                lineage_seals.is_empty(),
                "unexpected Lineage business seals"
            );
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
        let inputs = raw.inputs();
        let check_unique_inputs = inputs.len() > 1;
        let mut spent = HashSet::new();
        for input in inputs {
            let input = CellInput::decode(input)?;
            ensure!(!input.previous_output.is_null(), "null input OutPoint");
            ensure!(
                !check_unique_inputs || spent.insert(input.previous_output),
                "duplicate input OutPoint"
            );
        }
        let deps = tx.cell_deps();
        let check_unique_deps = deps.len() > 1;
        let mut dep_keys = HashSet::new();
        for dep in deps {
            DepType::try_from(dep.dep_type())?;
            ensure!(!dep.out_point().is_null(), "null Cell Dep OutPoint");
            ensure!(
                !check_unique_deps || dep_keys.insert(dep.out_point()),
                "duplicate Cell Dep OutPoint"
            );
        }
        let header_deps = raw.as_reader().header_deps();
        let mut headers = HashSet::new();
        ensure!(
            header_deps.len() <= 1
                || header_deps
                    .iter()
                    .all(|header| headers.insert(header.as_slice())),
            "duplicate header dependency"
        );
        for output in raw.as_reader().outputs().iter() {
            Cell::validate(output)?;
        }
        Ok(tx)
    }

    pub fn as_slice(&self) -> &[u8] {
        self.encoded.as_slice()
    }

    pub fn id(&self, network_id: &Byte32) -> Byte32 {
        let references = self.encoded.l1_reference_certificate();
        hash(
            b"Stem transaction ID\0",
            network_id,
            &[self.encoded.raw().as_slice(), references.as_slice()],
        )
    }

    pub fn commitment(&self, network_id: &Byte32) -> Byte32 {
        self.commitment_bytes(network_id).pack()
    }

    fn commitment_bytes(&self, network_id: &Byte32) -> [u8; 32] {
        hash_bytes(
            b"Stem transaction commitment\0",
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
            .or_else(|| {
                self.lineage_batch()?
                    .iter()
                    .filter_map(Self::valid_before_height)
                    .min()
            })
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

    /// Construct a bounded operation whose exact head inputs are resolved at execution.
    /// The single exact input pays the signed fee and prevents replay.
    pub fn new_lineage_command(
        cell_deps: Vec<CellDep>,
        fee_input: OutPoint,
        expires: u64,
        command: lineage::Command,
    ) -> Result<Self> {
        command.validate()?;
        let transaction = Self::new(
            cell_deps,
            vec![],
            vec![CellInput::new(fee_input, None)],
            vec![],
            vec![],
        )?
        .with_valid_before_height(expires)?;
        let raw = transaction
            .encoded
            .raw()
            .as_builder()
            .lineage_command(
                wire::LineageCommandOpt::new_builder()
                    .set(Some(command.encode()))
                    .build(),
            )
            .build();
        Self::from_entity(transaction.encoded.as_builder().raw(raw).build())
    }

    pub fn lineage_command(&self) -> Option<&lineage::Command> {
        self.lineage_command.as_deref()
    }

    pub fn has_lineage_command(&self) -> bool {
        self.lineage_command.is_some()
    }

    pub fn keeper_schedule(&self) -> Option<keeper::Schedule> {
        self.keeper_schedule
            .or_else(|| self.lineage_batch()?.first()?.keeper_schedule())
    }

    pub fn keeper_trigger(&self) -> Option<keeper::Trigger> {
        self.keeper_schedule().map(|schedule| schedule.trigger)
    }

    pub fn with_execute_at(&self, height: u64) -> Result<Self> {
        self.with_keeper_trigger(keeper::Trigger::Height(height))
    }

    pub fn with_keeper_trigger(&self, trigger: keeper::Trigger) -> Result<Self> {
        self.with_keeper_schedule(keeper::Schedule {
            trigger,
            retry: None,
        })
    }

    pub fn with_keeper_schedule(&self, schedule: keeper::Schedule) -> Result<Self> {
        ensure!(
            self.witnesses().len() == 0 && self.lineage_seals().is_empty(),
            "set Keeper schedule before signing"
        );
        let raw = self
            .encoded
            .raw()
            .as_builder()
            .keeper_schedule(
                wire::KeeperScheduleOpt::new_builder()
                    .set(Some(schedule.encode()))
                    .build(),
            )
            .build();
        Self::from_entity(self.encoded.clone().as_builder().raw(raw).build())
    }

    pub fn keeper_action(&self) -> Option<&keeper::Action> {
        self.keeper.as_ref()
    }

    pub fn with_keeper_action(&self, action: keeper::Action) -> Result<Self> {
        ensure!(
            self.witnesses().len() == 0,
            "set Keeper action before signing"
        );
        let raw = self
            .encoded
            .raw()
            .as_builder()
            .keeper(
                wire::KeeperActionOpt::new_builder()
                    .set(Some(action.encode()))
                    .build(),
            )
            .build();
        Self::from_entity(self.encoded.clone().as_builder().raw(raw).build())
    }

    pub fn lineage_intent(&self) -> Option<&lineage::Intent> {
        self.lineage_intent.as_deref()
    }

    /// Encode a group of fully sealed, batch-bound Lineage intents in signed order.
    pub fn new_lineage_batch(members: Vec<Transaction>) -> Result<Self> {
        ensure!(
            (2..=lineage::MAX_BATCH_MEMBERS).contains(&members.len()),
            "Lineage batch member count is outside bounds"
        );
        let payload = members
            .into_iter()
            .map(|tx| tx.encoded.as_bytes())
            .collect();
        Self::without_certificate(Operation::LineageBatch(payload))
    }

    pub fn lineage_batch(&self) -> Option<&[Self]> {
        self.lineage_batch.as_deref()
    }

    fn decode_lineage_batch(members: &[Bytes]) -> Result<Arc<[Self]>> {
        let mut fees = HashSet::new();
        let mut heads = HashSet::new();
        let parsed = members
            .iter()
            .map(|bytes| {
                let wire = wire::Transaction::from_slice(bytes)?;
                ensure!(
                    wire.raw().protocol().to_opt().is_none(),
                    "Lineage batches require non-nested command members"
                );
                let member = Self::from_entity(wire)?;
                ensure!(
                    member
                        .lineage_intent()
                        .is_some_and(|intent| intent.batch.is_some())
                        && member.witnesses().len() == 1,
                    "Lineage batch member requires batch-bound business and fee seals"
                );
                ensure!(
                    fees.insert(member.input(0).expect("validated command").previous_output),
                    "duplicate Lineage batch fee Cell"
                );
                let command = member.lineage_command().expect("validated intent");
                ensure!(
                    command
                        .targets
                        .iter()
                        .all(|target| heads.insert(target.id.clone())),
                    "overlapping Lineage batch targets"
                );
                Ok(member)
            })
            .collect::<Result<Arc<[_]>>>()?;
        ensure!(
            parsed
                .iter()
                .all(|member| member.keeper_schedule() == parsed[0].keeper_schedule()),
            "Lineage batch members must agree on the Keeper schedule"
        );
        Ok(parsed)
    }

    /// Bind this unsigned sponsored command to a plan before collecting its business seals.
    pub fn with_lineage_batch_plan(&self, plan: Byte32) -> Result<Self> {
        let mut intent = self
            .lineage_intent()
            .context("batch plan requires a Lineage intent")?
            .clone();
        intent.batch = Some(plan);
        self.with_lineage_intent(intent)
    }

    /// Each leaf excludes only sponsor selection and the plan ID, which would be circular.
    pub fn lineage_batch_plan(network: &Byte32, members: &[Self]) -> Result<Byte32> {
        ensure!(
            (2..=lineage::MAX_BATCH_MEMBERS).contains(&members.len()),
            "Lineage batch member count is outside bounds"
        );
        let mut heads = HashSet::new();
        let mut leaves = Vec::with_capacity(1 + members.len() * 32);
        leaves.push(members.len() as u8);
        for member in members {
            ensure!(
                member.keeper_schedule() == members[0].keeper_schedule(),
                "Lineage batch members must agree on the Keeper schedule"
            );
            let mut intent = member
                .lineage_intent()
                .context("batch plan requires a Lineage intent")?
                .clone();
            intent.batch = None;
            let terms = member.normalized_lineage_terms(&intent);
            let command = member.lineage_command().expect("validated intent");
            ensure!(
                command
                    .targets
                    .iter()
                    .all(|target| heads.insert(target.id.clone())),
                "overlapping Lineage batch targets"
            );
            leaves.extend_from_slice(
                hash(
                    b"Stem Lineage batch member terms\0",
                    network,
                    &[terms.as_slice()],
                )
                .as_slice(),
            );
        }
        Ok(hash(b"Stem Lineage batch plan\0", network, &[&leaves]))
    }

    /// Match every member's binding to the exact signed order and contents.
    pub fn verify_lineage_batch_plan(network: &Byte32, members: &[Self]) -> Result<Byte32> {
        let plan = Self::lineage_batch_plan(network, members)?;
        ensure!(
            members.iter().all(|member| member
                .lineage_intent()
                .and_then(|intent| intent.batch.as_ref())
                == Some(&plan)),
            "Lineage batch differs from the signed plan"
        );
        Ok(plan)
    }

    pub fn lineage_seals(&self) -> Bytes {
        self.encoded.lineage_seals().raw_data()
    }

    pub fn with_lineage_intent(&self, intent: lineage::Intent) -> Result<Self> {
        ensure!(
            self.witnesses().len() == 0 && self.lineage_seals().is_empty(),
            "set Lineage intent before signing"
        );
        let raw = self
            .encoded
            .raw()
            .as_builder()
            .lineage_intent(
                wire::LineageIntentOpt::new_builder()
                    .set(Some(intent.encode()))
                    .build(),
            )
            .build();
        Self::from_entity(self.encoded.clone().as_builder().raw(raw).build())
    }

    pub fn with_lineage_seals(&self, seals: Bytes) -> Result<Self> {
        ensure!(
            self.witnesses().len() == 0,
            "attach business seals before the fee seal"
        );
        Self::from_entity(
            self.encoded
                .clone()
                .as_builder()
                .lineage_seals(wire::Bytes::new_unchecked(seals.pack().as_bytes()))
                .build(),
        )
    }

    pub fn with_lineage_fee(&self, point: OutPoint, fee: u64) -> Result<Self> {
        ensure!(
            self.lineage_intent().is_some() && self.witnesses().len() == 0,
            "choose the sponsored fee before adding the fee seal"
        );
        let mut command = self.lineage_command().expect("validated intent").clone();
        command.fee = fee;
        let raw = self
            .encoded
            .raw()
            .as_builder()
            .inputs(
                wire::CellInputVec::new_builder()
                    .set(vec![CellInput::new(point, None).encode()])
                    .build(),
            )
            .lineage_command(
                wire::LineageCommandOpt::new_builder()
                    .set(Some(command.encode()))
                    .build(),
            )
            .build();
        Self::from_entity(self.encoded.clone().as_builder().raw(raw).build())
    }

    pub fn lineage_intent_digest(&self, network: &Byte32) -> Result<Byte32> {
        let intent = self.lineage_intent().context("expected a Lineage intent")?;
        let raw = self.normalized_lineage_terms(intent);
        Ok(hash(
            b"Stem Lineage business authorization\0",
            network,
            &[raw.as_slice()],
        ))
    }

    fn normalized_lineage_terms(&self, intent: &lineage::Intent) -> wire::RawTransaction {
        let mut terms = self.lineage_command().expect("validated intent").clone();
        terms.fee = 0;
        self.encoded
            .raw()
            .as_builder()
            .inputs(wire::CellInputVec::new_builder().build())
            .lineage_command(
                wire::LineageCommandOpt::new_builder()
                    .set(Some(terms.encode()))
                    .build(),
            )
            .lineage_intent(
                wire::LineageIntentOpt::new_builder()
                    .set(Some(intent.encode()))
                    .build(),
            )
            .build()
    }

    pub fn lineage_signing_digest(&self, network: &Byte32, fee_cell: &Cell) -> Result<Byte32> {
        ensure!(self.has_lineage_command(), "expected a Lineage command");
        if let Some(intent) = self.lineage_intent() {
            let seals = self.lineage_seals();
            ensure!(
                seals.len() == intent.actors.len() * lineage::SEAL_LEN,
                "fee seal requires complete business seals"
            );
            return Ok(hash(
                b"Stem sponsored Lineage fee authorization\0",
                network,
                &[
                    self.id(network).as_slice(),
                    fee_cell.content_hash().as_slice(),
                    &seals,
                ],
            ));
        }
        Ok(hash(
            b"Stem Lineage command authorization\0",
            network,
            &[
                self.id(network).as_slice(),
                fee_cell.hashes().content.as_slice(),
            ],
        ))
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

    pub fn l1_reference_certificate(&self) -> Option<Certificate> {
        self.encoded
            .l1_reference_certificate()
            .to_opt()
            .map(Certificate::validated_entity)
    }

    pub fn with_l1_reference_certificate(&self, certificate: Certificate) -> Result<Self> {
        ensure!(
            self.witnesses().all(|witness| witness.seal.is_empty()),
            "attach L1 references before collecting seals"
        );
        Self::from_entity(
            self.encoded
                .clone()
                .as_builder()
                .l1_reference_certificate(
                    wire::CertificateOpt::new_builder()
                        .set(Some(certificate.encode()))
                        .build(),
                )
                .build(),
        )
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

    pub fn messages(&self) -> impl ExactSizeIterator<Item = GroupMessage> {
        self.encoded
            .raw()
            .messages()
            .into_iter()
            .map(GroupMessage::decode)
    }

    pub fn with_messages(&self, messages: Vec<GroupMessage>) -> Result<Self> {
        ensure!(
            self.operation().is_none(),
            "protocol transaction cannot carry Messages"
        );
        ensure!(
            self.witnesses().all(|witness| witness.seal.is_empty()),
            "Group Messages must be set before collecting seals"
        );
        GroupMessage::validate_all(&messages)?;
        let messages = wire::GroupMessageVec::new_builder()
            .set(messages.iter().map(GroupMessage::encode).collect())
            .build();
        let raw = self.encoded.raw().as_builder().messages(messages).build();
        Self::from_entity(self.encoded.clone().as_builder().raw(raw).build())
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

fn raw_transaction(
    cell_deps: Vec<CellDep>,
    header_deps: Vec<Byte32>,
    inputs: Vec<CellInput>,
    outputs: Vec<Cell>,
    operation: Option<wire::ProtocolOperation>,
) -> wire::RawTransaction {
    wire::RawTransaction::new_builder()
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
        .build()
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
    hash_bytes(domain, network_id, items).pack()
}

fn hash_bytes(domain: &[u8], network_id: &Byte32, items: &[&[u8]]) -> [u8; 32] {
    let mut hasher = new_blake2b();
    hasher.update(domain);
    hasher.update(network_id.as_slice());
    for item in items {
        hasher.update(item);
    }
    let mut digest = [0; 32];
    hasher.finalize(&mut digest);
    digest
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
struct LineageAuthorization {
    original: Transaction,
    id: Byte32,
    digest: Byte32,
}

#[derive(Clone, Debug)]
pub struct ResolvedTransaction {
    network_id: Byte32,
    transaction: Transaction,
    height: u64,
    parent: Header,
    parent_id: Byte32,
    authorization: authorization::LockAuthorization,
    lineage_authorization: Option<LineageAuthorization>,
    inputs: Vec<LiveCell>,
    outputs: Vec<Cell>,
    cell_deps: Vec<LiveCell>,
    l1_references: Option<Arc<[protocol::l1_reference::L1ReferenceRecord]>>,
    headers: Vec<Header>,
    headers_by_origin: HashMap<Byte32, usize>,
    cache_key: OnceLock<Byte32>,
    groups: Vec<ScriptGroup>,
    messages: BTreeMap<u16, Bytes>,
    intent_scope: Option<IntentScope>,
    programs: OnceLock<ProgramIndex>,
    fee: u64,
    capacity: CapacityPolicy,
    minted_asset: Option<(Script, u128)>,
    resolved_cell_bytes: usize,
    cell_reads: usize,
}

#[derive(Clone, Debug)]
struct IntentScope {
    lock_code: Byte32,
    actions: Byte32,
}

impl IntentScope {
    fn resolve(tx: &Transaction, groups: &[ScriptGroup]) -> Option<Self> {
        if tx.operation().is_some()
            || tx.has_lineage_command()
            || tx.keeper_action().is_some()
            || tx.l1_reference_certificate().is_some()
        {
            return None;
        }
        let mut lock_code = None;
        for witness in tx
            .witnesses()
            .filter(|witness| !witness.proof_data.is_empty())
        {
            let group = groups.get(usize::from(witness.group_index))?;
            if group.kind != ScriptGroupKind::Lock
                || group.script.hash_type().as_slice() != [0]
                || intent::Terms::decode(&witness.proof_data).is_none()
            {
                return None;
            }
            let code = group.script.code_hash();
            if lock_code.as_ref().is_some_and(|previous| previous != &code) {
                return None;
            }
            lock_code = Some(code);
        }
        Some(Self {
            lock_code: lock_code?,
            actions: resolve_intent_messages(groups, tx.messages()).ok()?,
        })
    }
}

impl ResolvedTransaction {
    /// Resolve references in protocol order. Dep Groups use the CKB OutPointVec encoding.
    /// A Cell Dep may not be consumed by this transaction.
    pub fn resolve(
        transaction: Transaction,
        block: &BlockContext<'_>,
        live_cell: impl FnMut(&OutPoint) -> Result<LiveCell>,
        committed_header: impl FnMut(&Byte32) -> Result<Header>,
    ) -> Result<Self> {
        ensure!(
            transaction.operation().is_none()
                && !transaction.has_lineage_command()
                && transaction.l1_reference_certificate().is_none(),
            "transaction requires its authenticated execution path"
        );
        Self::resolve_with_capacity(transaction, block, None, None, live_cell, committed_header)
    }

    pub fn resolve_referenced(
        transaction: Transaction,
        block: &BlockContext<'_>,
        grant: protocol::l1_reference::VerifiedL1References,
        live_cell: impl FnMut(&OutPoint) -> Result<LiveCell>,
        committed_header: impl FnMut(&Byte32) -> Result<Header>,
    ) -> Result<Self> {
        ensure!(
            transaction.operation().is_none() && !transaction.has_lineage_command(),
            "L1 references require an ordinary Cell transaction"
        );
        let references = grant.authorize(&transaction, block.network())?;
        Self::resolve_with_capacity(
            transaction,
            block,
            None,
            Some(references),
            live_cell,
            committed_header,
        )
    }

    pub fn resolve_mint(
        transaction: Transaction,
        block: &BlockContext<'_>,
        grant: &protocol::VerifiedMint,
        asset: Option<Script>,
        live_cell: impl FnMut(&OutPoint) -> Result<LiveCell>,
        committed_header: impl FnMut(&Byte32) -> Result<Header>,
    ) -> Result<Self> {
        let capacity = grant.capacity(block.network(), &transaction)?;
        let units = grant.observation().units;
        ensure!(
            asset.is_some() == (units > 0),
            "mint asset differs from authenticated units"
        );
        let authorization = (capacity, asset.map(|script| (script, units)));
        Self::resolve_with_capacity(
            transaction,
            block,
            Some(authorization),
            None,
            live_cell,
            committed_header,
        )
    }

    fn resolve_with_capacity(
        transaction: Transaction,
        block: &BlockContext<'_>,
        mint: Option<(u64, Option<(Script, u128)>)>,
        l1_references: Option<Arc<[protocol::l1_reference::L1ReferenceRecord]>>,
        live_cell: impl FnMut(&OutPoint) -> Result<LiveCell>,
        mut committed_header: impl FnMut(&Byte32) -> Result<Header>,
    ) -> Result<Self> {
        let network = block.network();
        let candidate = block.candidate();
        ensure!(
            transaction
                .valid_before_height()
                .is_none_or(|height| candidate.height < height),
            "Transaction has expired"
        );
        let mut budget = ResolutionBudget::default();
        let mut headers = Vec::new();
        let mut headers_by_origin = HashMap::new();
        for hash in transaction.header_deps() {
            let header = committed_header(&hash)?;
            ensure!(
                header.id(network) == hash && header.context.height < candidate.height,
                "header dependency is not a committed ancestor"
            );
            headers_by_origin.insert(header.context.id(network), headers.len());
            headers.push(header);
        }
        let authorization::Cells {
            inputs,
            code: cell_deps,
            commitment,
            dependencies,
        } = authorization::resolve_cells(
            &transaction,
            &mut budget,
            transaction.encoded.witnesses().into_iter().any(|entry| {
                // Only the partial-authorization path needs this additional digest.
                matches!(
                    entry.proof_data().raw_data().len(),
                    intent::TRADE_BYTES | intent::SPONSOR_BYTES
                )
            }),
            live_cell,
        )?;
        for (input, record) in transaction.inputs().zip(&inputs) {
            block.verify_since(input.since, &record.origin)?;
        }
        let has_intent_dependencies = dependencies.is_some();
        let authorization = authorization::LockAuthorization::new(
            network,
            &transaction,
            commitment,
            dependencies,
            &headers,
        );
        let outputs: Vec<_> = transaction.outputs().collect();
        for cell in &outputs {
            block.capacity().verify(cell)?;
        }
        let output_capacity = sum_capacity(outputs.iter())?;
        let fee = if let Some((minted_capacity, _)) = &mint {
            ensure!(
                output_capacity == *minted_capacity,
                "mint differs from authenticated Capacity"
            );
            0
        } else {
            sum_capacity(inputs.iter().map(|input| &input.cell))?
                .checked_sub(output_capacity)
                .context("outputs exceed input capacity")?
        };

        let groups = derive_script_groups(inputs.iter().map(|input| &input.cell), outputs.iter())?;
        let mut messages = BTreeMap::new();
        for message in transaction.messages() {
            ensure!(
                usize::from(message.group_index) < groups.len(),
                "Group Message refers to an absent Script Group"
            );
            messages.insert(message.group_index, message.encode_bytes());
        }
        ensure!(
            transaction
                .witnesses()
                .all(|witness| usize::from(witness.group_index) < groups.len()),
            "Group Witness refers to an absent Script Group"
        );
        let intent_scope = has_intent_dependencies
            .then(|| IntentScope::resolve(&transaction, &groups))
            .flatten();
        Ok(Self {
            network_id: network.clone(),
            transaction,
            height: candidate.height,
            parent: block.parent().clone(),
            parent_id: candidate.parent.clone(),
            authorization,
            lineage_authorization: None,
            inputs,
            outputs,
            cell_deps,
            l1_references,
            headers,
            headers_by_origin,
            cache_key: OnceLock::new(),
            fee,
            groups,
            messages,
            intent_scope,
            programs: OnceLock::new(),
            capacity: block.capacity(),
            minted_asset: mint.and_then(|(_, asset)| asset),
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
    pub fn l1_references(&self) -> &[protocol::l1_reference::L1ReferenceRecord] {
        self.l1_references.as_deref().unwrap_or(&[])
    }
    pub fn header(&self, index: usize) -> Option<&Header> {
        self.headers.get(index)
    }

    pub fn header_by_origin(&self, hash: &Byte32) -> Option<&Header> {
        self.header(*self.headers_by_origin.get(hash)?)
    }

    pub fn height(&self) -> u64 {
        self.height
    }

    pub fn parent_header(&self) -> &Header {
        &self.parent
    }

    pub fn lock_digest(&self, group: &ScriptGroup) -> Byte32 {
        self.authorization.digest(&self.network_id, &group.script)
    }

    pub fn intent_dependency_commitment(&self) -> Option<Byte32> {
        self.authorization.intent_dependencies(&self.network_id)
    }

    pub fn intent_action_commitment(&self, group: &ScriptGroup) -> Option<&Byte32> {
        self.intent_scope
            .as_ref()
            .filter(|scope| {
                self.lineage_authorization.is_none()
                    && group.kind == ScriptGroupKind::Lock
                    && group.script.hash_type().as_slice() == [0]
                    && group.script.code_hash() == scope.lock_code
            })
            .map(|scope| &scope.actions)
    }

    /// Bind a kernel-materialized transaction to its original signed command.
    /// Input zero is the fee Cell and all subsequent inputs are resolved Lineage heads.
    pub fn authorize_materialized_lineage(
        mut self,
        original: &Transaction,
        fee_cell: &Cell,
    ) -> Result<Self> {
        ensure!(
            self.inputs
                .first()
                .is_some_and(|input| input.cell.as_slice() == fee_cell.as_slice())
                && original.input(0) == self.transaction.input(0),
            "Lineage fee input mismatch"
        );
        self.lineage_authorization = Some(LineageAuthorization {
            original: original.clone(),
            id: original.id(&self.network_id),
            digest: original.lineage_signing_digest(&self.network_id, fee_cell)?,
        });
        self.cache_key.take();
        Ok(self)
    }

    pub fn execution_id(&self) -> Byte32 {
        self.lineage_authorization.as_ref().map_or_else(
            || self.authorization.raw_id.clone(),
            |authorization| authorization.id.clone(),
        )
    }

    pub fn original_lineage_command(&self) -> Option<&Transaction> {
        self.lineage_authorization
            .as_ref()
            .map(|authorization| &authorization.original)
    }

    /// Role zero is the fee lock and role one covers only the resolved heads.
    /// A mixed group receives no delegated authorization.
    pub fn lineage_lock_context(&self, group: &ScriptGroup) -> Option<[u8; 33]> {
        let authorization = self.lineage_authorization.as_ref()?;
        if group.kind != ScriptGroupKind::Lock || group.inputs.is_empty() {
            return None;
        }
        let role = match group.inputs.as_slice() {
            [0] => 0,
            [first, ..] if *first > 0 => 1,
            _ => return None,
        };
        let mut bytes = [0; 33];
        bytes[0] = role;
        bytes[1..].copy_from_slice(authorization.digest.as_slice());
        Some(bytes)
    }

    pub fn parent_id(&self) -> &Byte32 {
        &self.parent_id
    }

    /// Commit the full witnessed transaction and resolved data, except the
    /// candidate height and its parent. The VM cache binds the parent separately
    /// when execution reads it. Resolution always checks the current state.
    pub fn script_cache_key(&self) -> &Byte32 {
        self.cache_key.get_or_init(|| {
            let mut hash = new_blake2b();
            hash.update(b"Stem VM resolved transaction\0");
            hash.update(self.network_id.as_slice());
            hash.update(self.authorization.cells.as_slice());
            hash.update(&self.capacity.bond_per_byte.to_le_bytes());
            hash.update(&self.capacity.state_overhead.to_le_bytes());
            hash.update(&[u8::from(self.minted_asset.is_some())]);
            if let Some((asset, units)) = &self.minted_asset {
                hash.update(asset.calc_script_hash().as_slice());
                hash.update(&units.to_le_bytes());
            }
            hash.update(self.transaction.commitment(&self.network_id).as_slice());
            hash.update(&[u8::from(self.lineage_authorization.is_some())]);
            if let Some(authorization) = &self.lineage_authorization {
                hash.update(
                    authorization
                        .original
                        .commitment(&self.network_id)
                        .as_slice(),
                );
                hash.update(authorization.digest.as_slice());
            }
            let references = self.l1_references();
            hash.update(&(references.len() as u64).to_le_bytes());
            for reference in references {
                let raw = reference.as_slice();
                hash.update(&(raw.len() as u64).to_le_bytes());
                hash.update(raw);
            }
            hash.update(&(self.headers.len() as u64).to_le_bytes());
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
    pub fn group_message(&self, group: u16) -> Option<&Bytes> {
        self.messages.get(&group)
    }
    pub fn fee(&self) -> u64 {
        self.fee
    }

    pub fn minted_units(&self, group: &ScriptGroup) -> u128 {
        self.minted_asset
            .as_ref()
            .filter(|(asset, _)| group.kind == ScriptGroupKind::Type && *asset == group.script)
            .map_or(0, |(_, units)| *units)
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
        match self.cell_deps.as_slice() {
            [] => bail!("missing Script code dependency"),
            [dep] => {
                let matches = match hash_type {
                    ScriptHashType::Type => dep
                        .cell
                        .type_scripts()
                        .any(|script| script.calc_script_hash() == expected),
                    _ => dep.cell.data_hash() == &expected,
                };
                matches
                    .then_some(0)
                    .context("missing Script code dependency")
            }
            deps => self
                .programs
                .get_or_init(|| ProgramIndex::build(deps))
                .lookup(hash_type, &expected),
        }
    }
}

/// Shared by the Script Groups that actually execute against several Code deps.
#[derive(Clone, Debug, Default)]
struct ProgramIndex {
    by_data: HashMap<Byte32, usize>,
    by_type: HashMap<Byte32, CodeMatch>,
}

impl ProgramIndex {
    fn build(deps: &[LiveCell]) -> Self {
        let mut programs = Self::default();
        for (index, dep) in deps.iter().enumerate() {
            let data_hash = dep.cell.data_hash();
            programs.by_data.entry(data_hash.clone()).or_insert(index);
            for script in dep.cell.type_scripts() {
                programs
                    .by_type
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
        programs
    }

    fn lookup(&self, hash_type: ScriptHashType, expected: &Byte32) -> Result<usize> {
        match hash_type {
            ScriptHashType::Type => match self.by_type.get(expected) {
                Some(CodeMatch::Unique(_, index)) => Ok(*index),
                Some(CodeMatch::Ambiguous) => bail!("ambiguous Script code dependency"),
                None => bail!("missing Script code dependency"),
            },
            _ => self
                .by_data
                .get(expected)
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

/// Derive canonical runtime groups from all concrete input and output Cells.
pub fn derive_script_groups<'a>(
    inputs: impl IntoIterator<Item = &'a Cell>,
    outputs: impl IntoIterator<Item = &'a Cell>,
) -> Result<Vec<ScriptGroup>> {
    let mut groups = BTreeMap::new();
    for (index, input) in inputs.into_iter().enumerate() {
        get_group(&mut groups, ScriptGroupKind::Lock, input.lock())
            .inputs
            .push(index);
        for script in input.type_scripts() {
            get_group(&mut groups, ScriptGroupKind::Type, script)
                .inputs
                .push(index);
        }
    }
    for (index, output) in outputs.into_iter().enumerate() {
        for script in output.type_scripts() {
            get_group(&mut groups, ScriptGroupKind::Type, script)
                .outputs
                .push(index);
        }
    }
    ensure!(groups.len() <= MAX_SCRIPT_GROUPS, "too many Script Groups");
    Ok(groups.into_values().collect())
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
    fn load<T: AsRef<Cell>>(
        &mut self,
        point: &OutPoint,
        live_cell: &mut impl FnMut(&OutPoint) -> Result<T>,
    ) -> Result<T> {
        self.references += 1;
        ensure!(
            self.references <= MAX_CELL_REFERENCES,
            "resolved reference count exceeds limit"
        );
        let cell = live_cell(point)?;
        self.bytes += cell.as_ref().as_slice().len();
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
            .set(vec![
                cell.encoded.type_scripts().get(1).unwrap(),
                cell.encoded.type_scripts().get(0).unwrap(),
            ])
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
    fn validates_wire_output_scripts_at_the_packed_size_limit() -> Result<()> {
        let max_args = MAX_SCRIPT_BYTES - Script::default().as_slice().len();
        let sized_script = |length, address| {
            script(1)
                .as_builder()
                .hash_type(address)
                .args(Bytes::from(vec![0; length]))
                .build()
        };
        let exact = sized_script(max_args, ScriptHashType::Data);
        assert_eq!(exact.as_slice().len(), MAX_SCRIPT_BYTES);
        let cell = Cell::new(1, exact.clone(), vec![exact], Bytes::new())?;
        let tx = Transaction::new(
            vec![],
            vec![],
            vec![CellInput::new(OutPoint::new([1; 32].pack(), 0), None)],
            vec![cell.clone()],
            vec![],
        )?;
        assert!(Transaction::from_slice(tx.as_slice()).is_ok());
        for bad in [
            sized_script(max_args + 1, ScriptHashType::Data),
            sized_script(0, ScriptHashType::Data1),
        ] {
            let bad = encode_script(bad);
            let bad_lock = cell.encoded.as_builder().lock(bad.clone()).build();
            let bad_type = cell
                .encoded
                .as_builder()
                .type_scripts(wire::ScriptVec::new_builder().set(vec![bad]).build())
                .build();
            for output in [bad_lock, bad_type] {
                assert!(Cell::from_slice(output.as_slice()).is_err());
                let raw = tx
                    .encoded
                    .raw()
                    .as_builder()
                    .outputs(wire::CellVec::new_builder().set(vec![output]).build())
                    .build();
                let invalid_tx = tx.encoded.as_builder().raw(raw).build();
                assert!(Transaction::from_slice(invalid_tx.as_slice()).is_err());
            }
        }
        Ok(())
    }

    #[test]
    fn rejects_duplicate_inputs_and_dependencies() -> Result<()> {
        let input = CellInput::new(OutPoint::new([1; 32].pack(), 0), None);
        let dep = CellDep::new_builder()
            .out_point(OutPoint::new([2; 32].pack(), 0))
            .build();
        let header = [3; 32].pack();
        let make = |deps, headers, inputs| Transaction::new(deps, headers, inputs, vec![], vec![]);
        make(vec![dep.clone()], vec![header.clone()], vec![input.clone()])?;
        assert!(make(vec![], vec![], vec![input.clone(), input.clone()]).is_err());
        assert!(make(vec![dep.clone(), dep], vec![], vec![input.clone()]).is_err());
        assert!(make(vec![], vec![header.clone(), header], vec![input]).is_err());
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
        let mut edit = tree.edit(tree.empty_root(), |_| Ok(None::<Vec<u8>>));
        edit.set(key, Some(record::cell_value(&tree, &point, &record)))?;
        let (_, proof) = edit.prove(&key)?;
        let encoded_path = proof.encode()?;
        let proof = patricia::Proof::decode(&encoded_path)?;
        assert!(patricia::Proof::decode(&encoded_path[..encoded_path.len() - 1]).is_err());
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
        let receipts = vec![
            block::Receipt {
                fee: 0,
                cycles: 1,
                protocol_result: Bytes::new(),
            },
            block::Receipt {
                fee: 1,
                cycles: 2,
                protocol_result: Bytes::new(),
            },
        ];
        let receipt_tree = proof::Tree::receipts(network.clone(), &receipts)?;
        assert!(
            proof
                .encode_execution_pair(&receipt_tree.prove(0)?)
                .is_err()
        );
        let execution = proof.encode_execution_pair(&receipt_tree.prove(1)?)?;
        let (transaction, receipt) = proof::InclusionProof::decode_execution_pair(&execution)?;
        assert!(transaction.verify_execution(
            &network,
            &block::ExecutionCommitment {
                transaction_root: transactions.root(),
                receipt_root: receipt_tree.root(),
            },
            &tx,
            &receipt,
            &receipts[1],
        ));
        assert!(
            proof::InclusionProof::decode_execution_pair(&execution[..execution.len() - 1])
                .is_err()
        );
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
        let parent = Header {
            context: HeaderContext::after(&network, &origin, 124, Byte32::default())?,
            ..origin.clone()
        };
        let block = BlockContext::after(
            &network,
            &parent,
            125,
            Byte32::default(),
            CapacityPolicy::default(),
        )?;
        assert_eq!(&block.candidate().parent, &parent.id(&network));
        assert!(
            BlockContext::after(
                &Byte32::default(),
                &parent,
                125,
                Byte32::default(),
                CapacityPolicy::default(),
            )
            .is_err()
        );
        assert!(
            BlockContext::after(
                &network,
                &parent,
                123,
                Byte32::default(),
                CapacityPolicy::default(),
            )
            .is_err()
        );
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
            let id = tx.id(&network);
            let resolved = ResolvedTransaction::resolve(
                tx,
                &block,
                |_| {
                    Ok(LiveCell {
                        cell: cell.clone(),
                        origin: Origin::new(&network, &origin.context),
                    })
                },
                |_| Ok(origin.clone()),
            )?;
            assert_eq!(resolved.execution_id(), id);
            Ok::<_, anyhow::Error>(resolved)
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

    #[test]
    fn resolves_code_lazily_and_preserves_type_ambiguity() -> Result<()> {
        let network = [7; 32].pack();
        let parent = Header {
            context: HeaderContext {
                height: 0,
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
        let block = BlockContext::after(
            &network,
            &parent,
            124,
            Byte32::default(),
            CapacityPolicy::default(),
        )?;
        let origin = Origin::new(&network, &parent.context);
        let lock = script(1);
        let code_type = script(2);
        let by_type = Script::new_builder()
            .code_hash(code_type.calc_script_hash())
            .hash_type(ScriptHashType::Type)
            .build();
        let by_data = Script::new_builder().code_hash(blake2b_256(b"ELF")).build();
        let alternate = Script::new_builder()
            .code_hash(blake2b_256(b"alternative ELF"))
            .build();
        let capacity = 100_000_000_000;
        let cells = [
            Cell::new(capacity, lock.clone(), vec![], Bytes::new())?,
            Cell::new(
                capacity,
                lock.clone(),
                vec![code_type.clone()],
                Bytes::from_static(b"ELF"),
            )?,
            Cell::new(
                capacity,
                lock.clone(),
                vec![code_type.clone()],
                Bytes::from_static(b"ELF"),
            )?,
            Cell::new(
                capacity,
                lock.clone(),
                vec![code_type],
                Bytes::from_static(b"alternative ELF"),
            )?,
        ];
        let point = |index| OutPoint::new([1; 32].pack(), index);
        let resolve = |dep_count| {
            let tx = Transaction::new(
                (1..=dep_count)
                    .map(|index| CellDep::new_builder().out_point(point(index)).build())
                    .collect(),
                vec![],
                vec![CellInput::new(point(0), None)],
                vec![cells[0].clone()],
                vec![],
            )?;
            ResolvedTransaction::resolve(
                tx,
                &block,
                |point| {
                    let index: usize = point.index().unpack();
                    Ok(LiveCell {
                        cell: cells[index].clone(),
                        origin: origin.clone(),
                    })
                },
                |_| bail!("unexpected Header Dep"),
            )
        };
        let absent = resolve(0)?;
        assert!(absent.program_index(&by_data).is_err());

        let single = resolve(1)?;
        assert_eq!(single.program_index(&by_data)?, 0);
        assert_eq!(single.program_index(&by_type)?, 0);
        assert!(single.program_index(&alternate).is_err());
        assert!(single.programs.get().is_none());

        let matching = resolve(2)?;
        let _ = matching.script_cache_key();
        assert!(matching.programs.get().is_none());
        assert_eq!(matching.program_index(&by_type)?, 0);
        assert_eq!(matching.program_index(&by_data)?, 0);

        let ambiguous = resolve(3)?;
        assert_eq!(
            ambiguous.program_index(&by_type).unwrap_err().to_string(),
            "ambiguous Script code dependency"
        );
        assert_eq!(ambiguous.program_index(&by_data)?, 0);
        assert_eq!(ambiguous.program_index(&alternate)?, 2);
        Ok(())
    }
}
