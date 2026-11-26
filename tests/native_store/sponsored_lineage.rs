use std::{path::Path, time::Instant};

use anyhow::ensure;
use ckb_types::{packed::Byte32, prelude::*};
use stem_ledger::lineage::{self as ledger, Intent, Outcome, actor_id};

use super::{
    Bytes, Cell, CellDep, Context, FEE, FUND, Fixture, OutPoint, Privkey, Result, Retention,
    Script, ScriptHashType, Store, TIME, Transaction, blake2b_256,
    lineage::{DynamicLineage, FixtureLineage, reserve_data},
};

pub(super) struct SponsoredLineage {
    pub(super) dynamic: DynamicLineage,
    order: [usize; 2],
    keys: [(Byte32, Privkey); 2],
    locks: [Script; 2],
    owners: [Byte32; 2],
}

impl SponsoredLineage {
    pub(super) fn fund(
        fixture: &mut Fixture,
        registry: &FixtureLineage,
        fees: usize,
    ) -> Result<Self> {
        Self::fund_at(fixture, registry, fees, 235, [90, 91])
    }

    pub(super) fn fund_at(
        fixture: &mut Fixture,
        registry: &FixtureLineage,
        fees: usize,
        prefix: u8,
        actor_seeds: [u8; 2],
    ) -> Result<Self> {
        let mut dynamic =
            DynamicLineage::fund_with_prefix(fixture, registry, fees, 2, None, prefix)?;
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
        let executable = super::contract::native_contract(workspace, "stem-lineage-match")?;
        let asset = super::contract::native_contract(workspace, "stem-lineage-reserve-udt")?;
        let asset_dep = OutPoint::new([prefix + 1; 32].pack(), 2);
        let program = |data: Bytes| {
            Cell::new(
                Cell::minimum_capacity(&fixture.lock, &[], data.len())?,
                fixture.lock.clone(),
                vec![],
                data,
            )
        };
        dynamic.executor = blake2b_256(&executable).pack();
        fixture
            .spec
            .development_cells
            .iter_mut()
            .find(|(point, _)| point == &dynamic.executor_dep)
            .context("Executor fixture")?
            .1 = program(executable)?;
        let code = blake2b_256(&asset);
        fixture
            .spec
            .development_cells
            .push((asset_dep.clone(), program(asset)?));
        dynamic
            .deps
            .push(CellDep::new_builder().out_point(asset_dep).build());
        let types: Vec<_> = [0, 1]
            .into_iter()
            .map(|slot| {
                Script::new_builder()
                    .code_hash(code)
                    .hash_type(ScriptHashType::Data)
                    .args(Bytes::copy_from_slice(&[slot]))
                    .build()
            })
            .collect();

        let [a, b] = actor_seeds.map(|seed| -> Result<_> {
            let key = Privkey::from_slice(&[seed; 32]);
            let actor = actor_id(key.pubkey()?.serialize().as_slice().try_into()?);
            Ok((actor, key))
        });
        let mut keys = [a?, b?];
        let owners = [keys[0].0.clone(), keys[1].0.clone()];
        let lock_for = |key: &Privkey| -> Result<Script> {
            let key_hash = blake2b_256(key.pubkey()?.serialize());
            Ok(ledger::lock(
                &dynamic.fee_cell.lock().code_hash(),
                key_hash[..20].try_into()?,
                true,
            ))
        };
        let locks = [lock_for(&keys[0].1)?, lock_for(&keys[1].1)?];
        let mut order = [0, 1];
        order.sort_unstable_by_key(|i| dynamic.ids[*i].clone());
        for (index, (x, y)) in order.into_iter().zip([(500, 30), (20, 800)]) {
            let owner = owners[index].unpack();
            fixture
                .spec
                .development_cells
                .iter_mut()
                .find(|(point, _)| point == &dynamic.anchors[index])
                .context("reserve fixture")?
                .1 = Cell::new(
                FUND,
                fixture.lock.clone(),
                types.clone(),
                reserve_data(x, y, 2, owner),
            )?;
        }
        keys.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        Ok(Self {
            dynamic,
            order,
            keys,
            locks,
            owners,
        })
    }

    pub(super) fn registers(&self, fixture: &Fixture) -> Result<Vec<Transaction>> {
        (0..2)
            .map(|i| {
                self.dynamic
                    .register_with_lock(fixture, i, self.locks[i].clone())
            })
            .collect()
    }

    pub(super) fn head_ids(&self) -> [Byte32; 2] {
        self.order.map(|i| self.dynamic.ids[i].clone())
    }

    pub(super) fn draft(&self, version: u64, x: u128, y: u128) -> Result<Transaction> {
        self.draft_after(version, x, y, None)
    }

    pub(super) fn draft_after(
        &self,
        version: u64,
        x: u128,
        y: u128,
        height: Option<u64>,
    ) -> Result<Transaction> {
        let mut payload = [x.to_le_bytes(), y.to_le_bytes()].concat();
        if let Some(height) = height {
            payload.extend_from_slice(&height.to_le_bytes());
        }
        Transaction::new_lineage_command(
            self.dynamic.deps.clone(),
            self.dynamic.fee_points[0].clone(),
            1000,
            ledger::Command {
                targets: self
                    .head_ids()
                    .into_iter()
                    .map(|id| {
                        Ok(ledger::Target {
                            reference: ledger::LineageRef::new(id, Some(version))?,
                            latest_bounds: None,
                            min_capacity: FUND - FEE,
                        })
                    })
                    .collect::<Result<_>>()?,
                executor: self.dynamic.executor.clone(),
                executor_dep: self.dynamic.executor_dep.clone(),
                executor_cycles: 100_000,
                validation_cycles: 5_000_000,
                fee: FEE,
                payload: payload.into(),
            },
        )?
        .with_lineage_intent(Intent {
            actors: self.keys.iter().map(|(id, _)| id.clone()).collect(),
            max_fee: 2 * FEE,
            batch: None,
        })
    }

    pub(super) fn business_seal(&self, fixture: &Fixture, tx: &Transaction) -> Result<Bytes> {
        let digest = tx.lineage_intent_digest(&fixture.spec.network)?.unpack();
        let mut sealed = Vec::new();
        for (_, key) in &self.keys {
            sealed.extend_from_slice(&key.sign_recoverable(&digest)?.serialize());
        }
        Ok(sealed.into())
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn signed(
        &self,
        fixture: &Fixture,
        fee_index: usize,
        version: u64,
        x: u128,
        y: u128,
        height: Option<u64>,
    ) -> Result<Transaction> {
        let draft = self.draft(version, x, y)?;
        let draft = match height {
            Some(height) => draft.with_execute_at(height)?,
            None => draft,
        };
        let sealed = draft
            .with_lineage_seals(self.business_seal(fixture, &draft)?)?
            .with_lineage_fee(self.dynamic.fee_points[fee_index].clone(), FEE)?;
        self.dynamic.sign_command(fixture, sealed)
    }

    pub(super) fn reserve(&self, rank: usize, x: u128, y: u128) -> Bytes {
        reserve_data(x, y, 2, self.owners[self.order[rank]].unpack())
    }
}

#[test]
fn bilateral_lineage_actors_and_sponsor_are_independent_and_cannot_replay() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let registry = FixtureLineage::fund(&mut fixture)?;
    let trade = SponsoredLineage::fund(&mut fixture, &registry, 4)?;
    super::keeper::fund_carriers(&mut fixture, 2)?;
    let other_sponsor = Privkey::from_slice(&[92; 32]);
    let other_owner = blake2b_256(other_sponsor.pubkey()?.serialize());
    let other_lock = ledger::lock(
        &trade.dynamic.fee_cell.lock().code_hash(),
        other_owner[..20].try_into()?,
        false,
    );
    let other_cell = Cell::new(FUND, other_lock, vec![], Bytes::new())?;
    fixture
        .spec
        .development_cells
        .iter_mut()
        .find(|(point, _)| point == &trade.dynamic.fee_points[2])
        .context("alternative sponsor input")?
        .1 = other_cell.clone();

    let temp = tempfile::tempdir()?;
    let mut store = Store::open(temp.path(), &fixture.spec, Retention::Archive)?;
    store.commit(store.prepare(&trade.registers(&fixture)?, TIME, true)?)?;
    let draft = trade.draft(0, 40, 70)?;
    let business = trade.business_seal(&fixture, &draft)?;
    assert!(draft.with_lineage_seals(business.slice(..65)).is_err());
    let with_business = draft.with_lineage_seals(business.clone())?;
    assert!(
        with_business
            .with_lineage_fee(trade.dynamic.fee_points[1].clone(), 2 * FEE + 1)
            .is_err()
    );
    let bound = with_business.with_lineage_fee(trade.dynamic.fee_points[1].clone(), 2 * FEE)?;
    assert_eq!(
        draft.lineage_intent_digest(store.network())?,
        bound.lineage_intent_digest(store.network())?
    );
    let first = trade.dynamic.sign_command(&fixture, bound)?;
    let replay = with_business.with_lineage_fee(trade.dynamic.fee_points[2].clone(), FEE)?;
    let digest = replay.lineage_signing_digest(&fixture.spec.network, &other_cell)?;
    let replay = replay.with_witnesses(vec![stem_ledger::GroupWitness {
        group_index: 0,
        proof_data: Bytes::new(),
        seal: Bytes::from(
            other_sponsor
                .sign_recoverable(&digest.unpack())?
                .serialize(),
        ),
    }])?;

    let mut swapped = business.to_vec();
    swapped.rotate_left(65);
    let bad = trade
        .dynamic
        .sign_command(&fixture, draft.with_lineage_seals(swapped.into())?)?;
    assert!(store.check(&bad).is_err());
    let modified = trade
        .draft(0, 41, 70)?
        .with_lineage_seals(business.clone())?;
    assert!(
        store
            .check(&trade.dynamic.sign_command(&fixture, modified)?)
            .is_err()
    );
    let cross_height = draft.with_execute_at(4)?.with_lineage_seals(business)?;
    let cross_height = trade.dynamic.sign_command(&fixture, cross_height)?;
    assert!(
        store
            .check(&super::keeper::schedule(&fixture, 1, &cross_height)?)
            .is_err()
    );

    let fee_override = first
        .with_witnesses(vec![])?
        .with_lineage_fee(trade.dynamic.fee_points[2].clone(), FEE)?
        .with_witnesses(first.witnesses().collect())?;
    assert!(store.check(&fee_override).is_err());
    assert_eq!(
        Outcome::decode(&store.check(&replay)?.protocol_result)?,
        Outcome::Applied
    );
    assert_eq!(store.check(&first)?.fee, 2 * FEE);
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let serial = store.prepare(std::slice::from_ref(&first), TIME, false)?;
    let parallel = pool.install(|| store.prepare(std::slice::from_ref(&first), TIME, true))?;
    assert_eq!(serial.header(), parallel.header());
    assert_eq!(serial.receipts(), parallel.receipts());
    let receipt = &serial.receipts()[0];
    assert_eq!(Outcome::decode(&receipt.protocol_result)?, Outcome::Applied);
    assert!(receipt.cycles >= 4 * stem_vm::SECP256K1_RECOVERY_CYCLES);
    store.commit(parallel)?;
    let replay_error = store.check(&replay).unwrap_err();
    assert!(format!("{replay_error:#}").contains("outside the signed version"));
    assert!(store.live_cell(&trade.dynamic.fee_points[2])?.is_some());
    let refund = store
        .live_cell(&OutPoint::new(first.id(store.network()), 0))?
        .unwrap();
    assert_eq!(refund.cell.lock(), trade.dynamic.fee_cell.lock());
    assert_eq!(refund.cell.capacity(), FUND - 2 * FEE);

    let wake = trade.signed(&fixture, 3, 1, 5, 7, Some(4))?;
    let register_wake = super::keeper::schedule(&fixture, 0, &wake)?;
    store.commit(store.prepare(&[register_wake], TIME, true)?)?;
    let due = store.prepare(&[], TIME, true)?;
    assert!(due.submitted_receipts().is_empty());
    assert_eq!(
        Outcome::decode(&due.receipts()[0].protocol_result)?,
        Outcome::Applied
    );
    store.commit(due)?;
    for (rank, ((id, x), y)) in trade
        .head_ids()
        .into_iter()
        .zip([455, 65])
        .zip([107, 723])
        .enumerate()
    {
        let (_, cell, _) = store
            .prove_current_lineage(&id)?
            .head
            .context("settled reserve")?;
        assert_eq!(cell.cell.data(), trade.reserve(rank, x, y));
    }
    Ok(())
}

#[test]
#[ignore = "local bilateral settlement measurement"]
fn measure_sponsored_lineage_settlement() -> Result<()> {
    const ROUNDS: usize = 31;
    let mut fixture = Fixture::new()?;
    let registry = FixtureLineage::fund(&mut fixture)?;
    let trade = SponsoredLineage::fund(&mut fixture, &registry, ROUNDS)?;
    let temp = tempfile::tempdir()?;
    let mut store = Store::open(temp.path(), &fixture.spec, Retention::Archive)?;
    store.commit(store.prepare(&trade.registers(&fixture)?, TIME, false)?)?;
    let txs = (0..ROUNDS)
        .map(|i| trade.signed(&fixture, i, i as u64, 7, 11, None))
        .collect::<Result<Vec<_>>>()?;
    let mut prepare = vec![];
    let mut commit = vec![];
    let mut cycles = 0;
    for tx in &txs {
        let start = Instant::now();
        let block = store.prepare(std::slice::from_ref(tx), TIME, false)?;
        prepare.push(start.elapsed().as_micros());
        let receipt = &block.receipts()[0];
        ensure!(Outcome::decode(&receipt.protocol_result)? == Outcome::Applied);
        cycles += receipt.cycles;
        let start = Instant::now();
        store.commit(block)?;
        commit.push(start.elapsed().as_micros());
    }
    let samples = |mut times: Vec<_>| {
        times.sort_unstable();
        [50, 95, 99].map(|p| times[(p as usize * ROUNDS).div_ceil(100) - 1] as u64)
    };
    println!(
        "{}",
        serde_json::json!({
            "rounds": ROUNDS, "intent_actors": 2, "sponsors": 1, "operations_per_block": 1,
            "transaction_bytes": txs[0].as_slice().len(), "mean_charged_cycles": cycles / ROUNDS as u64,
            "percentile_order": [50,95,99], "prepare_us": samples(prepare), "commit_us": samples(commit),
        })
    );
    Ok(())
}

#[test]
fn wallet_collects_business_seals_before_binding_and_signing_the_fee() -> Result<()> {
    use ckb_jsonrpc_types::{CellDep as JsonDep, JsonBytes, OutPoint as JsonPoint};
    use ckb_types::H256;
    use serde_json::json;
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    let mut fixture = Fixture::new()?;
    let registry = FixtureLineage::fund(&mut fixture)?;
    let trade = SponsoredLineage::fund(&mut fixture, &registry, 2)?;
    let template = trade.draft(0, 40, 70)?;
    let terms = template.lineage_command().unwrap();
    let intent = template.lineage_intent().unwrap();
    let scope = tempfile::tempdir()?;
    let path = |name| scope.path().join(name);
    super::comet::write_spec(&path("spec.json"), &fixture.spec)?;
    let executor = H256::from(terms.executor.clone());
    let executor_arg = format!("{executor:#x}");
    let targets: Vec<_> = terms
        .targets
        .iter()
        .map(|target| {
            json!({
                "reference": JsonBytes::from_bytes(target.reference.encode()),
                "latest_bounds": target.latest_bounds, "min_capacity": target.min_capacity
            })
        })
        .collect();
    fs::write(
        path("request.json"),
        serde_json::to_vec(&json!({
            "business": {
                "actors": intent.actors.iter().cloned().map(H256::from).collect::<Vec<_>>(),
                "max_fee": intent.max_fee,
            },
            "cell_deps": template.cell_deps().map(JsonDep::from).collect::<Vec<_>>(),
            "valid_before_height": template.valid_before_height(),
            "executor": executor, "executor_dep": JsonPoint::from(terms.executor_dep.clone()),
            "executor_cycles": terms.executor_cycles, "validation_cycles": terms.validation_cycles,
            "payload": JsonBytes::from_bytes(terms.payload.clone()), "targets": targets,
        }))?,
    )?;
    let wallet = |subcommand| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_stem"));
        command.args(["wallet", "lineage", subcommand]);
        command
    };
    let run = |command: &mut Command| -> Result<Vec<u8>> {
        let output = command.output()?;
        ensure!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(output.stdout)
    };
    let write_key = |name, seed| -> Result<()> {
        fs::write(path(name), [seed; 32])?;
        fs::set_permissions(path(name), fs::Permissions::from_mode(0o600))?;
        Ok(())
    };
    let draft = run(wallet("build").arg("--request").arg(path("request.json")))?;
    fs::write(path("draft.json"), draft)?;
    for (key_file, seal_file, seed) in [("a", "a.sig", 90), ("b", "b.sig", 91)] {
        write_key(key_file, seed)?;
        let seal = run(wallet("sign-business")
            .arg("--genesis")
            .arg(path("spec.json"))
            .arg("--transaction")
            .arg(path("draft.json"))
            .arg("--executor-hash")
            .arg(&executor_arg)
            .arg("--key-file")
            .arg(path(key_file)))?;
        let actor: H256 =
            serde_json::from_slice(&run(wallet("actor").arg("--key-file").arg(path(key_file)))?)?;
        let seal_json: serde_json::Value = serde_json::from_slice(&seal)?;
        assert_eq!(seal_json["actor"], serde_json::to_value(actor)?);
        fs::write(path(seal_file), seal)?;
    }

    let fee_point = &trade.dynamic.fee_points[1];
    fs::write(
        path("point.json"),
        serde_json::to_vec(&JsonPoint::from(fee_point.clone()))?,
    )?;
    let assemble = |max_fee: u64| {
        let mut command = wallet("assemble");
        command
            .arg("--genesis")
            .arg(path("spec.json"))
            .arg("--transaction")
            .arg(path("draft.json"))
            .arg("--fee-input")
            .arg(path("point.json"))
            .arg("--fee")
            .arg(max_fee.to_string())
            .arg("--seal")
            .arg(path("b.sig"))
            .arg("--seal")
            .arg(path("a.sig"));
        command
    };
    let over = assemble(2 * FEE + 1).output()?;
    assert!(!over.status.success());
    fs::write(path("assembled.json"), run(&mut assemble(FEE))?)?;
    write_key("sponsor", 76)?;
    let cells: Vec<_> = fixture
        .spec
        .development_cells
        .iter()
        .filter(|(point, _)| point == fee_point || point == &terms.executor_dep)
        .map(|(point, cell)| {
            json!({
                "out_point": JsonPoint::from(point.clone()),
                "cell": JsonBytes::from_vec(cell.as_slice().to_vec())
            })
        })
        .collect();
    fs::write(
        path("context.json"),
        serde_json::to_vec(&json!({ "cells": cells }))?,
    )?;
    fs::write(
        path("fee-lock"),
        super::contract::native_contract(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "stem-lineage-lock",
        )?,
    )?;
    let submit = run(wallet("sign")
        .arg("--genesis")
        .arg(path("spec.json"))
        .arg("--transaction")
        .arg(path("assembled.json"))
        .arg("--context")
        .arg(path("context.json"))
        .arg("--code")
        .arg(path("fee-lock"))
        .arg("--executor-hash")
        .arg(&executor_arg)
        .arg("--max-fee")
        .arg(FEE.to_string())
        .arg("--key-file")
        .arg(path("sponsor")))?;
    let signed: JsonBytes = serde_json::from_slice(&submit)?;
    let expected = trade.signed(&fixture, 1, 0, 40, 70, None)?;
    assert_eq!(signed.as_bytes(), expected.as_slice());

    let mut store = Store::open(&path("db"), &fixture.spec, Retention::Archive)?;
    store.commit(store.prepare(&trade.registers(&fixture)?, TIME, false)?)?;
    let result = store.check(&Transaction::from_slice(signed.as_bytes())?)?;
    assert_eq!(Outcome::decode(&result.protocol_result)?, Outcome::Applied);
    assert_eq!(result.fee, FEE);
    Ok(())
}
