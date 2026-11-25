use std::{collections::BTreeSet, path::Path};

use ckb_types::{packed::Byte32, prelude::*};
use stem_ledger::{
    GroupAction, GroupMessage,
    lineage::{self, Head, REGISTER, REPLACE_EXECUTOR, RETIRE},
};

use super::{
    Bytes, Cell, CellDep, CellInput, Context, FEE, FUND, Fixture, OutPoint, Result, Retention,
    Script, ScriptHashType, Store, TIME, Transaction, blake2b_256,
};

pub(super) struct FixtureLineage {
    marker_code: Byte32,
    asset: Script,
    deps: Vec<CellDep>,
    anchor: OutPoint,
}

impl FixtureLineage {
    pub(super) fn fund(fixture: &mut Fixture) -> Result<Self> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
        let marker = super::contract::native_contract(workspace, "stem-lineage")?;
        let udt = super::contract::native_contract(workspace, "bridge-udt")?;
        let marker_code: Byte32 = blake2b_256(&marker).pack();
        let asset = Script::new_builder()
            .code_hash(blake2b_256(&udt))
            .hash_type(ScriptHashType::Data)
            .args(Bytes::from_static(&[92; 32]))
            .build();
        fixture.spec.lineage_type = Some(marker_code.clone());
        let anchor = OutPoint::new([222; 32].pack(), 2);
        fixture.spec.development_cells.push((
            anchor.clone(),
            Cell::new(FUND, fixture.lock.clone(), vec![asset.clone()], amount())?,
        ));
        let mut deps = fixture.deps.clone();
        for (index, code) in [marker, udt].into_iter().enumerate() {
            let point = OutPoint::new([222; 32].pack(), index as u32);
            let bond = Cell::minimum_capacity(&fixture.lock, &[], code.len())?;
            fixture.spec.development_cells.push((
                point.clone(),
                Cell::new(bond, fixture.lock.clone(), vec![], code)?,
            ));
            deps.push(CellDep::new_builder().out_point(point).build());
        }
        Ok(Self {
            marker_code,
            asset,
            deps,
            anchor,
        })
    }

    pub(super) fn example(&self, fixture: &Fixture) -> Result<(Byte32, Transaction, Transaction)> {
        let id = lineage::id(&fixture.spec.network, &self.anchor, 0);
        let marker = lineage::marker(&self.marker_code, &id);
        let register = self.transition(
            fixture,
            self.anchor.clone(),
            &marker,
            FUND - FEE,
            Some((REGISTER, fixture.lock.code_hash().as_bytes())),
            false,
        )?;
        let update = self.transition(
            fixture,
            OutPoint::new(register.id(&fixture.spec.network), 0),
            &marker,
            FUND - 2 * FEE,
            None,
            false,
        )?;
        Ok((id, register, update))
    }

    fn transition(
        &self,
        fixture: &Fixture,
        point: OutPoint,
        marker: &Script,
        capacity: u64,
        action: Option<(u16, Bytes)>,
        retire: bool,
    ) -> Result<Transaction> {
        let types = BTreeSet::from([marker.as_bytes(), self.asset.as_bytes()]);
        let group_index = u16::try_from(
            1 + types
                .iter()
                .position(|bytes| bytes == &marker.as_bytes())
                .unwrap(),
        )?;
        let scripts = if retire {
            vec![self.asset.clone()]
        } else {
            vec![self.asset.clone(), marker.clone()]
        };
        let mut tx = Transaction::new(
            self.deps.clone(),
            vec![],
            vec![CellInput::new(point, None)],
            vec![Cell::new(
                capacity,
                fixture.lock.clone(),
                scripts,
                amount(),
            )?],
            vec![],
        )?;
        if let Some((kind, data)) = action {
            tx = tx.with_messages(vec![GroupMessage {
                group_index,
                actions: vec![GroupAction { kind, data }],
            }])?;
        }
        fixture.seal(tx)
    }
}

fn amount() -> Bytes {
    Bytes::copy_from_slice(&240u128.to_le_bytes())
}

#[test]
fn lineage_registry_orders_versions_authenticates_heads_and_excludes_recycling() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let lineage = FixtureLineage::fund(&mut fixture)?;
    let id = lineage::id(&fixture.spec.network, &lineage.anchor, 0);
    let marker = lineage::marker(&lineage.marker_code, &id);
    let executor = fixture.lock.code_hash();
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(
        &directory.path().join("source"),
        &fixture.spec,
        Retention::Archive,
    )?;
    let make_registration = |marker, action| {
        lineage.transition(
            &fixture,
            lineage.anchor.clone(),
            marker,
            FUND - FEE,
            action,
            false,
        )
    };
    let absent = make_registration(&marker, None)?;
    assert!(store.check(&absent).is_err());
    let bad_executor = make_registration(&marker, Some((REGISTER, Bytes::from_static(&[5; 32]))))?;
    assert!(store.check(&bad_executor).is_err());
    let false_marker = lineage::marker(&lineage.marker_code, &[4; 32].pack());
    let forged = make_registration(&false_marker, Some((REGISTER, executor.as_bytes())))?;
    assert!(store.check(&forged).is_err());

    let register = make_registration(&marker, Some((REGISTER, executor.as_bytes())))?;
    let registered = OutPoint::new(register.id(store.network()), 0);
    let next_executor = lineage.asset.code_hash();
    let update = lineage.transition(
        &fixture,
        registered.clone(),
        &marker,
        FUND - 2 * FEE,
        Some((REPLACE_EXECUTOR, next_executor.as_bytes())),
        false,
    )?;
    let updated = OutPoint::new(update.id(store.network()), 0);
    let follow = lineage.transition(
        &fixture,
        updated.clone(),
        &marker,
        FUND - 3 * FEE,
        None,
        false,
    )?;
    let current = OutPoint::new(follow.id(store.network()), 0);
    let transactions = [register, update, follow];
    let fork = lineage.transition(
        &fixture,
        updated.clone(),
        &marker,
        FUND - 4 * FEE,
        None,
        false,
    )?;
    assert!(
        store
            .prepare(
                &[
                    transactions[0].clone(),
                    transactions[1].clone(),
                    transactions[2].clone(),
                    fork
                ],
                TIME,
                true
            )
            .is_err()
    );
    assert!(
        store
            .prepare(
                &[transactions[1].clone(), transactions[0].clone()],
                TIME,
                true
            )
            .is_err()
    );
    let serial = store.prepare(&transactions, TIME, false)?;
    let workers = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let parallel = workers.install(|| store.prepare(&transactions, TIME, true))?;
    assert_eq!(serial.header(), parallel.header());
    assert_eq!(serial.receipts(), parallel.receipts());
    store.commit(parallel)?;
    assert!(store.live_cell(&registered)?.is_none() && store.live_cell(&updated)?.is_none());
    let expected = Head {
        out_point: current.clone(),
        version: 2,
        executor: next_executor,
    };
    let (head, proof) = store.prove_lineage(&id)?;
    assert_eq!(head.as_ref(), Some(&expected));
    let paired = store.prove_current_lineage(&id)?;
    assert!(paired.verify(store.network(), store.tip(), &id, &lineage.marker_code));
    assert!(!paired.verify(store.network(), store.tip(), &id, &[87; 32].pack()));
    let invalid_business = fixture.seal(Transaction::new(
        lineage.deps.clone(),
        vec![],
        vec![CellInput::new(current.clone(), None)],
        vec![Cell::new(
            FUND - 4 * FEE,
            fixture.lock.clone(),
            vec![lineage.asset.clone(), marker.clone()],
            Bytes::copy_from_slice(&241u128.to_le_bytes()),
        )?],
        vec![],
    )?)?;
    assert!(store.prepare(&[invalid_business], TIME, true).is_err());
    assert_eq!(store.prove_lineage(&id)?.0, head);
    assert!(Store::verify_lineage(
        store.network(),
        store.tip(),
        &id,
        head.as_ref(),
        &proof
    ));
    assert!(!Store::verify_lineage(
        store.network(),
        store.tip(),
        &id,
        Some(&Head {
            version: 3,
            ..expected.clone()
        }),
        &proof
    ));
    let mut restored = Store::open(
        &directory.path().join("restored"),
        &fixture.spec,
        Retention::Archive,
    )?;
    restored.install_state(
        store.export_state()?,
        &store.tip().app_hash(store.network()),
    )?;
    assert_eq!(restored.prove_lineage(&id)?.0, Some(expected));
    assert!(restored.check(&transactions[1]).is_err());

    let unmarked = lineage.transition(
        &fixture,
        current.clone(),
        &marker,
        FUND - 4 * FEE,
        None,
        true,
    )?;
    assert!(restored.check(&unmarked).is_err());
    let retire = lineage.transition(
        &fixture,
        current,
        &marker,
        FUND - 4 * FEE,
        Some((RETIRE, Bytes::new())),
        true,
    )?;
    let withdrawal = OutPoint::new(retire.id(restored.network()), 0);
    restored.commit(restored.prepare(&[retire], TIME, false)?)?;
    let (head, proof) = restored.prove_lineage(&id)?;
    assert!(
        head.is_none()
            && Store::verify_lineage(restored.network(), restored.tip(), &id, None, &proof)
    );
    assert_eq!(
        restored
            .live_cell(&withdrawal)?
            .context("retired asset output")?
            .cell
            .data(),
        amount()
    );
    let recycle = lineage.transition(
        &fixture,
        withdrawal,
        &marker,
        FUND - 5 * FEE,
        Some((REGISTER, executor.as_bytes())),
        false,
    )?;
    assert!(restored.check(&recycle).is_err());
    Ok(())
}

#[test]
#[ignore = "matched local signed Lineage lifecycle workload"]
fn measure_explicit_lineage_updates() -> Result<()> {
    use std::time::Instant;
    const WIDTH: usize = 8;
    const BLOCKS: usize = 25;
    let workers = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    for (workload, count) in [
        ("independent", WIDTH),
        ("same-head-chain", 1),
        ("atomic", WIDTH),
    ] {
        let mut fixture = Fixture::new()?;
        let lineage = FixtureLineage::fund(&mut fixture)?;
        let mut points = Vec::new();
        let mut markers = Vec::new();
        let mut registrations = Vec::new();
        for index in 0..count {
            let anchor = OutPoint::new([223; 32].pack(), index as u32);
            fixture.spec.development_cells.push((
                anchor.clone(),
                Cell::new(
                    FUND,
                    fixture.lock.clone(),
                    vec![lineage.asset.clone()],
                    amount(),
                )?,
            ));
            let id = lineage::id(&fixture.spec.network, &anchor, 0);
            let marker = lineage::marker(&lineage.marker_code, &id);
            let register = lineage.transition(
                &fixture,
                anchor,
                &marker,
                FUND - FEE,
                Some((REGISTER, fixture.lock.code_hash().as_bytes())),
                false,
            )?;
            points.push(OutPoint::new(register.id(&fixture.spec.network), 0));
            markers.push(marker);
            registrations.push(register);
        }
        let temp = tempfile::tempdir()?;
        let mut serial = Store::open(
            &temp.path().join("serial"),
            &fixture.spec,
            Retention::Archive,
        )?;
        let mut parallel = Store::open(
            &temp.path().join("parallel"),
            &fixture.spec,
            Retention::Archive,
        )?;
        serial.commit(serial.prepare(&registrations, TIME, false)?)?;
        parallel.commit(parallel.prepare(&registrations, TIME, false)?)?;
        let mut values = vec![FUND - FEE; count];
        let mut serial_us = Vec::new();
        let mut parallel_us = Vec::new();
        let mut total_cycles = 0u64;
        for _ in 0..BLOCKS {
            let mut txs = Vec::new();
            if workload == "atomic" {
                for value in &mut values {
                    *value -= FEE;
                }
                let outputs = markers
                    .iter()
                    .zip(&values)
                    .map(|(marker, &value)| {
                        Cell::new(
                            value,
                            fixture.lock.clone(),
                            vec![lineage.asset.clone(), marker.clone()],
                            amount(),
                        )
                    })
                    .collect::<Result<_>>()?;
                let tx = fixture.seal(Transaction::new(
                    lineage.deps.clone(),
                    vec![],
                    points
                        .iter()
                        .cloned()
                        .map(|point| CellInput::new(point, None))
                        .collect(),
                    outputs,
                    vec![],
                )?)?;
                let txid = tx.id(&fixture.spec.network);
                for (i, point) in points.iter_mut().enumerate() {
                    *point = OutPoint::new(txid.clone(), i as u32);
                }
                txs.push(tx);
            } else {
                for i in 0..WIDTH {
                    let index = i % count;
                    values[index] -= FEE;
                    let tx = lineage.transition(
                        &fixture,
                        points[index].clone(),
                        &markers[index],
                        values[index],
                        None,
                        false,
                    )?;
                    points[index] = OutPoint::new(tx.id(&fixture.spec.network), 0);
                    txs.push(tx);
                }
            }
            let timed = |store: &mut Store, use_parallel| -> Result<(u128, u64)> {
                let at = Instant::now();
                let prepared = if use_parallel {
                    workers.install(|| store.prepare(&txs, TIME, true))?
                } else {
                    store.prepare(&txs, TIME, false)?
                };
                let cycles = prepared
                    .receipts()
                    .iter()
                    .map(|receipt| receipt.cycles)
                    .sum();
                store.commit(prepared)?;
                Ok((at.elapsed().as_micros(), cycles))
            };
            let (a, first) = timed(&mut serial, false)?;
            let (b, second) = timed(&mut parallel, true)?;
            assert_eq!(serial.tip(), parallel.tip());
            assert_eq!(first, second);
            total_cycles += first;
            serial_us.push(a);
            parallel_us.push(b);
        }
        serial_us.sort_unstable();
        parallel_us.sort_unstable();
        println!(
            "Lineage {workload} blocks={BLOCKS} updates_per_block={WIDTH} serial_us_p50={} parallel_us_p50={} mean_cycles_per_block={}",
            serial_us[BLOCKS / 2],
            parallel_us[BLOCKS / 2],
            total_cycles / BLOCKS as u64
        );
    }
    Ok(())
}

pub(super) struct DynamicLineage {
    pub(super) marker: Byte32,
    pub(super) ids: Vec<Byte32>,
    anchors: Vec<OutPoint>,
    pub(super) fee_points: Vec<OutPoint>,
    pub(super) fee_cell: Cell,
    head_lock: Script,
    executor: Byte32,
    executor_dep: OutPoint,
    pub(super) deps: Vec<CellDep>,
}

impl DynamicLineage {
    pub(super) fn fund(fixture: &mut Fixture, fees: usize) -> Result<Self> {
        let register = FixtureLineage::fund(fixture)?;
        Self::fund_using(fixture, &register, fees, 3, Some(2))
    }

    pub(super) fn fund_using(
        fixture: &mut Fixture,
        register: &FixtureLineage,
        fees: usize,
        heads: usize,
        typed: Option<usize>,
    ) -> Result<Self> {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
        let lock = super::contract::native_contract(workspace, "stem-lineage-lock")?;
        let counter = super::contract::native_contract(workspace, "stem-lineage-counter")?;
        let lock_code = blake2b_256(&lock).pack();
        let executor = blake2b_256(&counter).pack();
        let owner = blake2b_256(fixture.key.pubkey()?.serialize());
        let owner = owner[..20].try_into()?;
        let fee_cell = Cell::new(
            FUND,
            lineage::lock(&lock_code, owner, false),
            vec![],
            Bytes::new(),
        )?;
        let head_lock = lineage::lock(&lock_code, owner, true);
        let anchors: Vec<_> = (0..heads as u32)
            .map(|index| OutPoint::new([231; 32].pack(), index))
            .collect();
        let ids = anchors
            .iter()
            .map(|anchor| lineage::id(&fixture.spec.network, anchor, 0))
            .collect();
        for (index, point) in anchors.iter().enumerate() {
            let types = if typed == Some(index) {
                vec![register.asset.clone()]
            } else {
                vec![]
            };
            fixture.spec.development_cells.push((
                point.clone(),
                Cell::new(FUND, fixture.lock.clone(), types, amount())?,
            ));
        }
        let mut deps = register.deps.clone();
        let executor_dep = OutPoint::new([232; 32].pack(), 1);
        for (index, code) in [lock, counter].into_iter().enumerate() {
            let point = OutPoint::new([232; 32].pack(), index as u32);
            fixture.spec.development_cells.push((
                point.clone(),
                Cell::new(
                    Cell::minimum_capacity(&fixture.lock, &[], code.len())?,
                    fixture.lock.clone(),
                    vec![],
                    code,
                )?,
            ));
            deps.push(CellDep::new_builder().out_point(point).build());
        }
        let fee_points: Vec<_> = (0..fees as u32)
            .map(|index| OutPoint::new([233; 32].pack(), index))
            .collect();
        for point in &fee_points {
            fixture
                .spec
                .development_cells
                .push((point.clone(), fee_cell.clone()));
        }
        Ok(Self {
            marker: register.marker_code.clone(),
            ids,
            anchors,
            fee_points,
            fee_cell,
            head_lock,
            executor,
            executor_dep,
            deps,
        })
    }

    pub(super) fn register(&self, fixture: &Fixture, index: usize) -> Result<Transaction> {
        let marker = lineage::marker(&self.marker, &self.ids[index]);
        let anchor_cell = &fixture
            .spec
            .development_cells
            .iter()
            .find(|(point, _)| point == &self.anchors[index])
            .context("fixture anchor")?
            .1;
        let mut types: Vec<_> = anchor_cell.type_scripts().collect();
        let marker_group = 1 + u16::try_from(
            types
                .iter()
                .filter(|script| script.as_slice() < marker.as_slice())
                .count(),
        )?;
        types.push(marker);
        let transaction = Transaction::new(
            self.deps.clone(),
            vec![],
            vec![CellInput::new(self.anchors[index].clone(), None)],
            vec![Cell::new(
                FUND - FEE,
                self.head_lock.clone(),
                types,
                anchor_cell.data(),
            )?],
            vec![],
        )?
        .with_messages(vec![GroupMessage {
            group_index: marker_group,
            actions: vec![GroupAction {
                kind: REGISTER,
                data: self.executor.as_bytes(),
            }],
        }])?;
        fixture.seal(transaction)
    }

    pub(super) fn signed(
        &self,
        fixture: &Fixture,
        fee: usize,
        targets: &[usize],
        versions: (u64, u64),
        increment: u64,
    ) -> Result<Transaction> {
        self.signed_payload(
            fixture,
            fee,
            targets,
            versions,
            Bytes::copy_from_slice(&increment.to_le_bytes()),
        )
    }

    pub(super) fn scheduled(
        &self,
        fixture: &Fixture,
        fee: usize,
        versions: (u64, u64),
        increment: u64,
        height: u64,
    ) -> Result<Transaction> {
        let tx = self.signed(fixture, fee, &[0], versions, increment)?;
        let tx = tx.with_witnesses(vec![])?.with_execute_at(height)?;
        self.sign_command(fixture, tx)
    }

    fn signed_payload(
        &self,
        fixture: &Fixture,
        fee: usize,
        targets: &[usize],
        versions: (u64, u64),
        payload: Bytes,
    ) -> Result<Transaction> {
        let mut targets: Vec<_> = targets
            .iter()
            .map(|index| lineage::Target {
                id: self.ids[*index].clone(),
                min_version: versions.0,
                max_version: versions.1,
                min_capacity: FUND - FEE,
            })
            .collect();
        targets.sort_unstable_by(|left, right| left.id.cmp(&right.id));
        let transaction = Transaction::new_lineage_command(
            self.deps.clone(),
            self.fee_points[fee].clone(),
            1000,
            lineage::Command {
                targets,
                executor: self.executor.clone(),
                executor_dep: self.executor_dep.clone(),
                executor_cycles: 100_000,
                validation_cycles: 5_000_000,
                fee: FEE,
                payload,
            },
        )?;
        self.sign_command(fixture, transaction)
    }

    fn sign_command(&self, fixture: &Fixture, transaction: Transaction) -> Result<Transaction> {
        let digest = transaction.lineage_signing_digest(&fixture.spec.network, &self.fee_cell)?;
        transaction.with_witnesses(vec![stem_ledger::GroupWitness {
            group_index: 0,
            proof_data: Bytes::new(),
            seal: Bytes::from(fixture.key.sign_recoverable(&digest.unpack())?.serialize()),
        }])
    }
}

#[test]
fn lineage_commands_resolve_latest_heads_and_commit_atomically() -> Result<()> {
    use lineage::Outcome;
    let mut fixture = Fixture::new()?;
    let dynamic = DynamicLineage::fund(&mut fixture, 8)?;
    let first = dynamic.signed(&fixture, 0, &[0], (0, 2), 2)?;
    let second = dynamic.signed(&fixture, 1, &[0, 1], (0, 2), 3)?;
    let business_failure = dynamic.signed(&fixture, 2, &[0, 1], (0, 2), 0)?;
    let type_failure = dynamic.signed(&fixture, 3, &[0, 2], (0, 2), 4)?;
    let stale = dynamic.signed(&fixture, 4, &[0], (0, 0), 1)?;
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(
        &directory.path().join("source"),
        &fixture.spec,
        Retention::Archive,
    )?;
    let register = (0..3)
        .map(|index| dynamic.register(&fixture, index))
        .collect::<Result<Vec<_>>>()?;
    store.commit(store.prepare(&register, TIME, false)?)?;
    let resign = |command: lineage::Command, expires| -> Result<Transaction> {
        Transaction::new_lineage_command(
            first.cell_deps().collect(),
            first.input(0).unwrap().previous_output,
            expires,
            command,
        )
    };
    let terms = first.lineage_command().unwrap().clone();
    let mut double_target = terms.clone();
    double_target.targets.push(double_target.targets[0].clone());
    assert!(resign(double_target, 1000).is_err());
    let mut altered = terms.clone();
    altered.payload = Bytes::copy_from_slice(&100u64.to_le_bytes());
    let tampered = resign(altered, 1000)?.with_witnesses(first.witnesses().collect())?;
    assert!(store.check(&tampered).is_err());
    let wrong_network = [1; 32].pack();
    let foreign = first.lineage_signing_digest(&wrong_network, &dynamic.fee_cell)?;
    let replay = first.with_witnesses(vec![stem_ledger::GroupWitness {
        group_index: 0,
        proof_data: Bytes::new(),
        seal: Bytes::from(fixture.key.sign_recoverable(&foreign.unpack())?.serialize()),
    }])?;
    assert!(store.check(&replay).is_err());
    let expired = resign(terms, 2)?.with_witnesses(first.witnesses().collect())?;
    assert!(store.check(&expired).is_err());
    assert!(store.live_cell(&dynamic.fee_points[0])?.is_some());
    let transactions = [first.clone(), second.clone(), business_failure.clone()];
    let serial = store.prepare(&transactions, TIME, false)?;
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let parallel = pool.install(|| store.prepare(&transactions, TIME, true))?;
    assert_eq!(serial.header(), parallel.header());
    assert_eq!(serial.receipts(), parallel.receipts());
    assert_eq!(
        parallel
            .receipts()
            .iter()
            .map(|receipt| Outcome::decode(&receipt.protocol_result))
            .collect::<Result<Vec<_>>>()?,
        [Outcome::Applied, Outcome::Applied, Outcome::Rejected],
    );
    store.commit(parallel)?;

    for (index, version, expected) in [(0, 2, 245u128), (1, 1, 243), (2, 0, 240)] {
        let proof = store.prove_current_lineage(&dynamic.ids[index])?;
        assert!(proof.verify(
            store.network(),
            store.tip(),
            &dynamic.ids[index],
            &dynamic.marker
        ));
        let (head, cell, _) = proof.head.context("missing dynamic Lineage head")?;
        assert_eq!(head.version, version);
        assert_eq!(cell.cell.data().as_ref(), &expected.to_le_bytes());
    }
    assert!(store.check(&first).is_err());
    assert!(store.check(&stale).is_err());
    let failed = store.prepare(&[type_failure.clone()], TIME, true)?;
    assert_eq!(
        Outcome::decode(&failed.receipts()[0].protocol_result)?,
        Outcome::Rejected
    );
    let before = store.prove_lineage(&dynamic.ids[0])?.0;
    store.commit(failed)?;
    assert_eq!(store.prove_lineage(&dynamic.ids[0])?.0, before);
    assert!(store.live_cell(&dynamic.fee_points[3])?.is_none());
    assert!(
        store
            .live_cell(&OutPoint::new(type_failure.id(store.network()), 1))?
            .is_none()
    );
    let fee_refund = OutPoint::new(business_failure.id(store.network()), 0);
    assert_eq!(
        store.live_cell(&fee_refund)?.unwrap().cell.capacity(),
        FUND - FEE
    );
    assert!(
        store
            .live_cell(&OutPoint::new(business_failure.id(store.network()), 1))?
            .is_none()
    );
    assert!(store.live_cell(&dynamic.fee_points[2])?.is_none());

    let mut restored = Store::open(
        &directory.path().join("restored"),
        &fixture.spec,
        Retention::Archive,
    )?;
    restored.install_state(
        store.export_state()?,
        &store.tip().app_hash(store.network()),
    )?;
    assert_eq!(
        restored.prove_lineage(&dynamic.ids[0])?.0,
        store.prove_lineage(&dynamic.ids[0])?.0
    );
    assert!(restored.check(&first).is_err());
    let next = dynamic.signed(&fixture, 5, &[0], (2, 2), 1)?;
    assert_eq!(
        restored.check(&next)?.protocol_result,
        Outcome::Applied.encode()
    );
    Ok(())
}

#[test]
#[ignore = "matched local Store benchmark using signed Lineage commands and native RISC-V"]
fn measure_dynamic_lineage_updates() -> Result<()> {
    const WIDTH: usize = 8;
    for (name, targets) in [
        (
            "independent",
            (0..WIDTH).map(|i| vec![i]).collect::<Vec<_>>(),
        ),
        ("single-hotspot", vec![vec![0]; WIDTH]),
        ("atomic", vec![(0..WIDTH).collect()]),
        (
            "mixed",
            [0, 1, 0, 2, 1, 3, 0, 7]
                .into_iter()
                .map(|i| vec![i])
                .collect(),
        ),
    ] {
        let mut fixture = Fixture::new()?;
        fixture.spec.limits.cell_reads = 1024;
        fixture.spec.limits.cell_read_bytes = 64 * 1024 * 1024;
        fixture.spec.limits.transaction_cycles = 15_000_000;
        let lineage = FixtureLineage::fund(&mut fixture)?;
        let dynamic =
            DynamicLineage::fund_using(&mut fixture, &lineage, targets.len() * 31, WIDTH, None)?;
        let batches = (0..31)
            .map(|block| {
                targets
                    .iter()
                    .enumerate()
                    .map(|(i, target_set)| {
                        dynamic.signed(
                            &fixture,
                            block * targets.len() + i,
                            target_set,
                            (0, 1000),
                            1,
                        )
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .collect::<Result<Vec<_>>>()?;
        measure_dynamic_batches(name, &fixture, &dynamic, &batches)?;
    }
    Ok(())
}

fn measure_dynamic_batches(
    name: &str,
    fixture: &Fixture,
    dynamic: &DynamicLineage,
    batches: &[Vec<Transaction>],
) -> Result<()> {
    use std::time::Instant;
    let blocks = batches.len();
    let workers = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let registrations = (0..dynamic.ids.len())
        .map(|i| dynamic.register(fixture, i))
        .collect::<Result<Vec<_>>>()?;
    let temp = tempfile::tempdir()?;
    let mut serial = Store::open(
        &temp.path().join("serial"),
        &fixture.spec,
        Retention::Archive,
    )?;
    let mut parallel = Store::open(
        &temp.path().join("parallel"),
        &fixture.spec,
        Retention::Archive,
    )?;
    serial.commit(serial.prepare(&registrations, TIME, false)?)?;
    parallel.commit(parallel.prepare(&registrations, TIME, true)?)?;
    let mut serial_us = Vec::with_capacity(blocks);
    let mut parallel_us = Vec::with_capacity(blocks);
    let mut commit_us = Vec::with_capacity(blocks);
    let mut total_cycles = 0;
    for batch in batches {
        let started = Instant::now();
        let a = serial.prepare(batch, TIME, false)?;
        serial_us.push(started.elapsed().as_micros() as u64);
        let started = Instant::now();
        let b = workers.install(|| parallel.prepare(batch, TIME, true))?;
        parallel_us.push(started.elapsed().as_micros() as u64);
        assert_eq!(a.header(), b.header());
        assert_eq!(a.receipts(), b.receipts());
        for receipt in b.receipts() {
            assert_eq!(
                lineage::Outcome::decode(&receipt.protocol_result)?,
                lineage::Outcome::Applied
            );
            total_cycles += receipt.cycles;
        }
        serial.commit(a)?;
        let started = Instant::now();
        parallel.commit(b)?;
        commit_us.push(started.elapsed().as_micros() as u64);
    }
    let percentiles = |mut times: Vec<_>| {
        times.sort_unstable();
        [50, 95, 99].map(|p| times[(p * blocks).div_ceil(100) - 1])
    };
    println!(
        "{}",
        serde_json::json!({
            "workload": name, "blocks": blocks, "commands_per_block": batches[0].len(),
            "head_updates_per_block": batches[0].iter().map(|tx| tx.lineage_command().unwrap().targets.len()).sum::<usize>(),
            "serial_prepare_us": percentiles(serial_us),
            "parallel_prepare_us": percentiles(parallel_us),
            "parallel_commit_us": percentiles(commit_us),
            "mean_charged_cycles_per_block": total_cycles / blocks as u64,
        })
    );
    Ok(())
}

fn reserve_data(x: u128, y: u128, tag: u8, owner: [u8; 32]) -> Bytes {
    let mut bytes = Vec::with_capacity(65);
    bytes.extend_from_slice(&x.to_le_bytes());
    bytes.extend_from_slice(&y.to_le_bytes());
    bytes.push(tag);
    bytes.extend_from_slice(&owner);
    bytes.into()
}

fn swap_payload(amount: u128, min_received: u128) -> Bytes {
    let mut bytes = [0; 33];
    bytes[..16].copy_from_slice(&amount.to_le_bytes());
    bytes[16..32].copy_from_slice(&min_received.to_le_bytes());
    Bytes::copy_from_slice(&bytes)
}

fn fund_amm(fixture: &mut Fixture, fees: usize) -> Result<DynamicLineage> {
    let registry = FixtureLineage::fund(fixture)?;
    let mut amm = DynamicLineage::fund_using(fixture, &registry, fees, 3, None)?;
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"));
    let swap = super::contract::native_contract(workspace, "stem-lineage-swap")?;
    let asset = super::contract::native_contract(workspace, "stem-lineage-reserve-udt")?;
    let asset_point = OutPoint::new([232; 32].pack(), 2);
    let replace_code = |data: Bytes| {
        Cell::new(
            Cell::minimum_capacity(&fixture.lock, &[], data.len())?,
            fixture.lock.clone(),
            vec![],
            data,
        )
    };
    amm.executor = blake2b_256(&swap).pack();
    fixture
        .spec
        .development_cells
        .iter_mut()
        .find(|(point, _)| point == &amm.executor_dep)
        .context("Executor fixture")?
        .1 = replace_code(swap)?;
    let code: Byte32 = blake2b_256(&asset).pack();
    fixture
        .spec
        .development_cells
        .push((asset_point.clone(), replace_code(asset)?));
    amm.deps
        .push(CellDep::new_builder().out_point(asset_point).build());
    let types: Vec<_> = [0, 1]
        .into_iter()
        .map(|slot| {
            Script::new_builder()
                .code_hash(code.clone())
                .hash_type(ScriptHashType::Data)
                .args(Bytes::copy_from_slice(&[slot]))
                .build()
        })
        .collect();
    for (index, data) in [
        reserve_data(1_000_000, 1_000_000, 0, [0; 32]),
        reserve_data(
            20_000,
            0,
            1,
            amm.fee_cell.lock().calc_script_hash().unpack(),
        ),
        reserve_data(20_000, 0, 1, [7; 32]),
    ]
    .into_iter()
    .enumerate()
    {
        fixture
            .spec
            .development_cells
            .iter_mut()
            .find(|(point, _)| point == &amm.anchors[index])
            .context("reserve fixture")?
            .1 = Cell::new(FUND, fixture.lock.clone(), types.clone(), data)?;
    }
    Ok(amm)
}

#[test]
fn lineage_amm_enforces_slippage_payer_and_two_asset_groups() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let amm = fund_amm(&mut fixture, 4)?;
    let approved = amm.signed_payload(&fixture, 0, &[0, 1], (0, 2), swap_payload(100, 99))?;
    let slippage = amm.signed_payload(&fixture, 1, &[0, 1], (0, 2), swap_payload(100, 100))?;
    let wrong_payer = amm.signed_payload(&fixture, 2, &[0, 2], (0, 2), swap_payload(100, 1))?;
    let registers = (0..3)
        .map(|i| amm.register(&fixture, i))
        .collect::<Result<Vec<_>>>()?;
    let temp = tempfile::tempdir()?;
    let mut store = Store::open(temp.path(), &fixture.spec, Retention::Archive)?;
    store.commit(store.prepare(&registers, TIME, false)?)?;
    let batch = store.prepare(&[approved, slippage, wrong_payer], TIME, true)?;
    assert_eq!(
        batch
            .receipts()
            .iter()
            .map(|receipt| lineage::Outcome::decode(&receipt.protocol_result))
            .collect::<Result<Vec<_>>>()?,
        [
            lineage::Outcome::Applied,
            lineage::Outcome::Rejected,
            lineage::Outcome::Rejected
        ],
    );
    store.commit(batch)?;
    for (index, version, expected_x, expected_y) in [
        (0, 1, 1_000_100u128, 999_901u128),
        (1, 1, 19_900, 99),
        (2, 0, 20_000, 0),
    ] {
        let (head, cell, _) = store
            .prove_current_lineage(&amm.ids[index])?
            .head
            .context("AMM Head")?;
        assert_eq!(head.version, version);
        assert_eq!(&cell.cell.data()[..16], &expected_x.to_le_bytes());
        assert_eq!(&cell.cell.data()[16..32], &expected_y.to_le_bytes());
        assert_eq!(cell.cell.type_scripts().len(), 3);
    }
    Ok(())
}

#[test]
#[ignore = "matched local Store benchmark of one shared AMM reserve"]
fn measure_lineage_hotspot_amm() -> Result<()> {
    const COMMANDS: usize = 8;
    const BLOCKS: usize = 31;
    let mut fixture = Fixture::new()?;
    fixture.spec.limits.cell_reads = 1024;
    fixture.spec.limits.cell_read_bytes = 64 * 1024 * 1024;
    fixture.spec.limits.transaction_cycles = 15_000_000;
    let amm = fund_amm(&mut fixture, COMMANDS * BLOCKS)?;
    let batches = (0..BLOCKS)
        .map(|block| {
            (0..COMMANDS)
                .map(|i| {
                    amm.signed_payload(
                        &fixture,
                        block * COMMANDS + i,
                        &[0, 1],
                        (0, 1000),
                        swap_payload(10, 1),
                    )
                })
                .collect::<Result<Vec<_>>>()
        })
        .collect::<Result<Vec<_>>>()?;
    measure_dynamic_batches("hotspot-amm", &fixture, &amm, &batches)
}

#[test]
fn wallet_builds_and_seals_the_native_lineage_command() -> Result<()> {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command};

    use ckb_jsonrpc_types::{CellDep as JsonDep, JsonBytes, OutPoint as JsonPoint};
    use serde_json::json;

    let mut fixture = Fixture::new()?;
    let dynamic = DynamicLineage::fund(&mut fixture, 1)?;
    let expected = dynamic.signed(&fixture, 0, &[0, 1], (0, 2), 3)?;
    let terms = expected.lineage_command().unwrap();
    let temp = tempfile::tempdir()?;
    let request = temp.path().join("request.json");
    let genesis = temp.path().join("genesis.json");
    let unsigned = temp.path().join("unsigned.json");
    let context = temp.path().join("context.json");
    let key = temp.path().join("fee-key");
    let lock = temp.path().join("stem-lineage-lock");
    let executor_hash = format!("0x{}", hex::encode(terms.executor.as_slice()));
    let targets: Vec<_> = terms
        .targets
        .iter()
        .map(|target| {
            json!({
                "id": format!("0x{}", hex::encode(target.id.as_slice())),
                "min_version": target.min_version,
                "max_version": target.max_version,
                "min_capacity": target.min_capacity,
            })
        })
        .collect();
    fs::write(
        &request,
        serde_json::to_vec(&json!({
            "fee_input": JsonPoint::from(dynamic.fee_points[0].clone()),
            "cell_deps": expected.cell_deps().map(JsonDep::from).collect::<Vec<_>>(),
            "valid_before_height": expected.valid_before_height(),
            "executor": executor_hash,
            "executor_dep": JsonPoint::from(terms.executor_dep.clone()),
            "executor_cycles": terms.executor_cycles,
            "validation_cycles": terms.validation_cycles,
            "fee": terms.fee,
            "payload": JsonBytes::from_bytes(terms.payload.clone()),
            "targets": targets,
        }))?,
    )?;
    super::comet::write_spec(&genesis, &fixture.spec)?;
    let lock_point = OutPoint::new([232; 32].pack(), 0);
    let cells: Vec<_> = fixture
        .spec
        .development_cells
        .iter()
        .filter(|(point, _)| {
            point == &dynamic.fee_points[0] || point == &terms.executor_dep || point == &lock_point
        })
        .map(|(point, cell)| {
            json!({
                "out_point": JsonPoint::from(point.clone()),
                "cell": JsonBytes::from_vec(cell.as_slice().to_vec()),
            })
        })
        .collect();
    fs::write(&context, serde_json::to_vec(&json!({"cells": cells}))?)?;
    fs::write(&key, [76u8; 32])?;
    fs::set_permissions(&key, fs::Permissions::from_mode(0o600))?;
    fs::write(
        &lock,
        super::contract::native_contract(
            Path::new(env!("CARGO_MANIFEST_DIR")),
            "stem-lineage-lock",
        )?,
    )?;

    let build = Command::new(env!("CARGO_BIN_EXE_stem"))
        .args(["wallet", "lineage", "build", "--request"])
        .arg(&request)
        .output()?;
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    fs::write(&unsigned, build.stdout)?;
    let sign = |pin: &str, max_fee: u64| {
        Command::new(env!("CARGO_BIN_EXE_stem"))
            .args(["wallet", "lineage", "sign"])
            .arg("--genesis")
            .arg(&genesis)
            .arg("--transaction")
            .arg(&unsigned)
            .arg("--context")
            .arg(&context)
            .arg("--code")
            .arg(&lock)
            .arg("--key-file")
            .arg(&key)
            .arg("--executor-hash")
            .arg(pin)
            .arg("--max-fee")
            .arg(max_fee.to_string())
            .output()
    };
    let low_fee_cap = sign(&executor_hash, FEE - 1)?;
    assert!(!low_fee_cap.status.success());
    assert!(String::from_utf8_lossy(&low_fee_cap.stderr).contains("fee cap"));
    let wrong_executor = sign(&format!("0x{}", hex::encode([17; 32])), FEE)?;
    assert!(!wrong_executor.status.success());
    assert!(
        String::from_utf8_lossy(&wrong_executor.stderr).contains("unexpected Lineage Executor")
    );
    let signed = sign(&executor_hash, FEE)?;
    assert!(
        signed.status.success(),
        "{}",
        String::from_utf8_lossy(&signed.stderr)
    );
    let submitted: JsonBytes = serde_json::from_slice(&signed.stdout)?;
    assert_eq!(submitted.as_bytes(), expected.as_slice());
    let mut store = Store::open(&temp.path().join("db"), &fixture.spec, Retention::Archive)?;
    let registrations = [
        dynamic.register(&fixture, 0)?,
        dynamic.register(&fixture, 1)?,
    ];
    store.commit(store.prepare(&registrations, TIME, false)?)?;
    let transaction = Transaction::from_slice(submitted.as_bytes())?;
    assert_eq!(
        store.check(&transaction)?.protocol_result,
        lineage::Outcome::Applied.encode()
    );
    let wallet_exact = temp.path().join("exact.json");
    let lock_script = temp.path().join("fee-lock.json");
    let fee_lock = dynamic.fee_cell.lock();
    let exact = Transaction::new(
        vec![CellDep::new_builder().out_point(lock_point).build()],
        vec![],
        vec![CellInput::new(dynamic.fee_points[0].clone(), None)],
        vec![Cell::new(
            FUND - FEE,
            fee_lock.clone(),
            vec![],
            Bytes::new(),
        )?],
        vec![],
    )?;
    fs::write(
        &wallet_exact,
        serde_json::to_vec(&JsonBytes::from_vec(exact.as_slice().to_vec()))?,
    )?;
    fs::write(
        &lock_script,
        serde_json::to_vec(&ckb_jsonrpc_types::Script::from(fee_lock))?,
    )?;
    let cancel = Command::new(env!("CARGO_BIN_EXE_stem"))
        .args(["wallet", "sign", "--group-index", "0"])
        .arg("--genesis")
        .arg(&genesis)
        .arg("--transaction")
        .arg(&wallet_exact)
        .arg("--context")
        .arg(&context)
        .arg("--lock")
        .arg(&lock_script)
        .arg("--lineage-code")
        .arg(&lock)
        .arg("--key-file")
        .arg(&key)
        .output()?;
    assert!(
        cancel.status.success(),
        "{}",
        String::from_utf8_lossy(&cancel.stderr)
    );
    let bytes: JsonBytes = serde_json::from_slice(&cancel.stdout)?;
    let cancel = Transaction::from_slice(bytes.as_bytes())?;
    store.commit(store.prepare(&[cancel], TIME, false)?)?;
    assert!(store.check(&transaction).is_err());
    Ok(())
}
