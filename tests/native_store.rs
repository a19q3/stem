use anyhow::Result;
use ckb_crypto::secp::Privkey;
use ckb_hash::blake2b_256;
use ckb_types::{
    bytes::Bytes,
    core::ScriptHashType,
    packed::{CellDep, OutPoint, Script},
    prelude::*,
};
use fern::native_store::{Limits, Retention, Spec, Store};
use fern_ledger::{
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
                chain_id: "fern-native-storage-test".into(),
                development_validators: vec![],
                stake: None,
                network: [5; 32].pack(),
                genesis_time_ms: TIME,
                capacity,
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
        let digest = tx.lock_digest(&self.spec.network, &self.lock);
        let signature = self.key.sign_recoverable(&digest.unpack())?;
        tx.with_witnesses(vec![GroupWitness {
            group_index: 0,
            proof_data: Bytes::new(),
            seal: Bytes::from(signature.serialize()),
        }])
    }
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
    let pending = recent.prepare(&block, TIME + 3, true)?;
    let serial = archive.prepare(&block, TIME + 3, false)?;
    assert_eq!(pending.receipts(), serial.receipts());
    assert_eq!(pending.header(), serial.header());
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
    let (record, proof) = recent.prove_cell(&output)?;
    let record = record.expect("output survives restart");
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
    let snapshot = archive.export_state()?;
    let hash = archive.tip().app_hash(archive.network());
    let restore_path = tempfile::tempdir()?;
    let mut restore = Store::open(restore_path.path(), &fixture.spec, Retention::Recent(2))?;
    let mut wrong_frontier = snapshot.clone();
    wrong_frontier.frontier[9] ^= 1;
    assert!(restore.install_state(wrong_frontier, &hash).is_err());
    let mut wrong_record = snapshot.clone();
    wrong_record.records.pop();
    assert!(restore.install_state(wrong_record, &hash).is_err());
    assert_eq!(*restore.tip(), genesis);
    restore.install_state(snapshot, &hash)?;
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

#[path = "native_store/staking.rs"]
mod staking;
