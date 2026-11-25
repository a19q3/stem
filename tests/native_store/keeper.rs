use ckb_types::prelude::*;
use stem::native_store::KeeperPolicy;
use stem_ledger::{
    GroupWitness,
    keeper::{Action, decode_queue},
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
    let carrier = Transaction::new(
        fixture.deps.clone(),
        vec![],
        vec![CellInput::new(carrier_input(index), None)],
        vec![Cell::new(
            FUND - 2 * FEE,
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
    height: u64,
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
        height,
        fee_input: point,
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
    assert!(store.check(&cancel(&fixture, &dynamic, 3, 4)?).is_err());

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

    let cancel = cancel(&fixture, &dynamic, 3, 4)?;
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
    use std::time::Instant;

    const BLOCKS: usize = 31;
    const WIDTH: usize = 4;
    let mut fixture = Fixture::new()?;
    let dynamic = DynamicLineage::fund(&mut fixture, BLOCKS * WIDTH)?;
    fund_carriers(&mut fixture, (BLOCKS * WIDTH) as u32)?;
    let dir = tempfile::tempdir()?;
    let mut idle = Store::open(&dir.path().join("idle"), &fixture.spec, Retention::Archive)?;
    let mut active = Store::open(
        &dir.path().join("active"),
        &fixture.spec,
        Retention::Archive,
    )?;
    let register = dynamic.register(&fixture, 0)?;
    for store in [&mut idle, &mut active] {
        store.commit(store.prepare(std::slice::from_ref(&register), TIME, false)?)?;
    }
    let first_due = BLOCKS as u64 + 2;
    for height_offset in 0..BLOCKS {
        let due = first_due + height_offset as u64;
        let claims = (0..WIDTH)
            .map(|index| {
                let i = height_offset * WIDTH + index;
                let next = dynamic.scheduled(&fixture, i, (0, 1000), 1, due)?;
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
            let block = store.prepare(&[], TIME, false)?;
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
            "percentile_order": [50, 95, 99],
            "idle_prepare_us": values[0], "active_prepare_us": values[1],
            "idle_commit_us": values[2], "active_commit_us": values[3],
            "active_mean_charged_cycles": charged / BLOCKS as u64,
        })
    );
    Ok(())
}
