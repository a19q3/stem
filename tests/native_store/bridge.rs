use std::path::Path;

use anyhow::{Context, ensure};
use ckb_types::packed::Byte32;
use ed25519_dalek::{Signer, SigningKey};
use stem::native_store::{
    BridgeGenesis, FeeFundDestination, FeeFundPolicy, LaneGenesis, RecipientLock, lane_id,
};
use stem_ledger::protocol::{
    BridgeMint, BridgeSettlement, Certificate, FeeClaim, FeeFund, FeePayouts, L1Anchor,
    Limits as ProtocolLimits, MintObservation, Operation, SettlementAction, VaultUpdate,
    settlement_digest,
};
use stem_stake::{Attestation, Member};

use super::*;

const MINT: u64 = 40_000_000_000;

fn hash(value: u8) -> Byte32 {
    [value; 32].pack()
}

fn out(value: u8, index: u32) -> OutPoint {
    OutPoint::new(hash(value), index)
}

fn anchor(height: u64) -> L1Anchor {
    L1Anchor {
        height,
        hash: hash(height as u8),
    }
}

fn commit(store: &mut Store, tx: Transaction) -> Result<()> {
    let block = store.prepare(&[tx], TIME, true)?;
    store.commit(block)
}

#[test]
fn bridge_mint_burn_and_payout_commit_backing_and_replay_state() -> Result<()> {
    let mut fixture = Fixture::new()?;
    let bridge_code =
        contract::native_contract(Path::new(env!("CARGO_MANIFEST_DIR")), "bridge-udt")?;
    let native_code = fixture.spec.development_cells.remove(0).1;
    let genesis = hash(25);
    let burn_code = hash(31);
    let vault_type = |id: u8| {
        let args = [
            hash(id),
            hash(57),
            hash(58),
            genesis.clone(),
            fixture.spec.network.clone(),
        ];
        Script::new_builder()
            .code_hash(hash(59))
            .hash_type(ScriptHashType::Data2)
            .args(
                Bytes::from(
                    args.iter()
                        .flat_map(|value| value.as_slice().iter().copied())
                        .collect::<Vec<_>>(),
                )
                .pack(),
            )
            .build()
    };
    let registry = vault_type(33);
    let custody = vault_type(34);
    let l1_token = Script::new_builder()
        .code_hash(hash(35))
        .hash_type(ScriptHashType::Data1)
        .args(hash(36).as_bytes())
        .build();
    let asset_id = l1_token.calc_script_hash();
    let wrapped_code = Byte32::from_slice(&blake2b_256(&bridge_code))?;
    let wrapped = Script::new_builder()
        .code_hash(wrapped_code.clone())
        .hash_type(ScriptHashType::Data)
        .args(asset_id.as_bytes())
        .build();
    let ckb_lane = lane_id(&genesis, &registry.calc_script_hash(), None);
    let asset_lane = lane_id(&genesis, &custody.calc_script_hash(), Some(&asset_id));
    let recipient = fixture.lock.clone();
    let reserved = recipient
        .clone()
        .as_builder()
        .args(Bytes::from(vec![30; 20]).pack())
        .build();
    let guardian = SigningKey::from_bytes(&[61; 32]);
    let authority = hash(39);
    let protocol = ProtocolLimits {
        operations: 4,
        bytes: 512 * 1024,
        signatures: 4,
    };
    fixture.spec.development_cells.clear();
    fixture.spec.limits.cell_write_bytes = 512 * 1024;
    fixture.spec.bridge = Some(BridgeGenesis {
        l1_genesis: genesis.clone(),
        confirmations: 1,
        anchor: anchor(1),
        guardian_generation: authority.clone(),
        guardians: vec![Member {
            key: guardian.verifying_key().to_bytes(),
            power: 1,
        }],
        wrapped_code_hash: wrapped_code,
        burn_code_hash: burn_code.clone(),
        lanes: vec![
            LaneGenesis {
                vault_type: registry,
                vault: out(40, 0),
                l1_type: None,
            },
            LaneGenesis {
                vault_type: custody,
                vault: out(41, 0),
                l1_type: Some(l1_token),
            },
        ],
        recipient_locks: vec![RecipientLock {
            code_hash: recipient.code_hash(),
            hash_type: ScriptHashType::Data,
            args_len: 20,
        }],
        reserved_recipient_locks: vec![reserved.clone()],
        mint_limits: protocol,
        settlement_limits: protocol,
        deposits_per_block: 4,
        fee_funds: Some(std::array::from_fn(|index| FeeFundDestination {
            basis_points: [6500, 1500, 1000, 1000][index],
            recipient: recipient
                .clone()
                .as_builder()
                .args(Bytes::from(vec![70 + index as u8; 20]).pack())
                .build(),
        })),
    });
    let original = fixture.spec.bridge.as_ref().unwrap().lanes[1]
        .vault_type
        .clone();
    let invalid = tempfile::tempdir()?;
    for offset in [32, 64] {
        let mut args = original.args().raw_data().to_vec();
        args[offset] ^= 1;
        fixture.spec.bridge.as_mut().unwrap().lanes[1].vault_type = original
            .clone()
            .as_builder()
            .args(Bytes::from(args).pack())
            .build();
        assert!(matches!(
            Store::open(invalid.path(), &fixture.spec, Retention::Archive),
            Err(error) if error.to_string().contains("same CKB checkpoint")
        ));
    }
    fixture.spec.bridge.as_mut().unwrap().lanes[1].vault_type = original;
    let store_path = tempfile::tempdir()?;
    let mut store = Store::open(store_path.path(), &fixture.spec, Retention::Recent(2))?;
    let make_mint = |deposit,
                     previous,
                     confirmed,
                     lane,
                     vault_input,
                     vault_output,
                     units,
                     deps: Vec<CellDep>,
                     outputs: Vec<Cell>|
     -> Result<Transaction> {
        let statement = BridgeMint {
            l1_genesis: genesis.clone(),
            deposits: vec![deposit],
        };
        let observation = MintObservation {
            authority: authority.clone(),
            previous_anchor: anchor(previous),
            anchor: anchor(confirmed),
            vault: VaultUpdate {
                lane,
                input: vault_input,
                output: vault_output,
            },
            capacity: outputs.iter().map(Cell::capacity).sum(),
            units,
        };
        let digest = Transaction::bridge_mint_signing_digest(
            &fixture.spec.network,
            &statement,
            &observation,
            deps.clone(),
            outputs.clone(),
        )?;
        let certificate = Certificate::new(
            observation.encode(),
            vec![Attestation {
                guardian: 0,
                signature: guardian.sign(digest.as_slice()).to_bytes(),
            }],
        )?;
        Transaction::bridge_mint(statement, certificate, deps, outputs)
    };
    let bridge_code = Cell::new(
        bridge_code.len() as u64 * 2_000_000,
        fixture.lock.clone(),
        vec![],
        bridge_code,
    )?;
    let code_mint = make_mint(
        out(50, 0),
        1,
        2,
        ckb_lane.clone(),
        out(40, 0),
        out(42, 0),
        0,
        vec![],
        vec![native_code, bridge_code],
    )?;
    let lock_dep = CellDep::new_builder()
        .out_point(OutPoint::new(code_mint.id(store.network()), 0))
        .build();
    let udt_dep = CellDep::new_builder()
        .out_point(OutPoint::new(code_mint.id(store.network()), 1))
        .build();
    let code_capacity: u64 = code_mint.outputs().map(|cell| cell.capacity()).sum();
    commit(&mut store, code_mint)?;
    let mint = make_mint(
        out(51, 0),
        2,
        3,
        asset_lane.clone(),
        out(41, 0),
        out(43, 0),
        100,
        vec![udt_dep.clone()],
        vec![Cell::new(
            MINT,
            fixture.lock.clone(),
            vec![wrapped.clone()],
            Bytes::copy_from_slice(&100u128.to_le_bytes()),
        )?],
    )?;
    let funded = OutPoint::new(mint.id(store.network()), 0);
    let prepared = store.prepare(std::slice::from_ref(&mint), TIME, true)?;
    assert!(prepared.receipts()[0].cycles > 0);
    store.commit(prepared)?;
    let (backing, proof) = store.prove_bridge_backing()?;
    assert_eq!(backing.liquid(), code_capacity + MINT);
    assert_eq!(backing.circulating(), backing.liquid());
    assert_eq!(backing.pending(), 0);
    assert!(Store::verify_bridge_backing(
        store.network(),
        store.tip(),
        &backing,
        &proof,
    ));
    assert!(!Store::verify_bridge_backing(
        store.network(),
        store.tip(),
        &backing.credit_mint(1)?,
        &proof,
    ));
    let mint_again = make_mint(
        out(51, 0),
        3,
        4,
        asset_lane.clone(),
        out(43, 0),
        out(44, 0),
        100,
        vec![udt_dep.clone()],
        vec![Cell::new(
            MINT,
            fixture.lock.clone(),
            vec![wrapped.clone()],
            Bytes::copy_from_slice(&100u128.to_le_bytes()),
        )?],
    )?;
    assert!(
        format!("{:#}", store.check(&mint_again).unwrap_err()).contains("deposit already credited")
    );

    let make_payment = |lock, units| -> Result<Transaction> {
        let tx = Transaction::new(
            vec![lock_dep.clone(), udt_dep.clone()],
            vec![],
            vec![CellInput::new(funded.clone(), None)],
            vec![Cell::new(
                MINT - FEE,
                lock,
                vec![wrapped.clone()],
                Bytes::copy_from_slice(&u128::to_le_bytes(units)),
            )?],
            vec![],
        )?;
        fixture.seal_with(tx, |point| {
            Ok(store
                .live_cell(point)?
                .context("missing Cell for bridge signature")?
                .cell)
        })
    };
    let burn = |recipient: &Script| {
        let mut args = asset_lane.as_slice().to_vec();
        args.extend_from_slice(recipient.as_slice());
        Script::new_builder()
            .code_hash(burn_code.clone())
            .hash_type(ScriptHashType::Data)
            .args(Bytes::from(args))
            .build()
    };
    assert!(
        store
            .check(&make_payment(fixture.lock.clone(), 101)?)
            .is_err()
    );
    assert!(
        format!(
            "{:#}",
            store
                .check(&make_payment(burn(&reserved), 100)?)
                .unwrap_err()
        )
        .contains("reserved L1 recipient")
    );
    let payment = make_payment(burn(&recipient), 100)?;
    let omitted = OutPoint::new(payment.id(store.network()), 0);
    commit(&mut store, payment)?;
    assert!(store.live_cell(&omitted)?.is_none());
    assert!(store.live_cell(&funded)?.is_none());
    assert_eq!(store.fee_surplus()?, FEE);
    let (backing, _) = store.prove_bridge_backing()?;
    assert_eq!(backing.liquid(), code_capacity + MINT);
    assert_eq!(backing.circulating(), code_capacity);
    assert_eq!(backing.pending(), MINT - FEE);
    assert_eq!(backing.unallocated(), FEE);
    let pending = store
        .bridge_receipt(&asset_lane, 0)?
        .context("pending nonce zero")?;
    let burn_tip = store.tip().clone();
    let (pending_record, pending_path) = store.prove_bridge_withdrawal(&asset_lane, 0)?;
    assert_eq!(pending_record.as_ref(), Some(&pending));
    let encoded_path = pending_path.encode()?;
    let pending_path = stem_ledger::patricia::Proof::decode(&encoded_path)?;
    assert!(Store::verify_bridge_withdrawal(
        store.network(),
        &burn_tip,
        &asset_lane,
        0,
        &pending,
        &pending_path,
    ));
    assert!(!Store::verify_bridge_withdrawal(
        store.network(),
        &burn_tip,
        &ckb_lane,
        0,
        &pending,
        &pending_path,
    ));
    assert!(!Store::verify_bridge_withdrawal(
        store.network(),
        &burn_tip,
        &asset_lane,
        1,
        &pending,
        &pending_path,
    ));
    let quote = store.bridge_lane(&asset_lane)?;
    assert_eq!(
        (quote.capacity, quote.reserved_capacity),
        (MINT, MINT - FEE)
    );
    assert_eq!((quote.units, quote.reserved_units), (100, 100));
    let (lane_record, proof) = store.prove_bridge_lane(&asset_lane)?;
    assert_eq!(lane_record, (quote, 1));
    assert!(Store::verify_bridge_lane(
        store.network(),
        store.tip(),
        &asset_lane,
        &lane_record,
        &proof,
    ));
    assert!(!Store::verify_bridge_lane(
        store.network(),
        store.tip(),
        &ckb_lane,
        &lane_record,
        &proof,
    ));
    let (lane, _) = lane_record;
    assert!(stem_ledger::protocol::Lane::decode(&lane.encode(0)).is_err());
    let paid = BridgeSettlement {
        l1_genesis: genesis,
        authority,
        previous_anchor: anchor(3),
        anchor: anchor(4),
        action: SettlementAction::WithdrawalPaid {
            vault: VaultUpdate {
                lane: asset_lane.clone(),
                input: out(43, 0),
                output: out(44, 0),
            },
            nonce: 0,
            receipt: Byte32::from_slice(&pending[..32])?,
        },
    };
    let message = settlement_digest(store.network(), &paid);
    let payouts = Certificate::new(
        Bytes::new(),
        vec![Attestation {
            guardian: 0,
            signature: guardian.sign(message.as_slice()).to_bytes(),
        }],
    )?;
    let settlement = Transaction::control(Operation::BridgeSettlement(paid), payouts)?;
    commit(&mut store, settlement.clone())?;
    assert!(store.check(&settlement).is_err());
    let quote = store.bridge_lane(&asset_lane)?;
    assert_eq!((quote.capacity, quote.reserved_capacity), (FEE, 0));
    assert_eq!(
        (quote.units, quote.reserved_units, quote.next_nonce),
        (0, 0, 1)
    );
    assert!(store.bridge_receipt(&asset_lane, 0)?.is_none());
    assert!(!Store::verify_bridge_withdrawal(
        store.network(),
        store.tip(),
        &asset_lane,
        0,
        &pending,
        &pending_path,
    ));
    let (backing, _) = store.prove_bridge_backing()?;
    assert_eq!(backing.liquid(), code_capacity + FEE);
    assert_eq!(backing.circulating(), code_capacity);
    assert_eq!(backing.pending(), 0);
    assert_eq!(backing.unallocated(), FEE);
    let snapshot = store.export_state()?;
    let trusted = store.tip().app_hash(store.network());
    let restore_path = tempfile::tempdir()?;
    let mut restored = Store::open(restore_path.path(), &fixture.spec, Retention::Recent(2))?;
    restored.install_state(snapshot, &trusted)?;
    assert_eq!(restored.bridge_lane(&asset_lane)?, quote);
    let (record, proof) = restored.prove_bridge_lane(&asset_lane)?;
    assert_eq!(record, (quote, 1));
    assert!(Store::verify_bridge_lane(
        restored.network(),
        restored.tip(),
        &asset_lane,
        &record,
        &proof,
    ));
    assert!(restored.bridge_receipt(&asset_lane, 0)?.is_none());
    assert_eq!(restored.fee_surplus()?, FEE);
    let policy = restored
        .fee_fund_policy()
        .context("missing restored funds")?;
    let policy_proof = restored.prove_fee_fund_policy()?;
    assert_eq!(&FeeFundPolicy::decode(&policy.encode())?, policy);
    assert!(Store::verify_fee_fund_policy(
        restored.network(),
        restored.tip(),
        policy,
        &policy_proof
    ));
    let mut changed = policy.clone();
    changed.funds.swap(0, 1);
    assert!(!Store::verify_fee_fund_policy(
        restored.network(),
        restored.tip(),
        &changed,
        &policy_proof
    ));
    let (funds, proof) = restored.prove_fee_payouts()?;
    assert_eq!(funds, FeePayouts::default());
    assert!(Store::verify_fee_payouts(
        restored.network(),
        restored.tip(),
        &funds,
        &proof
    ));
    let mut forged = funds;
    forged.queue(
        FeeClaim {
            fund: FeeFund::Validators,
            amount: 1,
            claimed_before: 0,
        },
        1,
    )?;
    assert!(!Store::verify_fee_payouts(
        restored.network(),
        restored.tip(),
        &forged,
        &proof
    ));
    let (restored_backing, proof) = restored.prove_bridge_backing()?;
    assert_eq!(restored_backing, backing);
    assert!(Store::verify_bridge_backing(
        restored.network(),
        restored.tip(),
        &backing,
        &proof,
    ));
    ensure!(restored.check(&mint_again).is_err());
    Ok(())
}
