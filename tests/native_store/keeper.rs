use ckb_types::prelude::*;
use stem::native_store::{KeeperPolicy, KeeperTimePolicy};
use stem_ledger::{
    GroupWitness,
    keeper::{Action, Trigger, decode_agenda, decode_queue},
    lineage::Outcome,
    proof::Tree,
};

use super::{
    Bytes, Cell, CellInput, FEE, FUND, Fixture, OutPoint, Result, Retention, Store, TIME,
    Transaction, lineage::DynamicLineage,
};

pub(super) fn fund_carriers(fixture: &mut Fixture, count: u32) -> Result<()> {
    fixture.spec.keeper = Some(KeeperPolicy {
        max_delay: 64,
        per_height: 4,
        reservation_fee: FEE,
        time: None,
    });
    fixture.spec.limits.cell_reads = fixture.spec.limits.cell_reads.max(256);
    for index in 0..count {
        fixture.spec.development_cells.push((
            carrier_input(index),
            Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?,
        ));
    }
    Ok(())
}

fn carrier_input(index: u32) -> OutPoint {
    OutPoint::new([244; 32].pack(), index)
}

pub(super) fn schedule(
    fixture: &Fixture,
    index: u32,
    command: &Transaction,
) -> Result<Transaction> {
    schedule_with_fee(fixture, index, command, 2 * FEE)
}

pub(super) fn schedule_with_fee(
    fixture: &Fixture,
    index: u32,
    command: &Transaction,
    fee: u64,
) -> Result<Transaction> {
    let carrier = Transaction::new(
        fixture.deps.clone(),
        vec![],
        vec![CellInput::new(carrier_input(index), None)],
        vec![Cell::new(
            FUND - fee,
            fixture.lock.clone(),
            vec![],
            Bytes::new(),
        )?],
        vec![],
    )?
    .with_keeper_action(Action::schedule(command)?)?;
    fixture.seal(carrier)
}

fn cancel(
    fixture: &Fixture,
    dynamic: &DynamicLineage,
    fee: usize,
    trigger: Trigger,
    scheduled: &Transaction,
) -> Result<Transaction> {
    let point = dynamic.fee_points[fee].clone();
    let lock = dynamic.fee_cell.lock();
    let tx = Transaction::new(
        dynamic.deps.clone(),
        vec![],
        vec![CellInput::new(point.clone(), None)],
        vec![Cell::new(FUND - FEE, lock.clone(), vec![], Bytes::new())?],
        vec![],
    )?
    .with_keeper_action(Action::Cancel {
        trigger,
        fee_input: point,
        command_id: scheduled.id(&fixture.spec.network),
    })?;
    let digest = tx.signing_digest(
        &fixture.spec.network,
        0,
        &lock,
        |point| {
            fixture
                .spec
                .development_cells
                .iter()
                .find(|(key, _)| key == point)
                .map(|(_, cell)| cell.clone())
                .ok_or_else(|| anyhow::anyhow!("missing fixture Cell"))
        },
        |_| anyhow::bail!("no Header Deps"),
    )?;
    tx.with_witnesses(vec![GroupWitness {
        group_index: 0,
        proof_data: Bytes::new(),
        seal: Bytes::from(fixture.key.sign_recoverable(&digest.unpack())?.serialize()),
    }])
}

#[test]
#[cfg(any(
    feature = "native-keeper-parallel-preparation",
    feature = "native-reuse-keeper-proposal-prefix"
))]
fn keeper_prepares_across_an_ordered_head_dependency() -> Result<()> {
    use super::lineage::FixtureLineage;
    use anyhow::Context;
    use std::num::NonZeroUsize;
    use stem::native_store::LiveCellIndexLayout;

    let mut fixture = Fixture::new()?;
    fixture.spec.limits.transaction_count = 16;
    let registry = FixtureLineage::fund(&mut fixture)?;
    let dynamic = DynamicLineage::fund_using(&mut fixture, &registry, 4, 2, None)?;
    fund_carriers(&mut fixture, 4)?;
    fixture.spec.keeper.as_mut().unwrap().time = Some(KeeperTimePolicy {
        per_height: 1,
        max_delay_ms: 10_000,
    });
    let target = TIME + 100;
    let height0 = dynamic.scheduled(&fixture, 0, (0, 0), 1, 4)?;
    let height1 = dynamic.scheduled(&fixture, 1, (1, 1), 2, 4)?;
    let timed = dynamic.signed(&fixture, 2, &[1], (0, 0), 3)?;
    let timed = dynamic.sign_command(
        &fixture,
        timed.with_witnesses(vec![])?
            .with_valid_before_height(12)?
            .with_keeper_trigger(Trigger::Time(target))?,
    )?;
    let stale = dynamic.scheduled(&fixture, 3, (0, 0), 9, 4)?;
    let reservations = [&height0, &height1, &stale, &timed]
        .iter()
        .enumerate()
        .map(|(index, command)| schedule(&fixture, index as u32, command))
        .collect::<Result<Vec<_>>>()?;
    let mut payments = Vec::new();
    let mut point = fixture.spend.clone();
    for fee_count in 1..=10 {
        let tx = fixture.signed(point, None, FUND - fee_count * FEE, None)?;
        point = OutPoint::new(tx.id(&fixture.spec.network), 0);
        payments.push(tx);
    }
    let registrations = [dynamic.register(&fixture, 0)?, dynamic.register(&fixture, 1)?];
    let dir = tempfile::tempdir()?;
    let mut serial = Store::open(&dir.path().join("serial"), &fixture.spec, Retention::Archive)?;
    let mut parallel = Store::open(
        &dir.path().join("parallel"),
        &fixture.spec,
        Retention::Archive,
    )?;
    let limit = NonZeroUsize::new(8).unwrap();
    parallel.enable_live_cell_index_with_layout(limit, LiveCellIndexLayout::InternedScripts)?;
    parallel.enable_cell_node_index(limit)?;
    for store in [&mut serial, &mut parallel] {
        store.commit(store.prepare(&registrations, TIME, false)?)?;
        store.commit(store.prepare(&reservations, TIME, false)?)?;
        store.commit(store.prepare(&[], target, false)?)?;
    }
    let workers = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let reference = serial.prepare(&payments, target, false)?;
    let candidate = workers.install(|| parallel.prepare(&payments, target, true))?;
    let (selected, leader) =
        workers.install(|| parallel.build_proposal(&payments, target, 64 * 1024))?;
    assert!(
        selected.iter().map(Transaction::as_slice)
            .eq(payments.iter().map(Transaction::as_slice))
    );
    assert_eq!(candidate.header(), reference.header());
    assert_eq!(leader.header(), reference.header());
    assert_eq!(candidate.receipts(), reference.receipts());
    assert_eq!(leader.receipts(), reference.receipts());
    assert_eq!(leader.submitted_receipts().len(), payments.len());
    assert_eq!(candidate.receipts().len(), payments.len() + 4);
    assert_eq!(
        candidate.receipts()[..4]
            .iter()
            .map(|receipt| Outcome::decode(&receipt.protocol_result))
            .collect::<Result<Vec<_>>>()?,
        [Outcome::Applied, Outcome::Applied, Outcome::Skipped, Outcome::Applied]
    );
    serial.commit(reference)?;
    parallel.commit(leader)?;
    drop(parallel);
    let parallel = Store::open(&dir.path().join("parallel"), &fixture.spec, Retention::Archive)?;
    let (expected_transactions, expected_receipts) =
        serial.execution(4)?.context("missing serial execution")?;
    let (actual_transactions, actual_receipts) =
        parallel.execution(4)?.context("missing reopened execution")?;
    assert!(
        actual_transactions.iter().map(Transaction::as_slice)
            .eq(expected_transactions.iter().map(Transaction::as_slice))
    );
    assert_eq!(actual_receipts, expected_receipts);
    let (expected_cell, expected_path) = serial.prove_cell(&point)?;
    let (actual_cell, actual_path) = parallel.prove_cell(&point)?;
    assert_eq!(
        stem_ledger::record::encode_cell(&point, &expected_cell.context("missing serial payment output")?),
        stem_ledger::record::encode_cell(&point, &actual_cell.context("missing resident payment output")?),
    );
    assert_eq!(expected_path.encode()?, actual_path.encode()?);
    for (id, version) in dynamic.ids.iter().zip([2, 1]) {
        let a = serial.prove_current_lineage(id)?;
        let b = parallel.prove_current_lineage(id)?;
        assert_eq!(a.system_path.encode()?, b.system_path.encode()?);
        assert_eq!(b.head.context("missing settled Head")?.0.version, version);
    }
    assert!(parallel.prove_keeper_agenda()?.0.is_none());
    assert!(parallel.prove_keeper_queue(4)?.0.is_none());
    for point in &dynamic.fee_points {
        let (expected, path) = serial.prove_keeper_reservation(point)?;
        let (actual, actual_path) = parallel.prove_keeper_reservation(point)?;
        assert!(actual.is_none());
        assert_eq!(expected, actual);
        assert_eq!(path.encode()?, actual_path.encode()?);
    }
    Ok(())
}

#[test]
fn keeper_automatically_executes_committed_commands_after_restore() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let dynamic = DynamicLineage::fund(&mut fixture, 5)?;
    fund_carriers(&mut fixture, 9)?;
    let first = dynamic.scheduled(&fixture, 0, (0, 2), 2, 4)?;
    let second = dynamic.scheduled(&fixture, 1, (1, 2), 3, 4)?;
    let stale = dynamic.scheduled(&fixture, 2, (0, 0), 9, 4)?;
    let canceled = dynamic.scheduled(&fixture, 3, (0, 4), 7, 4)?;
    let overflow = dynamic.scheduled(&fixture, 4, (0, 4), 7, 4)?;
    let far = dynamic.scheduled(&fixture, 4, (0, 8), 1, 90)?;

    let dir = tempfile::tempdir()?;
    let mut store = Store::open(&dir.path().join("a"), &fixture.spec, Retention::Archive)?;
    let register = dynamic.register(&fixture, 0)?;
    store.commit(store.prepare(&[register], TIME, false)?)?;
    assert!(store.check(&first).is_err());
    assert!(store.check(&schedule(&fixture, 6, &far)?).is_err());
    let mut invalid_seal = first.witness(0).unwrap();
    invalid_seal.seal = Bytes::from_static(&[255; 65]);
    let forged = first.with_witnesses(vec![invalid_seal])?;
    assert!(store.check(&schedule(&fixture, 7, &forged)?).is_err());
    assert!(
        store
            .check(&cancel(
                &fixture,
                &dynamic,
                3,
                Trigger::Height(4),
                &canceled
            )?)
            .is_err()
    );

    let claims = [&canceled, &second, &stale, &first]
        .into_iter()
        .enumerate()
        .map(|(i, cmd)| schedule(&fixture, i as u32, cmd))
        .collect::<Result<Vec<_>>>()?;
    let serial = store.prepare(&claims, TIME, false)?;
    let workers = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;
    let parallel = workers.install(|| store.prepare(&claims, TIME, true))?;
    assert_eq!(parallel.header(), serial.header());
    assert_eq!(parallel.receipts(), serial.receipts());
    assert!(
        parallel
            .receipts()
            .iter()
            .all(|receipt| receipt.cycles > 3_800_000)
    );
    store.commit(parallel)?;
    assert!(store.check(&schedule(&fixture, 8, &overflow)?).is_err());
    let duplicate = dynamic.scheduled(&fixture, 0, (2, 4), 2, 5)?;
    assert!(store.check(&schedule(&fixture, 8, &duplicate)?).is_err());

    let (record, path) = store.prove_keeper_queue(4)?;
    assert!(Store::verify_keeper_queue(
        store.network(),
        store.tip(),
        4,
        record.as_deref(),
        &path
    ));
    assert!(!Store::verify_keeper_queue(
        store.network(),
        store.tip(),
        5,
        record.as_deref(),
        &path
    ));
    let queue = decode_queue(record.as_deref().unwrap(), 4)?;
    assert_eq!(
        queue
            .iter()
            .map(|cmd| cmd.id(store.network()))
            .collect::<Vec<_>>(),
        [&first, &second, &stale, &canceled]
            .iter()
            .map(|cmd| cmd.id(store.network()))
            .collect::<Vec<_>>()
    );

    let cancel = cancel(&fixture, &dynamic, 3, Trigger::Height(4), &canceled)?;
    store.commit(store.prepare(&[cancel], TIME, false)?)?;
    let mut restored = Store::open(&dir.path().join("b"), &fixture.spec, Retention::Archive)?;
    restored.install_state(
        store.export_state()?,
        &store.tip().app_hash(store.network()),
    )?;
    assert!(restored.check(&first).is_err());
    assert!(
        restored
            .prepare(std::slice::from_ref(&first), TIME, false)
            .is_err()
    );
    assert!(
        restored
            .select(vec![first.clone()], TIME, 1_000_000)?
            .is_empty()
    );
    drop(store);
    let store = Store::open(&dir.path().join("a"), &fixture.spec, Retention::Archive)?;
    let payment = fixture.signed(fixture.spend.clone(), None, FUND - FEE, None)?;
    let on_disk = store.prepare(std::slice::from_ref(&payment), TIME, false)?;
    let recovered = workers.install(|| restored.prepare(&[payment], TIME, true))?;
    assert_eq!(on_disk.header(), recovered.header());
    assert_eq!(on_disk.receipts(), recovered.receipts());
    assert_eq!(recovered.submitted_receipts().len(), 1);
    assert_eq!(
        recovered.receipts()[..3]
            .iter()
            .map(|r| Outcome::decode(&r.protocol_result))
            .collect::<Result<Vec<_>>>()?,
        [Outcome::Applied, Outcome::Applied, Outcome::Skipped]
    );
    restored.commit(recovered)?;

    let (record, path) = restored.prove_keeper_queue(4)?;
    assert!(
        record.is_none()
            && Store::verify_keeper_queue(restored.network(), restored.tip(), 4, None, &path)
    );
    assert!(restored.live_cell(&dynamic.fee_points[0])?.is_none());
    assert!(restored.live_cell(&dynamic.fee_points[1])?.is_none());
    assert!(restored.live_cell(&dynamic.fee_points[2])?.is_some());
    let proof = restored.prove_current_lineage(&dynamic.ids[0])?;
    let (head, cell, _) = proof.head.unwrap();
    assert_eq!(head.version, 2);
    assert_eq!(cell.cell.data().as_ref(), &245u128.to_le_bytes());
    assert_eq!(
        head.out_point,
        OutPoint::new(second.id(restored.network()), 1)
    );
    let (txs, receipts) = restored.execution(4)?.unwrap();
    assert_eq!(txs.len(), 4);
    let root = Tree::transactions(restored.network().clone(), &txs)?;
    assert_eq!(root.root(), restored.tip().context.transaction_root);
    assert!(
        root.prove(2)?
            .verify_transaction(restored.network(), &root.root(), &stale)
    );
    let receipt_root = Tree::receipts(restored.network().clone(), &receipts)?;
    assert_eq!(receipt_root.root(), restored.tip().receipt_root);
    assert_eq!(receipts[2].fee, 0);
    assert!(receipts[2].cycles > 0);
    let fee_after_attempt = restored.fee_surplus()?;
    assert_eq!(fee_after_attempt, 13 * FEE);

    let again = dynamic.scheduled(&fixture, 2, (2, 2), 1, 6)?;
    assert!(restored.check(&schedule(&fixture, 8, &again)?).is_ok());
    assert_eq!(restored.tip().context.height, 4);
    Ok(())
}

#[test]
fn paired_signature_cannot_authorize_a_keeper_action() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let swap = super::intent::Swap::fund(&mut fixture)?;
    let dynamic = DynamicLineage::fund(&mut fixture, 1)?;
    fund_carriers(&mut fixture, 0)?;
    let temp = tempfile::tempdir()?;
    let genesis = temp.path().join("genesis.json");
    super::comet::write_spec(&genesis, &fixture.spec)?;
    let authorized = swap.sign(&fixture, &genesis, 10)?;
    let store = Store::open(&temp.path().join("db"), &fixture.spec, Retention::Archive)?;
    assert!(store.check(&authorized).is_ok());

    let command = dynamic.scheduled(&fixture, 0, (0, 10), 1, 4)?;
    let modified = authorized
        .with_witnesses(vec![])?
        .with_keeper_action(Action::schedule(&command)?)?
        .with_witnesses(authorized.witnesses().collect())?;
    let error = store.check(&modified).unwrap_err();
    assert!(
        format!("{error:#}").contains("Lock Script Group"),
        "{error:#}"
    );
    assert!(store.prove_keeper_queue(4)?.0.is_none());
    Ok(())
}

#[test]
#[ignore = "matched local measurement of a configured idle Keeper and four due commands"]
fn measure_keeper_slots() -> Result<()> {
    use super::lineage::FixtureLineage;
    use std::time::Instant;

    const BLOCKS: usize = 31;
    const WIDTH: usize = 4;
    let heads: usize = std::env::var("STEM_KEEPER_BENCH_HEADS")
        .ok()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(1);
    let worker_count: usize = std::env::var("STEM_KEEPER_BENCH_WORKERS")
        .ok()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(1);
    anyhow::ensure!(
        (1..=WIDTH).contains(&heads) && worker_count != 0,
        "invalid Keeper fixture size"
    );
    let workers = rayon::ThreadPoolBuilder::new().num_threads(worker_count).build()?;
    let mut fixture = Fixture::new()?;
    let dynamic = if heads == 1 {
        DynamicLineage::fund(&mut fixture, BLOCKS * WIDTH)?
    } else {
        let registry = FixtureLineage::fund(&mut fixture)?;
        DynamicLineage::fund_using(&mut fixture, &registry, BLOCKS * WIDTH, heads, None)?
    };
    fund_carriers(&mut fixture, (BLOCKS * WIDTH) as u32)?;
    let dir = tempfile::tempdir()?;
    let mut idle = Store::open(&dir.path().join("idle"), &fixture.spec, Retention::Archive)?;
    let mut active = Store::open(
        &dir.path().join("active"),
        &fixture.spec,
        Retention::Archive,
    )?;
    let registrations = (0..heads)
        .map(|index| dynamic.register(&fixture, index))
        .collect::<Result<Vec<_>>>()?;
    for store in [&mut idle, &mut active] {
        store.commit(store.prepare(&registrations, TIME, false)?)?;
    }
    let first_due = BLOCKS as u64 + 2;
    for height_offset in 0..BLOCKS {
        let due = first_due + height_offset as u64;
        let claims = (0..WIDTH)
            .map(|index| {
                let i = height_offset * WIDTH + index;
                let next = if heads == 1 {
                    dynamic.scheduled(&fixture, i, (0, 1000), 1, due)?
                } else {
                    let tx = dynamic.signed(&fixture, i, &[index % heads], (0, 1000), 1)?;
                    dynamic.sign_command(&fixture, tx.with_witnesses(vec![])?.with_execute_at(due)?)?
                };
                schedule(&fixture, i as u32, &next)
            })
            .collect::<Result<Vec<_>>>()?;
        idle.commit(idle.prepare(&[], TIME, false)?)?;
        active.commit(active.prepare(&claims, TIME, false)?)?;
    }

    let mut timings = std::array::from_fn::<_, 4, _>(|_| Vec::with_capacity(BLOCKS));
    let mut charged = 0u64;
    for index in 0..BLOCKS {
        // Alternate the first database to reduce CPU and allocator ordering bias.
        for slot in [index % 2, 1 - index % 2] {
            let store = if slot == 0 { &mut idle } else { &mut active };
            let start = Instant::now();
            let block = if worker_count > 1 {
                workers.install(|| store.prepare(&[], TIME, true))?
            } else {
                store.prepare(&[], TIME, false)?
            };
            timings[slot].push(start.elapsed().as_micros() as u64);
            if slot == 1 {
                assert_eq!(block.receipts().len(), WIDTH);
                for receipt in block.receipts() {
                    assert_eq!(Outcome::decode(&receipt.protocol_result)?, Outcome::Applied);
                    charged += receipt.cycles;
                }
            } else {
                assert!(block.receipts().is_empty());
            }
            assert!(block.submitted_receipts().is_empty());
            let start = Instant::now();
            store.commit(block)?;
            timings[2 + slot].push(start.elapsed().as_micros() as u64);
        }
    }
    let values = timings.map(|mut times| {
        times.sort_unstable();
        [50, 95, 99].map(|p| times[(p * BLOCKS).div_ceil(100) - 1])
    });
    println!(
        "{}",
        serde_json::json!({
            "blocks": BLOCKS, "due_per_active_block": WIDTH,
            "heads": heads, "workers": worker_count,
            "parallel_preparation": cfg!(feature = "native-keeper-parallel-preparation"),
            "percentile_order": [50, 95, 99],
            "idle_prepare_us": values[0], "active_prepare_us": values[1],
            "idle_commit_us": values[2], "active_commit_us": values[3],
            "active_mean_charged_cycles": charged / BLOCKS as u64,
        })
    );
    Ok(())
}

#[test]
fn keeper_time_uses_parent_header_and_bounded_slots_after_restore() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let dynamic = DynamicLineage::fund(&mut fixture, 5)?;
    fund_carriers(&mut fixture, 8)?;
    fixture.spec.keeper.as_mut().unwrap().time = Some(KeeperTimePolicy {
        per_height: 1,
        max_delay_ms: 10_000,
    });
    let target = TIME + 100;
    let first = dynamic.timed(&fixture, 0, (1, 1), 3, target, 12)?;
    let second = dynamic.timed(&fixture, 1, (2, 2), 4, target, 12)?;
    let expired = dynamic.timed(&fixture, 2, (0, 9), 50, target, 4)?;
    let canceled = dynamic.timed(&fixture, 3, (0, 9), 100, target + 1, 12)?;
    let height_bound = dynamic.scheduled(&fixture, 4, (0, 0), 2, 4)?;
    let tmp = tempfile::tempdir()?;
    let mut store = Store::open(&tmp.path().join("a"), &fixture.spec, Retention::Archive)?;
    store.commit(store.prepare(&[dynamic.register(&fixture, 0)?], TIME, false)?)?;
    assert!(store.check(&first).is_err());
    let too_far = dynamic.timed(&fixture, 0, (0, 0), 1, TIME + 10_001, 12)?;
    assert!(store.check(&schedule(&fixture, 5, &too_far)?).is_err());
    let forged = first
        .with_witnesses(vec![])?
        .with_keeper_trigger(Trigger::Time(target + 1))?
        .with_witnesses(vec![first.witness(0).unwrap()])?;
    assert!(store.check(&schedule(&fixture, 5, &forged)?).is_err());
    let duplicate = dynamic.scheduled(&fixture, 0, (1, 1), 3, 4)?;
    let reservations = [&second, &expired, &canceled, &first, &height_bound]
        .into_iter()
        .enumerate()
        .map(|(i, cmd)| schedule(&fixture, i as u32, cmd))
        .collect::<Result<Vec<_>>>()?;
    store.commit(store.prepare(&reservations, TIME, false)?)?;
    assert!(store.check(&schedule(&fixture, 5, &duplicate)?).is_err());
    assert!(
        store
            .check(&cancel(
                &fixture,
                &dynamic,
                3,
                Trigger::Time(target),
                &canceled
            )?)
            .is_err()
    );
    let (agenda, path) = store.prove_keeper_agenda()?;
    assert!(Store::verify_keeper_agenda(
        store.network(),
        store.tip(),
        agenda.as_deref(),
        &path
    ));
    let agenda = decode_agenda(agenda.as_deref().unwrap())?;
    assert_eq!(agenda.len(), 4);
    let (record, proof) = store.prove_keeper_timed_command(&dynamic.fee_points[0])?;
    assert_eq!(record.as_deref(), Some(first.as_slice()));
    assert!(Store::verify_keeper_timed_command(
        store.network(),
        store.tip(),
        &dynamic.fee_points[0],
        record.as_deref(),
        &proof
    ));
    assert!(!Store::verify_keeper_timed_command(
        store.network(),
        store.tip(),
        &dynamic.fee_points[1],
        record.as_deref(),
        &proof
    ));

    // This proposal jumps across the scheduled time. It is not a committed
    // time until the following block and cannot trigger an early execution.
    let cancel = cancel(&fixture, &dynamic, 3, Trigger::Time(target + 1), &canceled)?;
    let jumped = store.prepare(&[cancel], target + 1_000, false)?;
    assert_eq!(jumped.receipts().len(), 1);
    store.commit(jumped)?;
    let mut restored = Store::open(&tmp.path().join("b"), &fixture.spec, Retention::Archive)?;
    restored.install_state(
        store.export_state()?,
        &store.tip().app_hash(store.network()),
    )?;
    drop(store);
    let mut store = Store::open(&tmp.path().join("a"), &fixture.spec, Retention::Archive)?;
    assert!(
        store
            .select(vec![first.clone()], target + 1_000, 1_000_000)?
            .is_empty()
    );
    let pool = rayon::ThreadPoolBuilder::new().num_threads(4).build()?;

    for (offset, expected) in [
        vec![
            (&height_bound, Outcome::Applied),
            (&expired, Outcome::Skipped),
        ],
        vec![(&first, Outcome::Applied)],
        vec![(&second, Outcome::Applied)],
        vec![],
    ]
    .into_iter()
    .enumerate()
    {
        let height = 4 + offset as u64;
        let serial = store.prepare(&[], target + 1_000, false)?;
        let parallel = pool.install(|| restored.prepare(&[], target + 1_000, true))?;
        let (submitted, proposed) = pool.install(|| {
            restored.build_proposal(
                std::iter::empty::<&Transaction>(),
                target + 1_000,
                1_000_000,
            )
        })?;
        assert!(submitted.is_empty());
        assert_eq!(serial.header(), proposed.header());
        assert_eq!(serial.receipts(), proposed.receipts());
        assert_eq!(serial.header(), parallel.header());
        assert_eq!(serial.receipts(), parallel.receipts());
        let receipts = serial
            .receipts()
            .iter()
            .map(|r| Outcome::decode(&r.protocol_result))
            .collect::<Result<Vec<_>>>()?;
        assert_eq!(
            receipts,
            expected
                .iter()
                .map(|(_, outcome)| *outcome)
                .collect::<Vec<_>>()
        );
        for ((_, outcome), receipt) in expected.iter().zip(serial.receipts()) {
            if *outcome == Outcome::Skipped {
                assert_eq!(receipt.fee, 0);
                assert!(receipt.cycles > 0);
            }
        }
        store.commit(serial)?;
        restored.commit(parallel)?;
        let (transactions, _) = restored.execution(height)?.unwrap();
        assert_eq!(
            transactions
                .iter()
                .map(|tx| tx.id(restored.network()))
                .collect::<Vec<_>>(),
            expected
                .iter()
                .map(|(tx, _)| tx.id(restored.network()))
                .collect::<Vec<_>>()
        );
    }
    assert!(restored.live_cell(&dynamic.fee_points[2])?.is_some());
    assert!(restored.live_cell(&dynamic.fee_points[3])?.is_none());
    assert!(restored.live_cell(&dynamic.fee_points[0])?.is_none());
    assert!(restored.live_cell(&dynamic.fee_points[1])?.is_none());
    let (agenda, path) = restored.prove_keeper_agenda()?;
    assert!(agenda.is_none());
    assert!(Store::verify_keeper_agenda(
        restored.network(),
        restored.tip(),
        None,
        &path
    ));
    let (record, proof) = restored.prove_keeper_timed_command(&dynamic.fee_points[2])?;
    assert!(record.is_none());
    assert!(Store::verify_keeper_timed_command(
        restored.network(),
        restored.tip(),
        &dynamic.fee_points[2],
        None,
        &proof
    ));
    let head = restored
        .prove_current_lineage(&dynamic.ids[0])?
        .head
        .unwrap()
        .0;
    assert_eq!(head.version, 3);
    assert_eq!(
        head.out_point,
        OutPoint::new(second.id(restored.network()), 1)
    );
    let rescheduled = dynamic.timed(&fixture, 2, (3, 3), 1, target + 2_000, 12)?;
    assert!(
        restored
            .check(&schedule(&fixture, 6, &rescheduled)?)
            .is_ok()
    );
    Ok(())
}

#[test]
#[ignore = "matched local measurement of a full sleeping agenda and a timed burst"]
fn measure_keeper_time_agenda() -> Result<()> {
    use std::time::Instant;

    const BLOCKS: usize = 31;
    const WIDTH: usize = 4;
    const SIZE: usize = stem_ledger::keeper::MAX_AGENDA_PENDING;
    let mut fixture = Fixture::new()?;
    let dynamic = DynamicLineage::fund(&mut fixture, SIZE + 1)?;
    fund_carriers(&mut fixture, (SIZE + 1) as u32)?;
    let policy = fixture.spec.keeper.as_mut().unwrap();
    policy.per_height = 2;
    policy.time = Some(KeeperTimePolicy {
        per_height: WIDTH as u16,
        max_delay_ms: 120_000,
    });
    let dir = tempfile::tempdir()?;
    let mut idle = Store::open(&dir.path().join("idle"), &fixture.spec, Retention::Archive)?;
    let mut sleeping = Store::open(
        &dir.path().join("active"),
        &fixture.spec,
        Retention::Archive,
    )?;
    let register = dynamic.register(&fixture, 0)?;
    for store in [&mut idle, &mut sleeping] {
        store.commit(store.prepare(std::slice::from_ref(&register), TIME, false)?)?;
    }
    let target = TIME + 100_000;
    let mut commands = Vec::new();
    for offset in (0..SIZE).step_by(WIDTH) {
        let claims = (offset..offset + WIDTH)
            .map(|i| {
                let command = dynamic.timed(&fixture, i, (0, 1000), 1, target, 64)?;
                let carrier = schedule(&fixture, i as u32, &command)?;
                commands.push(command);
                Ok(carrier)
            })
            .collect::<Result<Vec<_>>>()?;
        idle.commit(idle.prepare(&[], TIME, false)?)?;
        sleeping.commit(sleeping.prepare(&claims, TIME, false)?)?;
    }
    let overflow = dynamic.timed(&fixture, SIZE, (0, 1000), 1, target, 64)?;
    let error = sleeping
        .check(&schedule(&fixture, SIZE as u32, &overflow)?)
        .unwrap_err();
    assert!(format!("{error:#}").contains("agenda is full"));
    let agenda = sleeping.prove_keeper_agenda()?.0.unwrap();
    assert_eq!(decode_agenda(&agenda)?.len(), SIZE);
    assert_eq!(agenda.len(), stem_ledger::keeper::agenda_bytes(SIZE));
    let mut measurements = Vec::with_capacity(BLOCKS * 2);
    for index in 0..BLOCKS {
        for slot in [index % 2, 1 - index % 2] {
            let store = if slot == 0 { &mut idle } else { &mut sleeping };
            let now = Instant::now();
            let block = store.prepare(&[], TIME, false)?;
            let prepare_ns = now.elapsed().as_nanos();
            assert!(block.receipts().is_empty());
            let now = Instant::now();
            store.commit(block)?;
            measurements.push(serde_json::json!({
                "phase": if slot == 0 {"empty"} else {"sleeping"},
                "height": store.tip().context.height,
                "prepare_ns": prepare_ns, "commit_ns": now.elapsed().as_nanos(),
            }));
        }
    }
    let jump = sleeping.prepare(&[], target, false)?;
    assert!(jump.receipts().is_empty());
    sleeping.commit(jump)?;
    let mut burst = Vec::new();
    for _ in 0..SIZE / WIDTH {
        let now = Instant::now();
        let block = sleeping.prepare(&[], target, false)?;
        let prepare_ns = now.elapsed().as_nanos();
        assert_eq!(block.receipts().len(), WIDTH);
        let mut cycles = 0;
        for receipt in block.receipts() {
            assert_eq!(Outcome::decode(&receipt.protocol_result)?, Outcome::Applied);
            cycles += receipt.cycles;
        }
        let now = Instant::now();
        sleeping.commit(block)?;
        burst.push(serde_json::json!({
            "height": sleeping.tip().context.height,
            "prepare_ns": prepare_ns, "commit_ns": now.elapsed().as_nanos(), "charged_cycles": cycles,
        }));
    }
    assert!(sleeping.prove_keeper_agenda()?.0.is_none());
    let head = sleeping
        .prove_current_lineage(&dynamic.ids[0])?
        .head
        .unwrap()
        .0;
    assert_eq!(head.version, SIZE as u64);
    println!(
        "{}",
        serde_json::json!({
            "pending": SIZE, "agenda_bytes": agenda.len(), "blocks": BLOCKS,
            "timed_slots_per_block": WIDTH, "empty_and_sleeping": measurements,
            "burst": burst, "rejected_overflow": format!("{error:#}"),
            "command_bytes": commands.iter().map(|tx| tx.as_slice().len()).collect::<Vec<_>>(),
        })
    );
    Ok(())
}
