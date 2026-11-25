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
