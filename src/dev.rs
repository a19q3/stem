use anyhow::{Context, Result, ensure};
use ckb_chain_spec::{OUTPUT_INDEX_SECP256K1_BLAKE160_SIGHASH_ALL, OUTPUT_INDEX_SECP256K1_DATA};
use ckb_crypto::secp::Privkey;
use ckb_hash::{blake2b_256, new_blake2b};
use ckb_jsonrpc_types::JsonBytes;
use ckb_types::{
    bytes::Bytes,
    core::{Capacity, DepType, ScriptHashType, TransactionBuilder, TransactionView},
    packed::{Byte32, CellDep, CellInput, CellOutput, OutPoint, OutPointVec, Script, WitnessArgs},
    prelude::*,
};
use serde::Serialize;

use crate::genesis::{Genesis, SYSTEM_LOCK, script_consensus};

#[derive(Serialize)]
pub struct Fixture {
    pub genesis: Genesis,
    pub transactions: Vec<JsonBytes>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub funding_transactions: Vec<JsonBytes>,
    pub funding_tx: Option<String>,
    pub deposit_dep_group: String,
    pub xudt_dep: Option<ckb_jsonrpc_types::OutPoint>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum Workload {
    Payments,
    Xudt,
    Mixed,
    Atomic,
    Contended,
}

const MAX_PREFUNDED_PAYMENTS: u32 = 262_144;
const PAYMENT_OUTPUTS_PER_SPLIT: u32 = 2048;
const PAYMENT_FEE: u64 = 1000;

pub fn fixture(
    genesis: Genesis,
    payments: u32,
    workload: Workload,
    xudt: Option<Vec<u8>>,
    prefund: bool,
) -> Result<Fixture> {
    ensure!(
        genesis.allocations.is_empty(),
        "fixture expects no preexisting allocations"
    );
    ensure!(
        (payments == 0) == genesis.bridge.is_some(),
        "bridged fixtures require zero unbacked payments"
    );
    if prefund {
        ensure!(
            genesis.bridge.is_none()
                && matches!(workload, Workload::Payments)
                && payments <= MAX_PREFUNDED_PAYMENTS,
            "prefunding requires at most 262144 development CKB payments"
        );
    } else {
        ensure!(
            payments <= 65_536,
            "fixture exceeds the genesis payment limit"
        );
    }
    let needs_xudt = !matches!(workload, Workload::Payments | Workload::Contended)
        || genesis
            .bridge
            .as_ref()
            .is_some_and(|bridge| bridge.xudt.is_some());
    ensure!(
        needs_xudt == xudt.is_some(),
        "typed fixtures require an xUDT ELF"
    );
    let consensus = script_consensus()?;
    let cellbase = consensus
        .genesis_block()
        .transaction(0)
        .context("missing dev cellbase")?;
    let lock = consensus
        .secp256k1_blake160_sighash_all_type_hash()
        .context("missing dev secp lock")?;
    let key = Privkey::from_slice(&[1u8; 32]);
    let recipient = Privkey::from_slice(&[2u8; 32]);
    let fund_lock = sighash_lock(&lock, &key)?;
    let payment_lock = sighash_lock(&lock, &recipient)?;
    let unspendable = SYSTEM_LOCK.clone();

    let code = [
        OUTPUT_INDEX_SECP256K1_BLAKE160_SIGHASH_ALL as usize,
        OUTPUT_INDEX_SECP256K1_DATA as usize,
    ]
    .into_iter()
    .try_fold(
        TransactionBuilder::default().header_dep(genesis.allocation_tag()),
        |tx, index| -> Result<_> {
            let output = cellbase
                .output(index)
                .context("missing dev system cell")?
                .as_builder()
                .lock(unspendable.clone())
                .build();
            let data = cellbase
                .outputs_data()
                .get(index)
                .context("missing system data")?;
            Ok(tx.output(output).output_data(data))
        },
    )?
    .build();

    let dep_data = OutPointVec::new_builder()
        .set(vec![
            OutPoint::new(code.hash(), 0),
            OutPoint::new(code.hash(), 1),
        ])
        .build();
    let deps = TransactionBuilder::default()
        .header_dep(genesis.allocation_tag())
        .output(
            CellOutput::new_builder()
                .lock(unspendable)
                .capacity(Capacity::bytes(200)?)
                .build(),
        )
        .output_data(Bytes::copy_from_slice(dep_data.as_slice()))
        .build();
    let dep_group = CellDep::new_builder()
        .dep_type(DepType::DepGroup)
        .out_point(OutPoint::new(deps.hash(), 0))
        .build();
    if prefund {
        return prefunded(
            genesis,
            [code, deps],
            payments,
            dep_group,
            fund_lock,
            payment_lock,
            &key,
        );
    }

    let xudt_code = xudt
        .map(|program| -> Result<_> {
            ensure!(
                program.starts_with(b"\x7fELF") && program.len() < 512 * 1024,
                "invalid xUDT ELF"
            );
            let bytes = Bytes::from(program);
            let output = CellOutput::new_builder()
                .lock(SYSTEM_LOCK.clone())
                .capacity(Capacity::bytes(bytes.len() + 41)?)
                .build();
            let tx = TransactionBuilder::default()
                .header_dep(genesis.allocation_tag())
                .output(output)
                .output_data(bytes.clone())
                .build();
            let dep = CellDep::new_builder()
                .out_point(OutPoint::new(tx.hash(), 0))
                .build();
            Ok((tx, dep, blake2b_256(&bytes)))
        })
        .transpose()?;
    let typed_cell = |asset: u8| -> Option<Script> {
        let (_, _, hash) = xudt_code.as_ref()?;
        let owner = blake2b_256([asset]);
        Some(
            Script::new_builder()
                .code_hash(*hash)
                .hash_type(ScriptHashType::Data1)
                .args(Bytes::copy_from_slice(&owner))
                .build(),
        )
    };
    let asset_a = typed_cell(1);
    let asset_b = typed_cell(2);
    let mut funding = TransactionBuilder::default().header_dep(genesis.allocation_tag());
    let mut groups = Vec::with_capacity(payments as usize);
    let mut cells = 0u32;
    for i in 0..payments {
        let types = match workload {
            Workload::Payments | Workload::Contended => vec![None],
            Workload::Xudt => vec![asset_a.clone()],
            Workload::Mixed if i % 2 == 0 => vec![None],
            Workload::Mixed => vec![asset_a.clone()],
            Workload::Atomic => vec![asset_a.clone(), asset_b.clone()],
        };
        let mut outputs = Vec::with_capacity(types.len());
        for type_ in types {
            let data = if type_.is_some() {
                Bytes::copy_from_slice(&10_000u128.to_le_bytes())
            } else {
                Bytes::new()
            };
            let capacity = Capacity::bytes(if type_.is_some() { 240 } else { 100 })?;
            let source = CellOutput::new_builder()
                .lock(fund_lock.clone())
                .capacity(capacity)
                .type_(type_)
                .build();
            let competitor = matches!(workload, Workload::Contended)
                .then_some(i % 4)
                .unwrap_or(0);
            if competitor == 0 {
                funding = funding.output(source.clone()).output_data(data.clone());
                cells += 1;
            }
            let dest = source
                .as_builder()
                .lock(payment_lock.clone())
                .capacity(capacity.safe_sub(1000u64 + u64::from(competitor))?)
                .build();
            outputs.push((cells - 1, dest, data));
        }
        groups.push(outputs);
    }
    let funding = funding.build();
    let transactions = groups
        .into_iter()
        .map(|group| {
            let mut tx = TransactionBuilder::default().cell_dep(dep_group.clone());
            if group.iter().any(|(_, out, _)| out.type_().is_some()) {
                tx = tx.cell_dep(xudt_code.as_ref().context("missing xUDT dep")?.1.clone());
            }
            for (index, output, data) in group {
                tx = tx
                    .input(CellInput::new(OutPoint::new(funding.hash(), index), 0))
                    .output(output)
                    .output_data(data);
            }
            sign_fixture_lock_group(&tx.build(), &key)
                .map(|tx| JsonBytes::from_bytes(tx.data().as_bytes()))
        })
        .collect::<Result<Vec<_>>>()?;
    let mut allocations = vec![code, deps.clone()];
    let mut xudt_dep = None;
    if let Some((tx, _, _)) = xudt_code {
        xudt_dep = Some(OutPoint::new(tx.hash(), 0).into());
        allocations.push(tx);
    }
    let funding_tx = (payments > 0).then(|| {
        let hash = format!("{:#x}", funding.hash());
        allocations.push(funding);
        hash
    });
    Fixture {
        genesis,
        transactions,
        funding_transactions: Vec::new(),
        funding_tx,
        deposit_dep_group: format!("{:#x}", deps.hash()),
        xudt_dep,
    }
    .with_allocations(allocations)
}

fn prefunded(
    genesis: Genesis,
    [code, deps]: [TransactionView; 2],
    payments: u32,
    dep_group: CellDep,
    fund_lock: Script,
    payment_lock: Script,
    key: &Privkey,
) -> Result<Fixture> {
    let splits = payments.div_ceil(PAYMENT_OUTPUTS_PER_SPLIT);
    let counts = (0..splits)
        .map(|split| (payments - split * PAYMENT_OUTPUTS_PER_SPLIT).min(PAYMENT_OUTPUTS_PER_SPLIT));
    let mut root = TransactionBuilder::default().header_dep(genesis.allocation_tag());
    for count in counts.clone() {
        let capacity = Capacity::bytes(100 * count as usize)?.safe_add(PAYMENT_FEE)?;
        root = root
            .output(
                CellOutput::new_builder()
                    .capacity(capacity)
                    .lock(fund_lock.clone())
                    .build(),
            )
            .output_data(Bytes::new());
    }
    let root = root.build();
    let funded = CellOutput::new_builder()
        .capacity(Capacity::bytes(100)?)
        .lock(fund_lock)
        .build();
    let paid = CellOutput::new_builder()
        .capacity(Capacity::bytes(100)?.safe_sub(PAYMENT_FEE)?)
        .lock(payment_lock)
        .build();
    let mut funding_transactions = Vec::with_capacity(splits as usize);
    let mut transactions = Vec::with_capacity(payments as usize);
    for (index, count) in counts.enumerate() {
        let split = TransactionBuilder::default()
            .cell_dep(dep_group.clone())
            .input(CellInput::new(OutPoint::new(root.hash(), index as u32), 0))
            .outputs((0..count).map(|_| funded.clone()))
            .outputs_data((0..count).map(|_| Bytes::new().pack()))
            .build();
        let split = sign_fixture_lock_group(&split, key)?;
        ensure!(
            split.data().as_slice().len() <= crate::genesis::MAX_TX_BYTES,
            "fixture split exceeds transaction byte limit"
        );
        funding_transactions.push(JsonBytes::from_bytes(split.data().as_bytes()));
        for outpoint in (0..count).map(|index| OutPoint::new(split.hash(), index)) {
            let payment = TransactionBuilder::default()
                .cell_dep(dep_group.clone())
                .input(CellInput::new(outpoint, 0))
                .output(paid.clone())
                .output_data(Bytes::new())
                .build();
            let payment = sign_fixture_lock_group(&payment, key)?;
            transactions.push(JsonBytes::from_bytes(payment.data().as_bytes()));
        }
    }
    Fixture {
        genesis,
        transactions,
        funding_transactions,
        funding_tx: Some(format!("{:#x}", root.hash())),
        deposit_dep_group: format!("{:#x}", deps.hash()),
        xudt_dep: None,
    }
    .with_allocations([code, deps, root])
}

impl Fixture {
    fn with_allocations(
        mut self,
        allocations: impl IntoIterator<Item = TransactionView>,
    ) -> Result<Self> {
        self.genesis.allocations = allocations
            .into_iter()
            .map(|tx| JsonBytes::from_bytes(tx.data().as_bytes()))
            .collect();
        self.genesis.validate()?;
        ensure!(
            serde_json::to_vec(&self.genesis)?.len() <= crate::genesis::MAX_GENESIS_BYTES,
            "generated genesis exceeds the application limit"
        );
        Ok(self)
    }

    pub fn add_governance(&mut self, lock: Script) -> Result<()> {
        let data = crate::governance::encode(&self.genesis.validators)?;
        self.genesis.governance = Some(self.add_capability(lock, data)?.into());
        self.genesis.validate()?;
        Ok(())
    }

    pub fn add_guardian_governance(&mut self, lock: Script) -> Result<()> {
        let bridge = self
            .genesis
            .bridge
            .as_ref()
            .context("guardian governance requires a bridge")?;
        let data = crate::governance::encode_guardians(&bridge.guardians)?;
        self.genesis.guardian_governance = Some(self.add_capability(lock, data)?.into());
        self.genesis.validate()?;
        Ok(())
    }

    fn add_capability(&mut self, lock: Script, data: Vec<u8>) -> Result<OutPoint> {
        let tx = TransactionBuilder::default()
            .header_dep(self.genesis.allocation_tag())
            .output(
                CellOutput::new_builder()
                    .lock(lock)
                    .capacity(Capacity::bytes(12_000)?)
                    .build(),
            )
            .output_data(Bytes::from(data))
            .build();
        let point = OutPoint::new(tx.hash(), 0);
        self.genesis
            .allocations
            .push(JsonBytes::from_bytes(tx.data().as_bytes()));
        Ok(point)
    }
}

fn sighash_lock(code_hash: &Byte32, key: &Privkey) -> Result<Script> {
    let args = blake2b_256(key.pubkey()?.serialize());
    Ok(Script::new_builder()
        .code_hash(code_hash.clone())
        .hash_type(ScriptHashType::Type)
        .args(Bytes::copy_from_slice(&args[..20]))
        .build())
}

pub fn sign_fixture_lock_group(tx: &TransactionView, key: &Privkey) -> Result<TransactionView> {
    let witness = WitnessArgs::new_builder()
        .lock(Some(Bytes::from_static(&[0; 65])).pack())
        .build();
    let mut digest = [0; 32];
    let mut hasher = new_blake2b();
    hasher.update(tx.hash().as_slice());
    hasher.update(&(witness.as_slice().len() as u64).to_le_bytes());
    hasher.update(witness.as_slice());
    for _ in 1..tx.inputs().len() {
        hasher.update(&0u64.to_le_bytes());
    }
    hasher.finalize(&mut digest);
    let lock = key.sign_recoverable(&digest.into())?.serialize();
    let witness = witness
        .as_builder()
        .lock(Some(Bytes::from(lock)).pack())
        .build();
    Ok(tx
        .as_advanced_builder()
        .set_witnesses(
            std::iter::once(witness.as_bytes().pack())
                .chain(std::iter::repeat_n(
                    ckb_types::packed::Bytes::default(),
                    tx.inputs().len() - 1,
                ))
                .collect(),
        )
        .build())
}
