use ckb_crypto::secp::Privkey;
use ckb_jsonrpc_types::JsonBytes;
use ckb_types::{
    bytes::Bytes,
    core::{
        TransactionView,
        cell::{CellProvider, CellStatus},
    },
    packed::{CellDep, CellInput, OutPoint, Transaction},
    prelude::*,
};

use crate::{
    dev::{Fixture, fixture, sign_fixture_lock_group},
    engine::Engine,
    genesis::{Genesis, Validator, script_consensus},
    store::Store,
};

pub(crate) fn payments(count: u32) -> Fixture {
    fixture(
        Genesis {
            chain_id: "fern-test".to_owned(),
            timestamp_ms: 1_100_000,
            epoch_length: 100,
            checkpoint_interval: 0,
            checkpoint_cell_proofs: false,
            checkpoint_compact_transactions: false,
            checkpoint_live_cell_replay: false,
            validators: vec![Validator {
                ed25519: JsonBytes::from_vec(vec![8; 32]),
                power: 1,
            }],
            allocations: Vec::new(),
            bridge: None,
            authenticate_withdrawals: false,
            governance: None,
            guardian_governance: None,
        },
        count,
        crate::dev::Workload::Payments,
        None,
        false,
    )
    .expect("test genesis and valid CKB signatures")
}

fn decode(entry: &JsonBytes) -> TransactionView {
    Transaction::from_slice(entry.as_bytes())
        .unwrap()
        .into_view()
}

fn raw(tx: &TransactionView) -> Bytes {
    tx.data().as_bytes()
}

#[test]
fn vm_rejects_wrong_lock_signature_and_checks_full_witness_hash_cache_key() {
    let fixture = payments(1);
    let path = tempfile::tempdir().unwrap();
    let store = Store::open(path.path(), &fixture.genesis).unwrap();
    let engine = Engine::new(script_consensus().unwrap(), fixture.genesis.epoch_length, 2).unwrap();
    let payment = decode(&fixture.transactions[0]);
    let cycles = engine.check_tx(&store, raw(&payment)).unwrap();
    assert!(cycles > 1_000_000);
    assert_eq!(engine.check_tx(&store, raw(&payment)).unwrap(), cycles);
    assert_eq!(engine.script_metrics(), (1, 1));
    let tampered = sign_fixture_lock_group(&payment, &Privkey::from_slice(&[3; 32])).unwrap();
    assert_eq!(tampered.hash(), payment.hash());
    assert_ne!(tampered.witness_hash(), payment.witness_hash());
    assert!(engine.check_tx(&store, raw(&tampered)).is_err());
    assert_eq!(engine.script_metrics(), (2, 1));
}

#[test]
fn proposer_filters_conflicts_and_cold_replay_recovers_live_cells() {
    let fixture = payments(2);
    let path = tempfile::tempdir().unwrap();
    let mut store = Store::open(path.path(), &fixture.genesis).unwrap();
    let consensus = script_consensus().unwrap();
    let proposer = Engine::new(consensus.clone(), fixture.genesis.epoch_length, 4).unwrap();
    let serial = Engine::new(consensus, fixture.genesis.epoch_length, 1).unwrap();
    let parent = decode(&fixture.transactions[0]);
    let independent = decode(&fixture.transactions[1]);
    let parent_out = OutPoint::new(parent.hash(), 0);
    let input_used_by_parent = parent.input_pts_iter().next().unwrap();
    let child = sign_fixture_lock_group(
        &parent
            .as_advanced_builder()
            .set_inputs(vec![CellInput::new(parent_out.clone(), 0)])
            .build(),
        &Privkey::from_slice(&[2; 32]),
    )
    .unwrap();
    let conflict = sign_fixture_lock_group(
        &parent
            .as_advanced_builder()
            .set_outputs(vec![
                parent
                    .output(0)
                    .unwrap()
                    .as_builder()
                    .capacity(9_000_000_000u64)
                    .build(),
            ])
            .build(),
        &Privkey::from_slice(&[1; 32]),
    )
    .unwrap();
    let time = fixture.genesis.timestamp_ms + 200;
    let (valid, block) = proposer
        .prepare(
            &store,
            1,
            time,
            vec![raw(&parent), raw(&conflict), raw(&child), raw(&independent)],
            usize::MAX,
        )
        .unwrap();
    assert_eq!(valid, vec![raw(&parent), raw(&child), raw(&independent)]);
    let replayed = serial.verify(&store, 1, time, valid).unwrap();
    assert_eq!(block.header, replayed.header);
    assert_eq!(block.transactions, replayed.transactions);
    assert_eq!(block.cycles, replayed.cycles);
    assert_eq!(block.cells, replayed.cells);
    assert!(
        serial
            .verify(&store, 1, time, vec![raw(&child), raw(&parent)])
            .is_err()
    );
    assert!(
        serial
            .verify(&store, 1, time, vec![raw(&parent), raw(&conflict)])
            .is_err()
    );
    store
        .commit(block.header, &block.transactions, block.cells)
        .unwrap();
    let expected = store.app_hash.clone();
    let empty = proposer.verify(&store, 2, time + 100, vec![]).unwrap();
    store.commit(empty.header, &[], empty.cells).unwrap();
    drop(store);

    let restored = Store::open(path.path(), &fixture.genesis).unwrap();
    assert_eq!(restored.tip.number(), 2);
    assert_eq!(restored.app_hash, expected);
    assert!(restored.cell(&input_used_by_parent, true).is_unknown());
    assert!(restored.cell(&parent_out, true).is_unknown());
    assert!(restored.transaction(&parent.hash()).is_some());
    let CellStatus::Live(cell) = restored.cell(&OutPoint::new(child.hash(), 0), true) else {
        panic!("child output should be live after reopening");
    };
    let info = cell.transaction_info.unwrap();
    assert_eq!(info.block_hash, expected);
    assert_eq!(info.block_number, 1);
    assert_eq!(info.index, 2);
    assert!(serial.check_tx(&restored, raw(&parent)).is_err());
}

#[test]
fn invalid_parent_removes_its_descendant_from_prepared_block() {
    let fixture = payments(2);
    let path = tempfile::tempdir().unwrap();
    let store = Store::open(path.path(), &fixture.genesis).unwrap();
    let engine = Engine::new(script_consensus().unwrap(), fixture.genesis.epoch_length, 2).unwrap();
    let valid_parent = decode(&fixture.transactions[0]);
    let invalid_parent =
        sign_fixture_lock_group(&valid_parent, &Privkey::from_slice(&[3; 32])).unwrap();
    let child = sign_fixture_lock_group(
        &valid_parent
            .as_advanced_builder()
            .set_inputs(vec![CellInput::new(
                OutPoint::new(valid_parent.hash(), 0),
                0,
            )])
            .build(),
        &Privkey::from_slice(&[2; 32]),
    )
    .unwrap();
    let independent = decode(&fixture.transactions[1]);
    let (valid, block) = engine
        .prepare(
            &store,
            1,
            fixture.genesis.timestamp_ms,
            vec![raw(&invalid_parent), raw(&child), raw(&independent)],
            usize::MAX,
        )
        .unwrap();
    assert_eq!(valid, vec![raw(&independent)]);
    assert!(!block.cells.contains_key(&OutPoint::new(child.hash(), 0)));
}

#[test]
fn dependency_reads_follow_canonical_cell_creation_and_consumption_order() {
    let fixture = payments(2);
    let path = tempfile::tempdir().unwrap();
    let store = Store::open(path.path(), &fixture.genesis).unwrap();
    let consensus = script_consensus().unwrap();
    let parallel = Engine::new(consensus.clone(), fixture.genesis.epoch_length, 4).unwrap();
    let serial = Engine::new(consensus, fixture.genesis.epoch_length, 1).unwrap();
    let change = decode(&fixture.transactions[0]);
    let reader = decode(&fixture.transactions[1]);
    let spent = change.input_pts_iter().next().unwrap();
    let created = OutPoint::new(change.hash(), 0);
    let time = fixture.genesis.timestamp_ms + 200;

    for (point, created_in_block) in [(spent, false), (created, true)] {
        let reader = sign_fixture_lock_group(
            &reader
                .as_advanced_builder()
                .cell_dep(CellDep::new_builder().out_point(point).build())
                .build(),
            &Privkey::from_slice(&[1; 32]),
        )
        .unwrap();
        let mut order = vec![raw(&reader), raw(&change)];
        if created_in_block {
            order.reverse();
        }
        let concurrent = parallel.verify(&store, 1, time, order.clone()).unwrap();
        let replayed = serial.verify(&store, 1, time, order.clone()).unwrap();
        assert_eq!(concurrent.header, replayed.header);
        assert_eq!(concurrent.cells, replayed.cells);
        assert_eq!(concurrent.cycles, replayed.cycles);
        order.reverse();
        assert!(parallel.verify(&store, 1, time, order.clone()).is_err());
        let (prepared, _) = parallel
            .prepare(&store, 1, time, order, usize::MAX)
            .unwrap();
        assert_eq!(prepared, vec![raw(&change)]);
    }
}

#[test]
fn verifies_since_against_committed_height_and_headers() {
    let fixture = payments(1);
    let path = tempfile::tempdir().unwrap();
    let mut store = Store::open(path.path(), &fixture.genesis).unwrap();
    let engine = Engine::new(script_consensus().unwrap(), fixture.genesis.epoch_length, 2).unwrap();
    let payment = decode(&fixture.transactions[0]);
    let payment = sign_fixture_lock_group(
        &payment
            .as_advanced_builder()
            .set_inputs(vec![CellInput::new(
                payment.input_pts_iter().next().unwrap(),
                2,
            )])
            .header_dep(store.genesis_hash.clone())
            .build(),
        &Privkey::from_slice(&[1; 32]),
    )
    .unwrap();
    let now = fixture.genesis.timestamp_ms;
    assert!(engine.check_tx(&store, raw(&payment)).is_err());
    let empty = engine.verify(&store, 1, now + 1, vec![]).unwrap();
    store.commit(empty.header, &[], empty.cells).unwrap();
    assert!(engine.check_tx(&store, raw(&payment)).is_ok());
    let invalid = payment.as_advanced_builder().header_dep([9; 32]).build();
    assert!(engine.check_tx(&store, raw(&invalid)).is_err());
}
