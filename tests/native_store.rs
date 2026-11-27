use anyhow::{Context, Result};
use ckb_crypto::secp::Privkey;
use ckb_hash::blake2b_256;
use ckb_types::{
    bytes::Bytes,
    core::ScriptHashType,
    packed::{CellDep, OutPoint, Script},
    prelude::*,
};
use stem::native_store::{Limits, Retention, Spec, Store};
use stem_ledger::{
    CapacityPolicy, Cell, CellInput, GroupWitness, Since, Transaction,
    patricia::{Namespace, Patricia},
    proof::Tree,
    record,
};

#[path = "../contracts/test_support.rs"]
mod contract;

const FUND: u64 = 100_000_000_000;
const FEE: u64 = 2_000;
const TIME: u64 = 10_000;

struct Fixture {
    spec: Spec,
    key: Privkey,
    lock: Script,
    spend: OutPoint,
    deps: Vec<CellDep>,
    outputs: std::cell::RefCell<std::collections::BTreeMap<OutPoint, Cell>>,
}

impl Fixture {
    fn new() -> Result<Self> {
        let lock_code = contract::native_lock(std::path::Path::new(env!("CARGO_MANIFEST_DIR")))?;
        let key = Privkey::from_slice(&[76; 32]);
        let pubkey = blake2b_256(key.pubkey()?.serialize());
        let lock = Script::new_builder()
            .code_hash(blake2b_256(&lock_code))
            .hash_type(ScriptHashType::Data)
            .args(Bytes::copy_from_slice(&pubkey[..20]))
            .build();
        let capacity = CapacityPolicy {
            bond_per_byte: 1_000_000,
            state_overhead: 80,
        };
        let mut development_cells = Vec::new();
        let mut deps = Vec::new();
        for (index, data) in [lock_code].into_iter().enumerate() {
            let point = OutPoint::new([9; 32].pack(), index as u32);
            let occupied = Cell::minimum_capacity(&lock, &[], data.len())?;
            let cell = Cell::new(occupied, lock.clone(), vec![], data)?;
            development_cells.push((point.clone(), cell));
            deps.push(CellDep::new_builder().out_point(point).build());
        }
        let spend = OutPoint::new([4; 32].pack(), 0);
        development_cells.push((
            spend.clone(),
            Cell::new(FUND, lock.clone(), vec![], Bytes::new())?,
        ));
        Ok(Self {
            spec: Spec {
                vm_crypto_v1: false,
                chain_id: "stem-native-storage-test".into(),
                development_validators: vec![],
                stake: None,
                bridge: None,
                lineage_type: None,
                keeper: None,
                network: [5; 32].pack(),
                genesis_time_ms: TIME,
                capacity,
                fees: Default::default(),
                limits: Limits {
                    transaction_count: 8,
                    transaction_bytes: 1024 * 1024,
                    transaction_cycles: 100_000_000,
                    block_cycles: 300_000_000,
                    cell_reads: 64,
                    cell_read_bytes: 8 * 1024 * 1024,
                    cell_write_bytes: 32 * 1024,
                    header_dep_age: 2,
                },
                development_cells,
            },
            key,
            lock,
            spend,
            deps,
            outputs: Default::default(),
        })
    }

    fn signed(
        &self,
        point: OutPoint,
        since: Option<Since>,
        output_value: u64,
        expires: Option<u64>,
    ) -> Result<Transaction> {
        let tx = Transaction::new(
            self.deps.clone(),
            vec![],
            vec![CellInput::new(point, since)],
            vec![Cell::new(
                output_value,
                self.lock.clone(),
                vec![],
                Bytes::new(),
            )?],
            vec![],
        )?;
        let tx = match expires {
            Some(height) => tx.with_valid_before_height(height)?,
            None => tx,
        };
        self.seal(tx)
    }

    fn seal(&self, tx: Transaction) -> Result<Transaction> {
        let signed = self.seal_with(tx, |point| {
            self.spec
                .development_cells
                .iter()
                .find(|(key, _)| key == point)
                .map(|(_, cell)| cell.clone())
                .or_else(|| self.outputs.borrow().get(point).cloned())
                .context("missing fixture signing Cell")
        })?;
        let id = signed.id(&self.spec.network);
        self.outputs.borrow_mut().extend(
            signed
                .outputs()
                .enumerate()
                .map(|(index, cell)| (OutPoint::new(id.clone(), index as u32), cell)),
        );
        Ok(signed)
    }

    fn seal_with(
        &self,
        tx: Transaction,
        cells: impl FnMut(&OutPoint) -> Result<Cell>,
    ) -> Result<Transaction> {
        let digest = tx.signing_digest(&self.spec.network, 0, &self.lock, cells, |_| {
            anyhow::bail!("unexpected Header Dep")
        })?;
        let signature = self.key.sign_recoverable(&digest.unpack())?;
        tx.with_witnesses(vec![GroupWitness {
            group_index: 0,
            proof_data: Bytes::new(),
            seal: Bytes::from(signature.serialize()),
        }])
    }
}

#[test]
fn wallet_finds_cells_by_complete_script_identity() -> Result<()> {
    use stem::native_store::{ScriptQuery, find_cells};

    let mut fixture = Fixture::new()?;
    let script = fixture
        .lock
        .clone()
        .as_builder()
        .args(Bytes::from_static(b"asset"))
        .build();
    let typed = OutPoint::new([24; 32].pack(), 0);
    fixture.spec.development_cells.push((
        typed.clone(),
        Cell::new(
            FUND,
            fixture.lock.clone(),
            vec![script.clone()],
            Bytes::new(),
        )?,
    ));
    let directory = tempfile::tempdir()?;
    let mut store = Store::open(directory.path(), &fixture.spec, Retention::Archive)?;
    let by_type = ScriptQuery::Type(script.calc_script_hash());
    let results = find_cells(directory.path(), &by_type, None, 10, Some(0))?;
    assert_eq!(
        results
            .cells
            .iter()
            .map(|(point, _)| point)
            .collect::<Vec<_>>(),
        [&typed]
    );
    let by_lock = ScriptQuery::Lock(fixture.lock.calc_script_hash());
    let first = find_cells(directory.path(), &by_lock, None, 1, Some(0))?;
    let second = find_cells(directory.path(), &by_lock, first.next, 1, Some(0))?;
    assert_ne!(first.cells[0].0, second.cells[0].0);
    let empty = store.prepare(&[], TIME + 1, false)?;
    store.commit(empty)?;
    assert!(find_cells(directory.path(), &by_lock, first.next, 1, Some(0)).is_err());
    Ok(())
}

#[test]
fn resource_fees_bound_cycles_in_parallel_and_cached_execution() -> Result<()> {
    use stem::native_store::ResourceFees;

    let mut fixture = Fixture::new()?;
    let shannon = ResourceFees::MICROSHANNONS_PER_SHANNON;
    fixture.spec.fees = ResourceFees {
        encoded_byte_microshannons: shannon,
        cycle_microshannons: shannon,
        cell_read_microshannons: shannon,
        cell_read_byte_microshannons: shannon,
        cell_write_byte_microshannons: shannon,
        cell_delete_microshannons: shannon,
    };
    let temp = tempfile::tempdir()?;
    let mut store = Store::open(temp.path(), &fixture.spec, Retention::Archive)?;
    let (initial, absence) = store.prove_extinguished_fees()?;
    assert_eq!(initial, None);
    assert!(Store::verify_extinguished_fees(
        store.network(),
        store.tip(),
        initial,
        &absence
    ));
    assert!(!Store::verify_extinguished_fees(
        store.network(),
        store.tip(),
        Some(0),
        &absence
    ));
    let low = fixture.signed(fixture.spend.clone(), None, FUND - 1_000_000, None)?;
    assert!(store.check(&low).is_err());
    let thread_pool = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    assert!(
        thread_pool
            .install(|| store.prepare(&[low], TIME + 1, true))
            .is_err()
    );

    let fee = 3_000_000;
    let enough = fixture.signed(fixture.spend.clone(), None, FUND - fee, None)?;
    let admitted = store.check(&enough)?;
    let serial = store.prepare(std::slice::from_ref(&enough), TIME + 1, false)?;
    let parallel = thread_pool.install(|| store.prepare(&[enough], TIME + 1, true))?;
    assert_eq!(serial.header(), parallel.header());
    assert_eq!(admitted.cycles, parallel.receipts()[0].cycles);
    store.commit(parallel)?;
    assert_eq!(store.fee_surplus()?, fee);
    let (amount, proof) = store.prove_extinguished_fees()?;
    let proof = stem_ledger::patricia::Proof::decode(&proof.encode()?)?;
    assert_eq!(amount, Some(fee));
    assert!(Store::verify_extinguished_fees(
        store.network(),
        store.tip(),
        amount,
        &proof
    ));
    assert!(!Store::verify_extinguished_fees(
        store.network(),
        store.tip(),
        None,
        &proof
    ));
    assert!(!Store::verify_extinguished_fees(
        store.network(),
        store.tip(),
        Some(fee + 1),
        &proof
    ));
    assert!(!Store::verify_extinguished_fees(
        &[6; 32].pack(),
        store.tip(),
        amount,
        &proof
    ));
    Ok(())
}

#[test]
#[ignore = "local RocksDB commit performance comparison"]
fn measure_commit_with_fixed_changes_and_varying_state_and_history() -> Result<()> {
    use std::time::Instant;

    for (live_count, first_height) in [(512u32, 0), (8192, 0), (8192, 512)] {
        let mut fixture = Fixture::new()?;
        let cell = Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?;
        for index in 0..live_count {
            fixture
                .spec
                .development_cells
                .push((OutPoint::new([82; 32].pack(), index), cell.clone()));
        }
        let temp = tempfile::tempdir()?;
        let mut store = Store::open(temp.path(), &fixture.spec, Retention::Archive)?;
        for height in 1..=first_height {
            let prepared = store.prepare(&[], TIME + height, false)?;
            store.commit(prepared)?;
        }
        let mut prepare_us = Vec::new();
        let mut sync_us = Vec::new();
        for sample in 0..25u32 {
            let txs = (sample * 8..(sample + 1) * 8)
                .map(|index| {
                    fixture.signed(
                        OutPoint::new([82; 32].pack(), index),
                        None,
                        FUND - FEE,
                        None,
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            let time = TIME + store.tip().context.height + 1;
            let before = Instant::now();
            let prepared = store.prepare(&txs, time, false)?;
            prepare_us.push(before.elapsed().as_micros());
            let before = Instant::now();
            store.commit(prepared)?;
            sync_us.push(before.elapsed().as_micros());
        }
        prepare_us.sort_unstable();
        sync_us.sort_unstable();
        println!(
            "state sample cells={} prior_height={first_height} changes_per_block=16 prepare_us_p50={} sync_us_p50={}",
            fixture.spec.development_cells.len(),
            prepare_us[12],
            sync_us[12]
        );
        assert_eq!(store.fee_surplus()?, 25 * 8 * FEE);
    }
    Ok(())
}

#[test]
fn verification_schedules_preserve_receipts_conflicts_and_block_cycles() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.spec.limits.block_cycles = 8 * fixture.spec.limits.transaction_cycles;
    let points = [10u8, 11, 12, 13, 14, 15, 16].map(|id| OutPoint::new([id; 32].pack(), 0));
    for point in &points {
        fixture.spec.development_cells.push((
            point.clone(),
            Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?,
        ));
    }
    let mut txs = points
        .iter()
        .map(|point| fixture.signed(point.clone(), None, FUND - FEE, None))
        .collect::<Result<Vec<_>>>()?;
    let intermediate = OutPoint::new(txs[0].id(&fixture.spec.network), 0);
    let successor = fixture.signed(intermediate.clone(), None, FUND - 3 * FEE, None)?;
    let output = OutPoint::new(successor.id(&fixture.spec.network), 0);
    txs.push(successor);
    let directory = tempfile::tempdir()?;
    let mut serial = Store::open(
        &directory.path().join("serial"),
        &fixture.spec,
        Retention::Archive,
    )?;
    let expected = serial.prepare(&txs, TIME, false)?;
    let workers = rayon::ThreadPoolBuilder::new().num_threads(2).build()?;
    let mut parallel = Store::open(
        &directory.path().join("parallel"),
        &fixture.spec,
        Retention::Archive,
    )?;
    let actual = workers.install(|| parallel.prepare(&txs, TIME, true))?;
    assert_eq!(expected.receipts(), actual.receipts());
    assert_eq!(expected.header(), actual.header());

    let mut duplicate = txs[..7].to_vec();
    duplicate.push(txs[0].clone());
    assert!(
        workers
            .install(|| parallel.prepare(&duplicate, TIME, true))
            .is_err()
    );
    let mut invalid = txs.clone();
    invalid[7] = txs[7].with_witnesses(vec![])?;
    assert!(
        workers
            .install(|| parallel.prepare(&invalid, TIME, true))
            .is_err()
    );
    assert!(parallel.live_cell(&points[0])?.is_some());

    let charged: u64 = actual.receipts().iter().map(|receipt| receipt.cycles).sum();
    serial.commit(expected)?;
    parallel.commit(actual)?;
    let (executed, receipts) = parallel.execution(1)?.context("missing block execution")?;
    assert!(
        executed
            .iter()
            .map(Transaction::as_slice)
            .eq(txs.iter().map(Transaction::as_slice))
    );
    assert_eq!(
        receipts,
        serial.execution(1)?.context("missing serial execution")?.1
    );
    for point in [&output, &intermediate, &points[0]] {
        let (serial_cell, serial_proof) = serial.prove_cell(point)?;
        let (parallel_cell, parallel_proof) = parallel.prove_cell(point)?;
        let encode = |cell: &stem_ledger::LiveCell| stem_ledger::record::encode_cell(point, cell);
        assert_eq!(
            serial_cell.as_ref().map(encode),
            parallel_cell.as_ref().map(encode)
        );
        assert_eq!(serial_proof.encode()?, parallel_proof.encode()?);
    }
    assert!(parallel.live_cell(&output)?.is_some());
    assert!(parallel.live_cell(&intermediate)?.is_none());

    fixture.spec.limits.transaction_cycles = charged / 2;
    fixture.spec.limits.block_cycles = charged - 1;
    let limited = Store::open(
        &directory.path().join("limited"),
        &fixture.spec,
        Retention::Archive,
    )?;
    assert!(limited.prepare(&txs[..2], TIME, false).is_ok());
    assert!(limited.prepare(&txs, TIME, false).is_err());
    assert!(
        workers
            .install(|| limited.prepare(&txs, TIME, true))
            .is_err()
    );
    Ok(())
}

#[cfg(feature = "native-commitment-pipeline")]
#[test]
fn parallel_commitments_match_serial_roots_and_reopened_proofs() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.spec.limits.transaction_count = 512;
    fixture.spec.limits.block_cycles = 4_000_000_000;
    fixture.spec.limits.cell_reads = 8192;
    fixture.spec.limits.cell_read_bytes = 128 * 1024 * 1024;
    fixture.spec.limits.cell_write_bytes = 4 * 1024 * 1024;
    let mut transactions = Vec::new();
    for index in 0..768 {
        let point = OutPoint::new([41; 32].pack(), index);
        fixture.spec.development_cells.push((
            point.clone(),
            Cell::new(FUND, fixture.lock.clone(), vec![], Bytes::new())?,
        ));
        transactions.push(fixture.signed(point, None, FUND - FEE, None)?);
    }

    let home = tempfile::tempdir()?;
    let open = |name| Store::open(&home.path().join(name), &fixture.spec, Retention::Archive);
    let mut serial = open("serial")?;
    let mut parallel = open("parallel")?;
    let workers = rayon::ThreadPoolBuilder::new().num_threads(3).build()?;
    let mut rest = transactions.as_slice();
    for (height, count) in (1..).zip([255, 256, 257]) {
        let (block, remaining) = rest.split_at(count);
        rest = remaining;
        let time = TIME + height;
        let reference = serial.prepare(block, time, false)?;
        let follower = workers.install(|| parallel.prepare(block, time, true))?;
        let (selected, leader) =
            workers.install(|| parallel.build_proposal(block, time, u64::MAX))?;
        assert!(
            selected
                .iter()
                .map(Transaction::as_slice)
                .eq(block.iter().map(Transaction::as_slice))
        );
        for candidate in [&leader, &follower] {
            assert_eq!(candidate.header().encode(), reference.header().encode());
            assert_eq!(candidate.receipts(), reference.receipts());
        }
        serial.commit(reference)?;
        parallel.commit(follower)?;
    }
    assert!(rest.is_empty());
    drop(parallel);
    let reopened = open("parallel")?;
    assert_eq!(serial.tip(), reopened.tip());
    assert_eq!(
        serial.prove_extinguished_fees()?,
        reopened.prove_extinguished_fees()?
    );
    assert_eq!(reopened.fee_surplus()?, 768 * FEE);
    let spent = OutPoint::new([41; 32].pack(), 767);
    let output = OutPoint::new(transactions[767].id(serial.network()), 0);
    for point in [spent, output] {
        let (_, expected) = serial.prove_cell(&point)?;
        let (_, actual) = reopened.prove_cell(&point)?;
        assert_eq!(expected.encode()?, actual.encode()?);
    }
    for height in 1..=3 {
        let (expected, receipts) = serial
            .execution(height)?
            .context("missing serial execution")?;
        let (actual, other) = reopened
            .execution(height)?
            .context("missing parallel execution")?;
        assert_eq!(receipts, other);
        assert!(
            expected
                .iter()
                .map(Transaction::as_slice)
                .eq(actual.iter().map(Transaction::as_slice))
        );
    }
    Ok(())
}

#[test]
fn sync_commit_reopen_and_prove_the_native_execution() -> Result<()> {
    let fixture = Fixture::new()?;
    let path = tempfile::tempdir()?;
    let archive_path = tempfile::tempdir()?;
    let mut recent = Store::open(path.path(), &fixture.spec, Retention::Recent(2))?;
    let mut archive = Store::open(archive_path.path(), &fixture.spec, Retention::Archive)?;
    let first = fixture.signed(fixture.spend.clone(), None, FUND - FEE, Some(2))?;
    let intermediate = OutPoint::new(first.id(recent.network()), 0);
    let last = fixture.signed(intermediate.clone(), None, FUND - 2 * FEE, None)?;
    let output = OutPoint::new(last.id(recent.network()), 0);
    let genesis = recent.tip().clone();
    assert!(
        recent
            .prepare(&[first.clone(), first.clone()], TIME, true)
            .is_err()
    );
    assert!(
        recent
            .prepare(&[first.with_witnesses(vec![])?], TIME, true)
            .is_err()
    );
    assert!(recent.live_cell(&fixture.spend)?.is_some());

    let block = vec![first, last];
    let candidates = [&block[0], &block[0], &block[1]];
    let (selected, pending) = recent.build_proposal(candidates, TIME + 3, u64::MAX)?;
    assert_eq!(selected.len(), block.len());
    assert!(
        selected
            .iter()
            .zip(&block)
            .all(|(a, b)| a.as_slice() == b.as_slice())
    );
    let parallel = recent.prepare(&block, TIME + 3, true)?;
    let serial = archive.prepare(&block, TIME + 3, false)?;
    assert_eq!(pending.receipts(), serial.receipts());
    assert_eq!(pending.header(), serial.header());
    assert_eq!(pending.receipts(), parallel.receipts());
    assert_eq!(pending.header(), parallel.header());
    assert_eq!(*recent.tip(), genesis);
    let stale = recent.prepare(&[], TIME + 3, false)?;
    recent.commit(pending)?;
    archive.commit(serial)?;
    assert!(recent.commit(stale).is_err());
    assert!(recent.live_cell(&fixture.spend)?.is_none());
    assert!(recent.live_cell(&intermediate)?.is_none());
    assert_eq!(recent.fee_surplus()?, 2 * FEE);
    let committed = recent.tip().clone();
    drop(recent);
    let mut recent = Store::open(path.path(), &fixture.spec, Retention::Recent(2))?;
    assert_eq!(*recent.tip(), committed);
    let points = [
        intermediate.clone(),
        output.clone(),
        fixture.spend.clone(),
        output.clone(),
    ];
    for (point, (cell, proof)) in points.iter().zip(recent.prove_cells(&points)?) {
        let (reference, expected) = recent.prove_cell(point)?;
        let encode = |cell: &stem_ledger::LiveCell| record::encode_cell(point, cell);
        assert_eq!(cell.as_ref().map(encode), reference.as_ref().map(encode));
        assert_eq!(proof.encode()?, expected.encode()?);
    }
    let (record, proof) = recent.prove_cell(&output)?;
    let record = record.expect("output survives restart");
    assert_eq!(
        record.origin.context_id,
        committed.context.id(recent.network())
    );
    let tree = Patricia::new(fixture.spec.network.clone(), Namespace::Cells);
    let root = committed.state.cell_root.unpack();
    assert!(record::verify_cell(
        &tree,
        &root,
        &output,
        Some(&record),
        &proof
    ));
    let mut forged = record.clone();
    forged.origin.timestamp_ms += 1;
    assert!(!record::verify_cell(
        &tree,
        &root,
        &output,
        Some(&forged),
        &proof
    ));
    let (transactions, receipts) = recent.execution(1)?.expect("recent execution");
    let tx_tree = Tree::transactions(fixture.spec.network.clone(), &transactions)?;
    let receipt_tree = Tree::receipts(fixture.spec.network.clone(), &receipts)?;
    assert!(tx_tree.prove(1)?.verify_execution(
        recent.network(),
        &committed.execution(),
        &transactions[1],
        &receipt_tree.prove(1)?,
        &receipts[1]
    ));
    assert!(!tx_tree.prove(1)?.verify_execution(
        recent.network(),
        &committed.execution(),
        &transactions[1],
        &receipt_tree.prove(0)?,
        &receipts[0]
    ));

    // The lock uses the parent time, even when the candidate timestamp would make it mature.
    let mature = fixture.signed(output, Some(Since::RelativeTime(20)), FUND - 3 * FEE, None)?;
    assert!(
        recent
            .prepare(std::slice::from_ref(&mature), TIME + 100, true)
            .is_err()
    );
    for store in [&mut recent, &mut archive] {
        let empty = store.prepare(&[], TIME + 100, true)?;
        assert_ne!(
            empty.header().app_hash(store.network()),
            committed.app_hash(store.network())
        );
        store.commit(empty)?;
        let block = store.prepare(std::slice::from_ref(&mature), TIME + 100, true)?;
        store.commit(block)?;
    }
    assert_eq!(recent.consensus_retain_height(), 2);
    assert_eq!(archive.consensus_retain_height(), 0);
    let snapshot = archive.export_state()?;
    let hash = archive.tip().app_hash(archive.network());
    let restore_path = tempfile::tempdir()?;
    let mut restore = Store::open(restore_path.path(), &fixture.spec, Retention::Recent(2))?;
    assert!(restore.live_cell(&fixture.spend)?.is_some());
    let mut wrong_frontier = snapshot.clone();
    wrong_frontier.frontier[9] ^= 1;
    assert!(restore.install_state(wrong_frontier, &hash).is_err());
    let mut wrong_record = snapshot.clone();
    wrong_record.records.pop();
    assert!(restore.install_state(wrong_record, &hash).is_err());
    assert_eq!(*restore.tip(), genesis);
    assert!(restore.live_cell(&fixture.spend)?.is_some());
    restore.install_state(snapshot, &hash)?;
    assert!(restore.live_cell(&fixture.spend)?.is_none());
    assert!(restore.prove_cell(&fixture.spend)?.0.is_none());
    assert_eq!(restore.tip(), archive.tip());
    let next = OutPoint::new(mature.id(archive.network()), 0);
    assert!(restore.prove_cell(&next)?.0.is_some());
    let restored_next = restore.prepare(&[], TIME + 101, true)?;
    let source_next = archive.prepare(&[], TIME + 101, true)?;
    assert_eq!(restored_next.header(), source_next.header());
    assert_eq!(recent.tip(), archive.tip());
    assert!(recent.execution(1)?.is_none());
    assert!(archive.execution(1)?.is_some());
    Ok(())
}

#[test]
fn rejects_expiry_same_block_relative_locks_and_unfunded_outputs() -> Result<()> {
    let fixture = Fixture::new()?;
    let path = tempfile::tempdir()?;
    let store = Store::open(path.path(), &fixture.spec, Retention::Archive)?;
    let spend = fixture.spend.clone();
    let expired = fixture.signed(spend.clone(), None, FUND - FEE, Some(1))?;
    assert!(store.prepare(&[expired], TIME, false).is_err());
    let undersized = fixture.signed(spend.clone(), None, 1, None)?;
    assert!(store.prepare(&[undersized], TIME, false).is_err());
    let inflated = fixture.signed(spend.clone(), None, FUND + 1, None)?;
    assert!(store.prepare(&[inflated], TIME, false).is_err());
    let first = fixture.signed(spend, None, FUND - FEE, None)?;
    let output = OutPoint::new(first.id(store.network()), 0);
    let relative = fixture.signed(output, Some(Since::RelativeBlocks(1)), FUND - 2 * FEE, None)?;
    assert!(store.prepare(&[first, relative], TIME, false).is_err());
    assert_eq!(store.tip().context.height, 0);
    Ok(())
}

#[path = "native_store/comet.rs"]
mod comet;

#[path = "native_store/jolteon.rs"]
mod jolteon;

#[path = "native_store/staking.rs"]
mod staking;

#[path = "native_store/bridge.rs"]
mod bridge;

#[path = "native_store/intent.rs"]
mod intent;

#[path = "native_store/keeper.rs"]
mod keeper;
#[path = "native_store/keeper_load.rs"]
mod keeper_load;
#[path = "native_store/lineage.rs"]
mod lineage;
#[path = "native_store/lineage_load.rs"]
mod lineage_load;
#[path = "native_store/lineage_selection.rs"]
mod lineage_selection;
#[path = "native_store/storage_load.rs"]
mod storage_load;

#[path = "native_store/polytype_attachment.rs"]
mod polytype_attachment;

#[path = "native_store/lineage_batch.rs"]
mod lineage_batch;
#[path = "native_store/sponsored_lineage.rs"]
mod sponsored_lineage;

#[test]
fn candidate_branch_executes_outputs_without_mutating_the_finalized_store() -> Result<()> {
    let fixture = Fixture::new()?;
    let dir = tempfile::tempdir()?;
    let ref_dir = tempfile::tempdir()?;
    let mut store = Store::open(dir.path(), &fixture.spec, Retention::Archive)?;
    let mut reference = Store::open(ref_dir.path(), &fixture.spec, Retention::Archive)?;
    assert!(store.live_cell(&fixture.spend)?.is_some());
    let parent = fixture.signed(fixture.spend.clone(), None, FUND - FEE, None)?;
    let parent_output = OutPoint::new(parent.id(store.network()), 0);
    let child = fixture.signed(
        parent_output.clone(),
        Some(Since::RelativeBlocks(1)),
        FUND - 2 * FEE,
        None,
    )?;
    let fork = fixture.signed(fixture.spend.clone(), None, FUND - 3 * FEE, None)?;
    let parent_batch =
        std::sync::Arc::new(store.prepare(std::slice::from_ref(&parent), TIME + 1, false)?);
    let fork_batch = std::sync::Arc::new(store.prepare(&[fork], TIME + 1, false)?);
    let later_parent =
        std::sync::Arc::new(store.prepare(std::slice::from_ref(&parent), TIME + 2, false)?);
    for (candidate, expected_time) in [(&parent_batch, TIME + 1), (&later_parent, TIME + 2)] {
        store.on_branch(std::slice::from_ref(candidate), |state| {
            assert!(state.live_cell(&fixture.spend)?.is_none());
            let (cell, _) = state.prove_cell(&parent_output)?;
            assert_eq!(
                cell.context("branch output missing")?.origin.timestamp_ms,
                expected_time
            );
            Ok(())
        })?;
    }

    assert!(
        store
            .prepare(std::slice::from_ref(&child), TIME + 2, false)
            .is_err()
    );
    let next = store.on_branch(std::slice::from_ref(&parent_batch), |state| {
        state.prepare(std::slice::from_ref(&child), TIME + 2, false)
    })?;
    assert!(
        store
            .on_branch(&[fork_batch], |state| state.prepare(
                std::slice::from_ref(&child),
                TIME + 2,
                false
            ))
            .is_err()
    );
    assert!(store.live_cell(&parent_output)?.is_none());
    assert!(store.live_cell(&fixture.spend)?.is_some());
    assert_eq!(store.tip().context.height, 0);

    let finalized = reference.prepare(&[parent], TIME + 1, false)?;
    reference.commit(finalized)?;
    let expected = reference.prepare(&[child], TIME + 2, false)?;
    assert_eq!(next.header(), expected.header());
    assert_eq!(next.receipts(), expected.receipts());
    let next = std::sync::Arc::new(next);
    store.on_branch(&[parent_batch.clone(), next.clone()], |state| {
        assert!(state.live_cell(&parent_output)?.is_none());
        assert!(state.live_cell(&fixture.spend)?.is_none());
        Ok(())
    })?;

    let parent_batch = std::sync::Arc::try_unwrap(parent_batch)
        .ok()
        .context("shared test batch")?;
    store.commit(parent_batch)?;
    assert!(store.live_cell(&fixture.spend)?.is_none());
    assert_eq!(
        store
            .live_cell(&parent_output)?
            .context("finalized output missing")?
            .origin
            .timestamp_ms,
        TIME + 1
    );
    let next = std::sync::Arc::try_unwrap(next)
        .ok()
        .context("shared test batch")?;
    store.commit(next)?;
    assert!(store.live_cell(&parent_output)?.is_none());
    assert!(store.prove_cell(&parent_output)?.0.is_none());

    Ok(())
}

#[test]
fn vm_crypto_activation_changes_genesis_and_reopen_checks_it() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let old_path = tempfile::tempdir()?;
    let new_path = tempfile::tempdir()?;
    let open = |path: &std::path::Path, spec: &Spec| Store::open(path, spec, Retention::Archive);
    let old = open(old_path.path(), &fixture.spec)?.genesis_id().clone();
    fixture.spec.vm_crypto_v1 = true;
    let new = open(new_path.path(), &fixture.spec)?.genesis_id().clone();
    assert_ne!(old, new);
    assert!(open(old_path.path(), &fixture.spec).is_err());
    assert_eq!(open(new_path.path(), &fixture.spec)?.genesis_id(), &new);
    fixture.spec.vm_crypto_v1 = false;
    assert!(open(new_path.path(), &fixture.spec).is_err());
    assert_eq!(open(old_path.path(), &fixture.spec)?.genesis_id(), &old);
    Ok(())
}
