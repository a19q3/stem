use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::ensure;
use ckb_jsonrpc_types::JsonBytes;
use serde_json::json;
use stem_ledger::{GroupAction, GroupMessage};

use super::*;

const DEBIT: u64 = 5_000_000_000;

pub(super) struct Swap {
    code: Bytes,
    inputs: [OutPoint; 3],
    outputs: [Cell; 3],
    deps: Vec<CellDep>,
}

impl Swap {
    pub(super) fn fund(fixture: &mut Fixture) -> Result<Self> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
        let code = contract::native_contract(workspace, "stem-intent-lock")?;
        let udt_code = contract::native_contract(workspace, "bridge-udt")?;
        let lock_hash = blake2b_256(&code);
        let udt_hash = blake2b_256(&udt_code);
        let mut deps = fixture.deps.clone();
        for (index, code) in [code.clone(), udt_code.clone(), udt_code]
            .into_iter()
            .enumerate()
        {
            let point = OutPoint::new([220; 32].pack(), index as u32);
            let bond = Cell::minimum_capacity(&fixture.lock, &[], code.len())?;
            fixture.spec.development_cells.push((
                point.clone(),
                Cell::new(bond, fixture.lock.clone(), vec![], code)?,
            ));
            if index < 2 {
                deps.push(CellDep::new_builder().out_point(point).build());
            }
        }
        let args = |index: usize| -> Result<Bytes> {
            let hash = blake2b_256(Privkey::from_slice(&key(index)).pubkey()?.serialize());
            let mut bytes = hash[..20].to_vec();
            bytes.extend_from_slice(&[index as u8; 32]);
            Ok(bytes.into())
        };
        let mut locks = Vec::new();
        for index in 0..3 {
            locks.push(
                Script::new_builder()
                    .code_hash(lock_hash)
                    .hash_type(ScriptHashType::Data)
                    .args(args(index)?)
                    .build(),
            );
        }
        let assets: Vec<_> = [1, 2]
            .map(|id| {
                Script::new_builder()
                    .code_hash(udt_hash)
                    .hash_type(ScriptHashType::Data)
                    .args(Bytes::from(vec![id; 32]))
                    .build()
            })
            .into();
        let make_asset = |owner: Script, asset: usize| {
            Cell::new(
                FUND,
                owner,
                vec![assets[asset].clone()],
                Bytes::copy_from_slice(&[40u128, 76][asset].to_le_bytes()),
            )
        };
        let sponsor = Cell::new(FUND, locks[0].clone(), vec![], Bytes::new())?;
        let first = make_asset(locks[1].clone(), 0)?;
        let second = make_asset(locks[2].clone(), 1)?;
        let inputs = [0, 1, 2].map(|i| OutPoint::new([221; 32].pack(), i));
        fixture
            .spec
            .development_cells
            .extend(inputs.iter().cloned().zip([sponsor, first.clone(), second]));
        fixture
            .spec
            .development_cells
            .push((OutPoint::new([221; 32].pack(), 3), first));
        let outputs = [
            Cell::new(FUND - DEBIT, locks[0].clone(), vec![], Bytes::new())?,
            make_asset(ordinary_lock(fixture, 1)?, 1)?,
            make_asset(ordinary_lock(fixture, 2)?, 0)?,
        ];
        Ok(Self {
            code,
            inputs,
            outputs,
            deps,
        })
    }

    fn draft(&self, order: &[usize]) -> Result<Transaction> {
        Transaction::new(
            self.deps.clone(),
            vec![],
            order
                .iter()
                .map(|&i| CellInput::new(self.inputs[i].clone(), None))
                .collect(),
            order.iter().map(|&i| self.outputs[i].clone()).collect(),
            vec![],
        )
    }

    pub(super) fn sign(&self, fixture: &Fixture, spec: &Path, expires: u64) -> Result<Transaction> {
        let temp = tempfile::tempdir()?;
        let code = temp.path().join("intent-lock");
        fs::write(&code, &self.code)?;
        let context = temp.path().join("signing-context.json");
        fs::write(
            &context,
            serde_json::to_vec(&json!({
                "cells": fixture.spec.development_cells.iter().map(|(point, cell)| json!({
                    "out_point": ckb_jsonrpc_types::OutPoint::from(point.clone()),
                    "cell": format!("0x{}", hex::encode(cell.as_slice())),
                })).collect::<Vec<_>>(),
            }))?,
        )?;
        let quote_paths = [0, 1, 2].map(|i| temp.path().join(format!("intent-{i}.json")));
        for (index, quote) in quote_paths.iter().enumerate() {
            let key_file = temp.path().join(format!("key-{index}"));
            fs::write(&key_file, key(index))?;
            fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600))?;
            let draft = if index == 0 {
                self.draft(&[0, 1])?
            } else {
                self.draft(&[index])?
            };
            let transaction = write_transaction(temp.path().join("sign.json"), &draft)?;
            let mut command = wallet("sign", spec, &transaction, &context, &code);
            command.args(["--key-file"]).arg(key_file).args([
                "--input-index",
                "0",
                "--max-fee",
                &DEBIT.to_string(),
                "--valid-before-height",
                &expires.to_string(),
            ]);
            if index == 0 {
                command.args(["--sponsor-max-debit", &DEBIT.to_string()]);
            }
            fs::write(quote, checked(command)?)?;
        }
        let draft = self.draft(&[0, 1, 2])?;
        let transaction = write_transaction(temp.path().join("swap.json"), &draft)?;
        let mut command = wallet("attach", spec, &transaction, &context, &code);
        // Solver discovery order is independent of the group order.
        for quote in quote_paths.iter().rev() {
            command.arg("--intent").arg(quote);
        }
        let encoded: JsonBytes = serde_json::from_slice(&checked(command)?)?;
        Transaction::from_slice(encoded.as_bytes())
    }
}

fn key(index: usize) -> [u8; 32] {
    [81 + index as u8; 32]
}

fn ordinary_lock(fixture: &Fixture, index: usize) -> Result<Script> {
    let owner = blake2b_256(Privkey::from_slice(&key(index)).pubkey()?.serialize());
    Ok(fixture
        .lock
        .clone()
        .as_builder()
        .args(Bytes::copy_from_slice(&owner[..20]))
        .build())
}

fn write_transaction(path: PathBuf, tx: &Transaction) -> Result<PathBuf> {
    fs::write(
        &path,
        serde_json::to_vec(&JsonBytes::from_vec(tx.as_slice().to_vec()))?,
    )?;
    Ok(path)
}

fn wallet(action: &str, genesis: &Path, tx: &Path, context: &Path, code: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_stem"));
    command
        .args(["wallet", "intent", action])
        .arg("--genesis")
        .arg(genesis)
        .arg("--transaction")
        .arg(tx)
        .arg("--context")
        .arg(context)
        .arg("--code")
        .arg(code);
    command
}

fn checked(mut command: Command) -> Result<Vec<u8>> {
    let output = command.output()?;
    ensure!(
        output.status.success(),
        "wallet {command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output.stdout)
}

#[test]
fn signed_asset_pairs_and_fee_sponsorship_enforce_execution_scope() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let swap = Swap::fund(&mut fixture)?;
    let temp = tempfile::tempdir()?;
    let genesis = temp.path().join("spec.json");
    super::comet::write_spec(&genesis, &fixture.spec)?;
    let tx = swap.sign(&fixture, &genesis, 2)?;
    let path = temp.path().join("state");
    let mut store = Store::open(&path, &fixture.spec, Retention::Archive)?;
    assert_eq!(store.check(&tx)?.fee, DEBIT);
    let serial = store.prepare(std::slice::from_ref(&tx), TIME, false)?;
    let parallel = store.prepare(std::slice::from_ref(&tx), TIME, true)?;
    assert_eq!(serial.header(), parallel.header());
    assert_eq!(serial.receipts(), parallel.receipts());

    let build = |inputs, outputs, deps| {
        Transaction::new(deps, vec![], inputs, outputs, tx.witnesses().collect())
    };
    let mut diverted = swap.outputs.to_vec();
    diverted.push(Cell::new(
        DEBIT,
        fixture.lock.clone(),
        vec![],
        Bytes::new(),
    )?);
    assert!(
        store
            .check(&build(tx.inputs().collect(), diverted, swap.deps.clone())?)
            .is_err()
    );

    let mut relabelled: Vec<_> = tx.inputs().collect();
    relabelled[1].previous_output = OutPoint::new([221; 32].pack(), 3);
    assert!(
        store
            .check(&build(
                relabelled,
                swap.outputs.to_vec(),
                swap.deps.clone()
            )?)
            .is_err()
    );

    let mut deps = swap.deps.clone();
    *deps.last_mut().context("asset dep")? = CellDep::new_builder()
        .out_point(OutPoint::new([220; 32].pack(), 2))
        .build();
    assert!(
        store
            .check(&build(tx.inputs().collect(), swap.outputs.to_vec(), deps)?)
            .is_err()
    );

    let mut changed: Vec<_> = tx.inputs().collect();
    changed[1].since = Some(Since::AbsoluteHeight(0));
    assert!(
        store
            .check(&build(changed, swap.outputs.to_vec(), swap.deps.clone())?)
            .is_err()
    );

    let mut delayed = swap.outputs.to_vec();
    delayed.swap(1, 2);
    assert!(
        store
            .check(&build(tx.inputs().collect(), delayed, swap.deps.clone())?)
            .is_err()
    );
    let missing = build(
        tx.inputs().collect(),
        swap.outputs[..2].to_vec(),
        swap.deps.clone(),
    )?;
    assert!(store.check(&missing).is_err());
    let with_message = swap
        .draft(&[0, 1, 2])?
        .with_messages(vec![GroupMessage {
            group_index: 0,
            actions: vec![GroupAction {
                kind: 1,
                data: Bytes::from_static(b"not authorized"),
            }],
        }])?
        .with_witnesses(tx.witnesses().collect())?;
    assert!(store.check(&with_message).is_err());

    let mut foreign = Fixture::new()?;
    Swap::fund(&mut foreign)?;
    foreign.spec.network = [94; 32].pack();
    let other = Store::open(
        &temp.path().join("foreign"),
        &foreign.spec,
        Retention::Archive,
    )?;
    assert!(other.check(&tx).is_err());

    // Cancellation binds the full transaction and refunds the original asset.
    let cancel_input = &swap.inputs[1];
    let original = store
        .live_cell(cancel_input)?
        .context("missing cancellation input")?
        .cell;
    let cancellation = Transaction::new(
        swap.deps.clone(),
        vec![],
        vec![CellInput::new(cancel_input.clone(), None)],
        vec![Cell::new(
            FUND - FEE,
            ordinary_lock(&fixture, 1)?,
            original.type_scripts().collect(),
            original.data(),
        )?],
        vec![],
    )?;
    let digest = cancellation.signing_digest(
        store.network(),
        0,
        &original.lock(),
        |point| {
            Ok(store
                .live_cell(point)?
                .context("missing cancellation context")?
                .cell)
        },
        |_| anyhow::bail!("unexpected Header Dep"),
    )?;
    let seal = Privkey::from_slice(&key(1)).sign_recoverable(&digest.unpack())?;
    let cancellation = cancellation.with_witnesses(vec![GroupWitness {
        group_index: 0,
        proof_data: Bytes::new(),
        seal: Bytes::from(seal.serialize()),
    }])?;
    assert_eq!(store.check(&cancellation)?.fee, FEE);

    // Test expiry without spending the inputs, then verify replay after commit.
    let mut later = Store::open(
        &temp.path().join("later"),
        &fixture.spec,
        Retention::Archive,
    )?;
    later.commit(later.prepare(&[], TIME, false)?)?;
    assert!(later.check(&tx).is_err());
    store.commit(parallel)?;
    assert!(store.check(&tx).is_err());
    for (index, expected) in swap.outputs.iter().enumerate() {
        let output = OutPoint::new(tx.id(store.network()), index as u32);
        let found = store
            .live_cell(&output)?
            .context("missing atomic trade output")?;
        assert_eq!(found.cell.as_slice(), expected.as_slice());
    }
    Ok(())
}
