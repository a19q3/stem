use std::{collections::HashMap, path::Path, process::Command, sync::Arc};

use anyhow::{Context, Result, ensure};
use ckb_crypto::secp::Privkey;
use ckb_hash::blake2b_256;
use ckb_script::fern::{VerificationCache, verify};
use ckb_types::{
    bytes::Bytes,
    core::ScriptHashType,
    packed::{Byte32, CellDep, OutPoint, Script},
    prelude::*,
};
use fern_ledger::{
    BlockContext, Cell, CellInput, GroupWitness, LiveCell, Origin, ResolvedTransaction,
    ScriptGroupKind, Transaction, block::HeaderContext,
};

#[path = "../../../contracts/test_support.rs"]
mod contract;

const CAPACITY: u64 = 100_000_000_000;
const CYCLE_LIMIT: u64 = 100_000_000;

struct Network {
    id: Byte32,
    key: Privkey,
    lock: Script,
    cells: HashMap<OutPoint, Cell>,
    deps: Vec<CellDep>,
}

impl Network {
    fn new() -> Result<Self> {
        let key = Privkey::from_slice(&[42; 32]);
        let code = contract::native_lock(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))?;
        let key_hash = blake2b_256(key.pubkey()?.serialize());
        let lock = script(&code, &key_hash[..20]);
        let mut network = Self {
            id: [1; 32].pack(),
            key,
            lock,
            cells: HashMap::new(),
            deps: Vec::new(),
        };
        network.add_dep(code)?;
        Ok(network)
    }

    fn add_dep(&mut self, data: Bytes) -> Result<()> {
        let point = OutPoint::new([2; 32].pack(), self.deps.len() as u32);
        let occupied = Cell::minimum_capacity(&self.lock, &[], data.len())?;
        let cell = Cell::new(occupied, self.lock.clone(), vec![], data)?;
        self.cells.insert(point.clone(), cell);
        self.deps
            .push(CellDep::new_builder().out_point(point).build());
        Ok(())
    }

    fn add_input(&mut self, cell: Cell) -> CellInput {
        let point = OutPoint::new([3; 32].pack(), self.cells.len() as u32);
        self.cells.insert(point.clone(), cell);
        CellInput::new(point, None)
    }

    fn output(&self, types: Vec<Script>, data: impl Into<Bytes>) -> Result<Cell> {
        Cell::new(CAPACITY, self.lock.clone(), types, data.into())
    }

    fn asset(&self, scripts: Vec<Script>, amount: u64, classification: u8) -> Result<Cell> {
        let mut data = amount.to_le_bytes().to_vec();
        data.push(classification);
        self.output(scripts, data)
    }

    fn signed(&self, inputs: Vec<CellInput>, outputs: Vec<Cell>) -> Result<Transaction> {
        let tx = Transaction::new(self.deps.clone(), vec![], inputs, outputs, vec![])?;
        let digest = tx.lock_digest(&self.id, &self.lock);
        let sig = self.key.sign_recoverable(&digest.unpack())?;
        tx.with_witnesses(vec![GroupWitness {
            group_index: 0,
            proof_data: Bytes::new(),
            seal: Bytes::from(sig.serialize()),
        }])
    }

    fn resolve(&self, id: &Byte32, tx: Transaction) -> Result<Arc<ResolvedTransaction>> {
        let candidate = HeaderContext {
            height: 1,
            timestamp_ms: 0,
            parent: Byte32::default(),
            transaction_root: Byte32::default(),
        };
        let origin = Origin::new(
            id,
            &HeaderContext {
                height: 0,
                ..candidate.clone()
            },
        );
        let tx = ResolvedTransaction::resolve(
            id.clone(),
            tx,
            BlockContext {
                candidate: &candidate,
                parent_timestamp_ms: 0,
                capacity: Default::default(),
            },
            |point| {
                let cell = self
                    .cells
                    .get(point)
                    .cloned()
                    .context("missing test outpoint")?;
                Ok(LiveCell {
                    cell,
                    origin: origin.clone(),
                })
            },
            |_| anyhow::bail!("unknown test header"),
        )?;
        Ok(Arc::new(tx))
    }
}

fn script(code: &[u8], args: &[u8]) -> Script {
    Script::new_builder()
        .code_hash(blake2b_256(code))
        .hash_type(ScriptHashType::Data)
        .args(Bytes::copy_from_slice(args))
        .build()
}

fn build_contracts(network: &mut Network) -> Result<Vec<Bytes>> {
    let tmp = tempfile::tempdir()?;
    let source =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/examples/type_rules.c");
    let compiler = std::env::var_os("FERN_RISCV_CC").unwrap_or_else(|| "clang".into());
    let mut programs = Vec::new();
    for rule in 1..=10 {
        let path = tmp.path().join(format!("rule-{rule}"));
        let output = Command::new(&compiler)
            .args([
                "--target=riscv64-unknown-elf",
                "-march=rv64im",
                "-mabi=lp64",
                "-ffreestanding",
                "-fno-builtin",
                "-nostdlib",
                "-fuse-ld=lld",
                "-O2",
                "-Wl,--no-relax",
                "-Wl,-e,_start",
            ])
            .arg(format!("-DRULE={rule}"))
            .arg(&source)
            .arg("-o")
            .arg(&path)
            .output()
            .context("compile the Fern examples with clang and lld")?;
        ensure!(
            output.status.success(),
            "RISC-V compile failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let program = Bytes::from(std::fs::read(path)?);
        network.add_dep(program.clone())?;
        programs.push(program);
    }
    Ok(programs)
}

#[test]
fn multi_asset_settlement_runs_every_group_with_real_signature() -> Result<()> {
    let mut network = Network::new()?;
    let binaries = build_contracts(&mut network)?;
    let asset_a = script(&binaries[0], b"a");
    let asset_b = script(&binaries[0], b"b");
    let collateral = script(&binaries[1], b"");
    let policy = script(&binaries[2], b"");
    let settlement = script(&binaries[5], b"");

    let a = network.add_input(network.asset(
        vec![asset_a.clone(), collateral.clone(), settlement.clone()],
        40,
        3,
    )?);
    let b = network.add_input(network.asset(
        vec![policy.clone(), asset_b.clone(), settlement.clone()],
        25,
        1,
    )?);
    let inputs = vec![a, b];
    let outputs = vec![
        network.asset(
            vec![asset_b.clone(), settlement.clone(), policy.clone()],
            25,
            1,
        )?,
        network.asset(
            vec![settlement.clone(), collateral.clone(), asset_a.clone()],
            40,
            4,
        )?,
    ];
    let valid = network.signed(inputs.clone(), outputs)?;
    let tx = network.resolve(&network.id, valid.clone())?;
    assert_eq!(tx.groups().len(), 6);
    let shared = tx
        .groups()
        .iter()
        .find(|group| group.script == settlement)
        .unwrap();
    assert_eq!(shared.kind, ScriptGroupKind::Type);
    assert_eq!(shared.inputs, [0, 1]);
    assert_eq!(shared.outputs, [0, 1]);

    let serial = verify(tx.clone(), CYCLE_LIMIT, false)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let parallel = pool.install(|| verify(tx.clone(), CYCLE_LIMIT, true))?;
    assert_eq!(serial, parallel);
    let cache = VerificationCache::new(8);
    assert_eq!(cache.verify(tx.clone(), CYCLE_LIMIT, false)?, serial);
    assert_eq!(cache.verify(tx.clone(), serial, false)?, serial);
    assert!(cache.verify(tx.clone(), serial - 1, false).is_err());
    println!("five Type Script Groups across two assets: {serial} cycles");
    assert_eq!(pool.install(|| verify(tx.clone(), serial, true))?, serial);
    assert!(verify(tx.clone(), serial - 1, false).is_err());
    assert!(pool.install(|| verify(tx, serial - 1, true)).is_err());

    let stripped = network.signed(
        inputs.clone(),
        vec![
            network.asset(
                vec![asset_b.clone(), settlement.clone(), policy.clone()],
                25,
                1,
            )?,
            network.asset(vec![settlement.clone(), asset_a.clone()], 40, 4)?,
        ],
    )?;
    let error = verify(network.resolve(&network.id, stripped)?, CYCLE_LIMIT, false).unwrap_err();
    assert!(format!("{error:#}").contains("Type Script Group"));

    let inflated = network.signed(
        inputs,
        vec![
            network.asset(vec![asset_b, settlement.clone(), policy], 24, 1)?,
            network.asset(vec![settlement, collateral, asset_a], 41, 4)?,
        ],
    )?;
    assert!(verify(network.resolve(&network.id, inflated)?, CYCLE_LIMIT, false).is_err());
    assert!(
        verify(
            network.resolve(&[8; 32].pack(), valid.clone())?,
            CYCLE_LIMIT,
            false
        )
        .is_err()
    );
    let unsigned = valid.with_witnesses(vec![])?;
    assert_eq!(unsigned.id(&network.id), valid.id(&network.id));
    assert_ne!(
        unsigned.commitment(&network.id),
        valid.commitment(&network.id)
    );
    assert!(
        cache
            .verify(network.resolve(&network.id, unsigned)?, CYCLE_LIMIT, false)
            .is_err()
    );
    Ok(())
}

#[test]
fn identity_lifecycle_and_single_type_asset_are_executed() -> Result<()> {
    let mut network = Network::new()?;
    let binaries = build_contracts(&mut network)?;
    let asset = script(&binaries[0], b"asset");
    let token = network.add_input(network.asset(vec![asset.clone()], 7, 0)?);
    let tx = network.signed(vec![token], vec![network.asset(vec![asset], 7, 0)?])?;
    let cycles = verify(network.resolve(&network.id, tx)?, CYCLE_LIMIT, false)?;
    println!("single asset Type Script Group: {cycles} cycles");

    let funding = network.add_input(network.output(vec![], Bytes::new())?);
    let identity = script(&binaries[3], funding.previous_output.as_slice());
    let lifecycle = script(&binaries[4], b"");
    let types = vec![lifecycle.clone(), identity.clone()];
    let create = network.signed(
        vec![funding.clone()],
        vec![network.output(types.clone(), vec![0])?],
    )?;
    verify(network.resolve(&network.id, create)?, CYCLE_LIMIT, false)?;
    let wrong = script(&binaries[3], &[0; 36]);
    let forge = network.signed(
        vec![funding],
        vec![network.output(vec![wrong, lifecycle], vec![0])?],
    )?;
    assert!(verify(network.resolve(&network.id, forge)?, CYCLE_LIMIT, false).is_err());
    let object = network.add_input(network.output(types.clone(), vec![0])?);
    let advance = network.signed(
        vec![object.clone()],
        vec![network.output(types.clone(), vec![1])?],
    )?;
    let cycles = verify(network.resolve(&network.id, advance)?, CYCLE_LIMIT, false)?;
    println!("identity and lifecycle Type Script Groups: {cycles} cycles");
    let jump = network.signed(vec![object], vec![network.output(types, vec![2])?])?;
    assert!(verify(network.resolve(&network.id, jump)?, CYCLE_LIMIT, false).is_err());
    Ok(())
}

#[test]
fn spawn_and_exec_read_the_same_native_script_group() -> Result<()> {
    let mut network = Network::new()?;
    let binaries = build_contracts(&mut network)?;
    // The first dependency is the native secp256k1 Lock Script.
    let child_index = 8u32.to_le_bytes();
    for rule in [7, 9] {
        let type_script = script(&binaries[rule - 1], &child_index);
        let cell = network.output(vec![type_script], Bytes::new())?;
        let input = network.add_input(cell.clone());
        let tx = network.signed(vec![input], vec![cell])?;
        verify(network.resolve(&network.id, tx)?, CYCLE_LIMIT, true)?;
    }
    Ok(())
}

#[test]
fn native_witness_proofs_and_seals_are_bound_to_the_resolved_groups() -> Result<()> {
    let mut network = Network::new()?;
    let binaries = build_contracts(&mut network)?;
    let proof_script = script(&binaries[9], b"");
    let cell = network.output(vec![proof_script], Bytes::new())?;
    let input = network.add_input(cell.clone());
    let proof = GroupWitness {
        group_index: 1,
        proof_data: Bytes::copy_from_slice(&42u32.to_le_bytes()),
        seal: Bytes::new(),
    };
    let tx = Transaction::new(
        network.deps.clone(),
        vec![],
        vec![input],
        vec![cell],
        vec![proof.clone()],
    )?;
    let digest = tx.lock_digest(&network.id, &network.lock);
    let seal = GroupWitness {
        group_index: 0,
        proof_data: Bytes::new(),
        seal: Bytes::from(network.key.sign_recoverable(&digest.unpack())?.serialize()),
    };
    let signed = tx.with_witnesses(vec![seal.clone(), proof.clone()])?;
    verify(
        network.resolve(&network.id, signed.clone())?,
        CYCLE_LIMIT,
        true,
    )?;
    assert!(
        signed
            .with_witnesses(vec![proof.clone(), seal.clone()])
            .is_err()
    );
    assert!(
        signed
            .with_witnesses(vec![seal.clone(), seal.clone()])
            .is_err()
    );
    assert!(
        signed
            .with_witnesses(vec![GroupWitness {
                group_index: 0,
                proof_data: Bytes::new(),
                seal: Bytes::new()
            }])
            .is_err()
    );
    let absent = signed.with_witnesses(vec![
        seal.clone(),
        GroupWitness {
            group_index: 2,
            ..proof.clone()
        },
    ])?;
    assert!(network.resolve(&network.id, absent).is_err());
    let tampered = signed.with_witnesses(vec![
        seal,
        GroupWitness {
            proof_data: Bytes::copy_from_slice(&41u32.to_le_bytes()),
            ..proof
        },
    ])?;
    assert!(verify(network.resolve(&network.id, tampered)?, CYCLE_LIMIT, false).is_err());
    Ok(())
}
