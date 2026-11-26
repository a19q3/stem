use std::{collections::HashMap, path::Path, process::Command, sync::Arc};

use anyhow::{Context, Result, ensure};
use ckb_crypto::secp::Privkey;
use ckb_hash::blake2b_256;
use ckb_types::{
    bytes::Bytes,
    core::{DepType, ScriptHashType},
    packed::{Byte32, CellDep, OutPoint, Script},
    prelude::*,
};
use stem_ledger::{
    BlockContext, Cell, CellInput, GroupAction, GroupMessage, GroupWitness, LiveCell, Origin,
    ResolvedTransaction, ScriptGroupKind, Transaction,
    block::{Header, HeaderContext, StateCommitment},
};
use stem_vm::{VerificationCache, verify};

#[path = "../../contracts/test_support.rs"]
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
        let code = contract::native_lock(&Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))?;
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
        self.seal(tx)
    }

    fn seal(&self, tx: Transaction) -> Result<Transaction> {
        let digest = self.signing_digest(&tx)?;
        let sig = self.key.sign_recoverable(&digest.unpack())?;
        tx.with_witnesses(vec![GroupWitness {
            group_index: 0,
            proof_data: Bytes::new(),
            seal: Bytes::from(sig.serialize()),
        }])
    }

    fn signing_digest(&self, tx: &Transaction) -> Result<Byte32> {
        tx.signing_digest(
            &self.id,
            0,
            &self.lock,
            |point| {
                self.cells
                    .get(point)
                    .cloned()
                    .context("missing signing Cell")
            },
            |_| anyhow::bail!("unexpected Header Dep"),
        )
    }

    fn resolve(&self, id: &Byte32, tx: Transaction) -> Result<Arc<ResolvedTransaction>> {
        self.resolve_at(id, tx, 42)
    }

    fn resolve_at(
        &self,
        id: &Byte32,
        tx: Transaction,
        parent_time: u64,
    ) -> Result<Arc<ResolvedTransaction>> {
        let parent = Header {
            context: HeaderContext {
                height: 1,
                timestamp_ms: parent_time,
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
        let block = BlockContext::after(id, &parent, 43, Byte32::default(), Default::default())?;
        let origin = Origin::new(
            id,
            &HeaderContext {
                height: 0,
                timestamp_ms: 1,
                ..parent.context.clone()
            },
        );
        let tx = ResolvedTransaction::resolve(
            tx,
            &block,
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
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../contracts/examples/type_rules.c");
    let compiler = std::env::var_os("STEM_RISCV_CC").unwrap_or_else(|| "clang".into());
    let mut programs = Vec::new();
    for rule in (1..=12).chain(14..=15) {
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
            .context("compile the Stem examples with clang and lld")?;
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
fn block_context_uses_committed_parent_time_and_partitions_cache() -> Result<()> {
    let mut network = Network::new()?;
    let binaries = build_contracts(&mut network)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;

    // Exercise each read in isolation, both on the original VM and via Spawn
    // or Exec. Rule 14 is Cell Dep 13 because rule 13 was not built.
    for (program, args) in [
        (10, Vec::new()),
        (12, Vec::new()),
        (13, Vec::new()),
        (6, 13u32.to_le_bytes().to_vec()),
        (8, 14u32.to_le_bytes().to_vec()),
    ] {
        let type_script = script(&binaries[program], &args);
        let cell = network.output(vec![type_script], Bytes::new())?;
        let input = network.add_input(cell.clone());
        let tx = network.signed(vec![input], vec![cell])?;
        let accepted = network.resolve_at(&network.id, tx.clone(), 42)?;
        let rejected = network.resolve_at(&network.id, tx, 41)?;
        // The candidate time is 43 in both cases. The resolved inputs,
        // including their creation origins, are identical.
        assert_eq!(accepted.script_cache_key(), rejected.script_cache_key());
        assert_ne!(accepted.parent_id(), rejected.parent_id());
        let cache = VerificationCache::new(4);
        let expected = verify(accepted.clone(), CYCLE_LIMIT, false)?;
        pool.install(|| -> Result<()> {
            assert_eq!(cache.verify(accepted.clone(), CYCLE_LIMIT, true)?, expected);
            assert!(cache.verify(rejected, CYCLE_LIMIT, true).is_err());
            assert_eq!(cache.verify(accepted, expected, true)?, expected);
            Ok(())
        })?;
    }
    Ok(())
}

#[test]
fn payment_verification_reuses_cycles_after_the_parent_changes() -> Result<()> {
    let mut network = Network::new()?;
    let output = network.output(vec![], Bytes::new())?;
    let input = network.add_input(output.clone());
    let signed = network.signed(vec![input], vec![output])?;
    let first = network.resolve_at(&network.id, signed.clone(), 41)?;
    let second = network.resolve_at(&network.id, signed.clone(), 42)?;
    assert_eq!(first.script_cache_key(), second.script_cache_key());
    assert_ne!(first.parent_id(), second.parent_id());
    let cache = VerificationCache::new(4);
    let expected = verify(second.clone(), CYCLE_LIMIT, false)?;
    assert_eq!(cache.verify(first, CYCLE_LIMIT, true)?, expected);
    assert_eq!(cache.verify(second.clone(), expected, true)?, expected);
    assert!(cache.verify(second, expected - 1, true).is_err());
    let unsigned = signed.with_witnesses(Vec::new())?;
    assert_eq!(signed.id(&network.id), unsigned.id(&network.id));
    assert!(
        cache
            .verify(
                network.resolve_at(&network.id, unsigned, 42)?,
                CYCLE_LIMIT,
                true
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn native_group_message_is_signed_and_owned_by_a_derived_group() -> Result<()> {
    let mut network = Network::new()?;
    let programs = build_contracts(&mut network)?;
    let rule = script(&programs[11], b"");
    let cell = network.output(vec![rule], Bytes::new())?;
    let input = network.add_input(cell.clone());
    let draft = Transaction::new(
        network.deps.clone(),
        vec![],
        vec![input],
        vec![cell],
        vec![],
    )?;
    let message = GroupMessage {
        group_index: 1,
        actions: vec![GroupAction {
            kind: 7,
            data: Bytes::from_static(b"open"),
        }],
    };
    assert!(
        draft
            .with_messages(vec![message.clone(), message.clone()])
            .is_err()
    );
    let wrong = draft.with_messages(vec![GroupMessage {
        group_index: 2,
        ..message.clone()
    }])?;
    assert!(network.resolve(&network.id, wrong).is_err());
    let tx = draft.with_messages(vec![message])?;
    assert_ne!(tx.id(&network.id), draft.id(&network.id));
    let signed = network.seal(tx)?;
    verify(
        network.resolve(&network.id, signed.clone())?,
        CYCLE_LIMIT,
        true,
    )?;
    let unsigned_message = draft.with_witnesses(signed.witnesses().collect())?;
    let error = verify(
        network.resolve(&network.id, unsigned_message)?,
        CYCLE_LIMIT,
        false,
    )
    .unwrap_err();
    assert!(error.to_string().starts_with("Lock Script Group"));
    let no_message = network.seal(draft)?;
    let error = verify(
        network.resolve(&network.id, no_message)?,
        CYCLE_LIMIT,
        false,
    )
    .unwrap_err();
    assert!(error.to_string().contains("exited with 35"));
    Ok(())
}

#[test]
fn complete_authorization_binds_input_data_and_dep_group_members() -> Result<()> {
    let mut network = Network::new()?;
    let code_point = network.deps[0].out_point();
    let group_point = OutPoint::new([81; 32].pack(), 0);
    let members: ckb_types::packed::OutPointVec = vec![code_point.clone()].pack();
    let group = network.output(vec![], members.as_bytes())?;
    network.cells.insert(group_point.clone(), group);
    network.deps = vec![
        CellDep::new_builder()
            .out_point(group_point.clone())
            .dep_type(DepType::DepGroup)
            .build(),
    ];
    let cell = network.output(vec![], Bytes::new())?;
    let input = network.add_input(cell.clone());
    let point = input.previous_output.clone();
    let signed = network.signed(vec![input], vec![cell])?;
    let valid = network.resolve(&network.id, signed.clone())?;
    let cache = VerificationCache::new(4);
    cache.verify(valid, CYCLE_LIMIT, false)?;

    let bad_input = network.output(vec![], Bytes::from_static(b"changed input data"))?;
    let original_input = network.cells.insert(point.clone(), bad_input).unwrap();
    let changed = network.resolve(&network.id, signed.clone())?;
    assert!(cache.verify(changed, CYCLE_LIMIT, true).is_err());
    network.cells.insert(point, original_input);

    // Keep the same ELF and program hash. The lock must reject different
    // resolved Cell contents even when the program selected by the host stays the same.
    let original_code = network.cells[&code_point].clone();
    let changed = Cell::new(
        original_code.capacity() + 1,
        original_code.lock(),
        vec![],
        original_code.data(),
    )?;
    network.cells.insert(code_point.clone(), changed);
    assert!(
        verify(
            network.resolve(&network.id, signed.clone())?,
            CYCLE_LIMIT,
            false
        )
        .is_err()
    );
    network.cells.insert(code_point, original_code);

    let dep_group = &network.cells[&group_point];
    let changed = Cell::new(
        dep_group.capacity() + 1,
        dep_group.lock(),
        vec![],
        dep_group.data(),
    )?;
    network.cells.insert(group_point, changed);
    assert!(
        cache
            .verify(network.resolve(&network.id, signed)?, CYCLE_LIMIT, true)
            .is_err()
    );
    Ok(())
}

#[test]
fn separate_lock_groups_collect_seals_independently() -> Result<()> {
    let mut network = Network::new()?;
    let sponsor = Privkey::from_slice(&[60; 32]);
    let key_hash = blake2b_256(sponsor.pubkey()?.serialize());
    let sponsor_lock = network
        .lock
        .clone()
        .as_builder()
        .args(Bytes::copy_from_slice(&key_hash[..20]))
        .build();
    let paid = Cell::new(CAPACITY, sponsor_lock.clone(), vec![], Bytes::new())?;
    let transfer = network.output(vec![], Bytes::new())?;
    let inputs = vec![network.add_input(transfer.clone()), network.add_input(paid)];
    let change = Cell::new(CAPACITY - 10, sponsor_lock.clone(), vec![], Bytes::new())?;
    let draft = Transaction::new(
        network.deps.clone(),
        vec![],
        inputs,
        vec![transfer, change],
        vec![],
    )?;
    let mut signers = [(&network.lock, &network.key), (&sponsor_lock, &sponsor)];
    signers.sort_by(|left, right| left.0.as_slice().cmp(right.0.as_slice()));
    let digest = |tx: &Transaction, group, lock: &Script| {
        tx.signing_digest(
            &network.id,
            group,
            lock,
            |point| network.cells.get(point).cloned().context("missing Cell"),
            |_| anyhow::bail!("unexpected Header Dep"),
        )
    };
    assert!(digest(&draft, 1, signers[0].0).is_err());
    let seal = |group, hash: Byte32, key: &Privkey| -> Result<GroupWitness> {
        Ok(GroupWitness {
            group_index: group,
            proof_data: Bytes::new(),
            seal: Bytes::from(key.sign_recoverable(&hash.unpack())?.serialize()),
        })
    };
    let first = seal(0, digest(&draft, 0, signers[0].0)?, signers[0].1)?;
    let with_first = draft.with_witnesses(vec![first.clone()])?;
    let second_digest = digest(&draft, 1, signers[1].0)?;
    assert_eq!(second_digest, digest(&with_first, 1, signers[1].0)?);
    assert!(
        verify(
            network.resolve(&network.id, with_first)?,
            CYCLE_LIMIT,
            false
        )
        .is_err()
    );
    let signed = draft.with_witnesses(vec![first, seal(1, second_digest, signers[1].1)?])?;
    verify(network.resolve(&network.id, signed)?, CYCLE_LIMIT, true)?;
    Ok(())
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
    let digest = network.signing_digest(&tx)?;
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

#[test]
fn measure_type_groups_with_matched_payment_fixture() -> Result<()> {
    let mut network = Network::new()?;
    let programs = build_contracts(&mut network)?;
    let asset = script(&programs[0], b"asset");
    let collateral = script(&programs[1], b"asset");
    let policy = script(&programs[2], b"asset");
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    for types in [
        vec![],
        vec![asset.clone()],
        vec![asset.clone(), collateral.clone()],
        vec![asset, collateral, policy],
    ] {
        let count = types.len();
        let input_cell = network.asset(types.clone(), 25, 1)?;
        let input = network.add_input(input_cell);
        let tx = network.signed(vec![input], vec![network.asset(types, 25, 1)?])?;
        let bytes = tx.as_slice().len();
        let tx = network.resolve(&network.id, tx)?;
        let serial = verify(tx.clone(), CYCLE_LIMIT, false)?;
        let parallel = pool.install(|| verify(tx, CYCLE_LIMIT, true))?;
        assert_eq!(serial, parallel);
        println!("matched fixture: {count} Type Groups, {bytes} bytes, {serial} cycles");
    }
    Ok(())
}

#[path = "../../research/scheduling/resolved_jobs.rs"]
mod resolved_job_scheduling;
