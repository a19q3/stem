use std::collections::BTreeMap;

use anyhow::{Context, Result};
use ckb_types::{
    bytes::Bytes,
    packed::{Byte32, OutPoint},
    prelude::*,
};
use ed25519_dalek::{Signer, SigningKey};
use fern_ledger::{
    BlockContext, GroupWitness, ResolvedTransaction, Transaction,
    block::HeaderContext,
    protocol::{
        BlockBudget, BridgeLedger, BridgeSettlement, Certificate, L1Anchor, Lane, Limits,
        Operation, PendingWithdrawal, SettlementAction, SettlementChanges, StakeSnapshot,
        VaultUpdate, settlement_digest,
    },
    replay::{InsertionBudget, Key},
};
use fern_stake::{Attestation, Authority, Member};

fn hash(byte: u8) -> Byte32 {
    [byte; 32].pack()
}

fn point(byte: u8, index: u32) -> OutPoint {
    OutPoint::new(hash(byte), index)
}

fn limits(operations: u32) -> Limits {
    Limits {
        operations,
        bytes: 512 * 1024,
        signatures: 16,
    }
}

#[test]
fn fees_and_resource_caps_are_independent_for_control_transactions() -> Result<()> {
    let snapshot = Operation::StakeSnapshot(StakeSnapshot {
        epoch: 4,
        signing_anchor: hash(10),
        commitment: hash(8),
    });
    let certificate = Certificate::new(
        Bytes::from_static(b"snapshot fixture"),
        vec![Attestation {
            guardian: 0,
            signature: [1; 64],
        }],
    )?;
    let tx = Transaction::control(snapshot.clone(), certificate.clone())?;
    let decoded = Transaction::from_slice(tx.as_slice())?;
    assert_eq!(decoded.operation(), Some(snapshot));
    assert_eq!(decoded.inputs().len(), 0);
    assert_eq!(decoded.outputs().len(), 0);
    assert!(
        decoded
            .with_witnesses(vec![GroupWitness {
                group_index: 0,
                proof_data: Bytes::new(),
                seal: Bytes::from_static(b"seal")
            }])
            .is_err()
    );
    assert!(Transaction::new(vec![], vec![], vec![], vec![], vec![]).is_err());

    let block = HeaderContext {
        height: 1,
        timestamp_ms: 0,
        parent: hash(0),
        transaction_root: hash(0),
    };
    assert!(
        ResolvedTransaction::resolve(
            hash(1),
            decoded,
            BlockContext {
                candidate: &block,
                parent_timestamp_ms: 0,
                capacity: Default::default(),
            },
            |_| panic!("control transaction queried Live Cells"),
            |_| panic!("control transaction queried headers"),
        )
        .is_err()
    );
    let mut budget = BlockBudget::new(limits(0), limits(1), limits(1));
    budget.charge(&tx)?;
    assert!(budget.charge(&tx).is_err());

    let mut zero_signature_budget = BlockBudget::new(
        limits(0),
        Limits {
            signatures: 0,
            ..limits(1)
        },
        limits(1),
    );
    assert!(zero_signature_budget.charge(&tx).is_err());
    let mut zero_byte_budget = BlockBudget::new(
        limits(0),
        Limits {
            bytes: 0,
            ..limits(1)
        },
        limits(1),
    );
    assert!(zero_byte_budget.charge(&tx).is_err());
    Ok(())
}

#[derive(Default)]
struct Bridge {
    lanes: BTreeMap<Byte32, Lane>,
    pending: BTreeMap<(Byte32, u64), PendingWithdrawal>,
}

impl BridgeLedger for Bridge {
    fn lane(&self, id: &Byte32) -> Result<Lane> {
        self.lanes.get(id).cloned().context("unknown vault lane")
    }

    fn pending(&self, id: &Byte32, nonce: u64) -> Result<PendingWithdrawal> {
        self.pending
            .get(&(id.clone(), nonce))
            .cloned()
            .context("no pending receipt")
    }
}

#[test]
fn authenticated_settlement_consumes_receipts_and_preserves_reserved_capacity() -> Result<()> {
    let network = hash(1);
    let l1_genesis = hash(2);
    let generation = [3; 32];
    let mut keys: Vec<_> = (1..=4)
        .map(|id| SigningKey::from_bytes(&[id; 32]))
        .collect();
    keys.sort_by_key(|key| key.verifying_key().to_bytes());
    let committee: Vec<_> = keys
        .iter()
        .map(|key| Member {
            key: key.verifying_key().to_bytes(),
            power: 1,
        })
        .collect();
    let authority = Authority::new(generation, &committee)?;
    let mut budget = BlockBudget::new(limits(0), limits(1), limits(16));

    let source = hash(7);
    let destination = hash(8);
    let mut state = Bridge::default();
    state.lanes.insert(
        source.clone(),
        Lane {
            vault: point(10, 0),
            next_nonce: 1,
            capacity: 100,
            units: 100,
            reserved_capacity: 60,
            reserved_units: 50,
        },
    );
    state.lanes.insert(
        destination.clone(),
        Lane {
            vault: point(11, 0),
            next_nonce: 0,
            capacity: 20,
            units: 0,
            reserved_capacity: 0,
            reserved_units: 0,
        },
    );
    state.pending.insert(
        (source.clone(), 1),
        PendingWithdrawal {
            receipt: hash(9),
            capacity: 60,
            units: 30,
        },
    );
    let anchor = L1Anchor {
        height: 4,
        hash: hash(14),
    };
    let mut statement = BridgeSettlement {
        l1_genesis: l1_genesis.clone(),
        authority: generation.pack(),
        previous_anchor: anchor.clone(),
        anchor: L1Anchor {
            height: 5,
            hash: hash(15),
        },
        action: SettlementAction::WithdrawalPaid {
            vault: VaultUpdate {
                lane: source.clone(),
                input: point(10, 0),
                output: point(20, 0),
            },
            nonce: 1,
            receipt: hash(9),
        },
    };
    let sign =
        |statement: &BridgeSettlement, count: usize, network: &Byte32| -> Result<Transaction> {
            let digest = settlement_digest(network, statement);
            let votes = keys
                .iter()
                .take(count)
                .enumerate()
                .map(|(index, key)| Attestation {
                    guardian: index as u16,
                    signature: key.sign(digest.as_slice()).to_bytes(),
                })
                .collect();
            let certificate = Certificate::new(Bytes::new(), votes)?;
            Transaction::control(Operation::BridgeSettlement(statement.clone()), certificate)
        };

    let minority = sign(&statement, 2, &network)?;
    assert!(
        SettlementChanges::verify(
            &minority,
            &network,
            &l1_genesis,
            &anchor,
            &authority,
            &state,
            &mut budget
        )
        .is_err()
    );
    let forged = sign(&statement, 3, &hash(99))?;
    assert!(
        SettlementChanges::verify(
            &forged,
            &network,
            &l1_genesis,
            &anchor,
            &authority,
            &state,
            &mut budget
        )
        .is_err()
    );
    let payment = sign(&statement, 3, &network)?;
    let paid = SettlementChanges::verify(
        &payment,
        &network,
        &l1_genesis,
        &anchor,
        &authority,
        &state,
        &mut budget,
    )?;
    assert_eq!(paid.paid, Some((source.clone(), 1)));
    assert_eq!(paid.lanes[0].1.capacity, 40);
    assert_eq!(paid.lanes[0].1.units, 70);
    assert_eq!(paid.lanes[0].1.reserved_capacity, 0);
    assert_eq!(paid.lanes[0].1.reserved_units, 20);
    assert_eq!(paid.lanes[0].1.next_nonce, 2);
    state.pending.remove(&(source.clone(), 1));
    state.lanes.extend(paid.lanes);
    assert!(
        SettlementChanges::verify(
            &payment,
            &network,
            &l1_genesis,
            &paid.anchor,
            &authority,
            &state,
            &mut budget
        )
        .is_err()
    );

    statement.previous_anchor = paid.anchor.clone();
    statement.anchor = L1Anchor {
        height: 6,
        hash: hash(16),
    };
    statement.action = SettlementAction::WithdrawalPaid {
        vault: VaultUpdate {
            lane: source.clone(),
            input: point(20, 0),
            output: point(21, 0),
        },
        nonce: 1,
        receipt: hash(9),
    };
    let repeated_nonce = sign(&statement, 3, &network)?;
    assert!(
        SettlementChanges::verify(
            &repeated_nonce,
            &network,
            &l1_genesis,
            &paid.anchor,
            &authority,
            &state,
            &mut budget,
        )
        .unwrap_err()
        .to_string()
        .contains("nonce")
    );
    let send = VaultUpdate {
        lane: source.clone(),
        input: point(20, 0),
        output: point(21, 0),
    };
    let recv = VaultUpdate {
        lane: destination.clone(),
        input: point(11, 0),
        output: point(21, 1),
    };
    state.lanes.get_mut(&source).unwrap().reserved_capacity = 25;
    statement.action = SettlementAction::ReserveMoved {
        source: send.clone(),
        destination: recv.clone(),
        capacity: 16,
    };
    let unfunded = sign(&statement, 3, &network)?;
    let mut fresh_budget = BlockBudget::new(limits(0), limits(1), limits(16));
    assert!(
        SettlementChanges::verify(
            &unfunded,
            &network,
            &l1_genesis,
            &paid.anchor,
            &authority,
            &state,
            &mut fresh_budget
        )
        .is_err()
    );
    statement.action = SettlementAction::ReserveMoved {
        source: send,
        destination: recv,
        capacity: 15,
    };
    let move_reserve = sign(&statement, 3, &network)?;
    let moved = SettlementChanges::verify(
        &move_reserve,
        &network,
        &l1_genesis,
        &paid.anchor,
        &authority,
        &state,
        &mut fresh_budget,
    )?;
    assert_eq!(moved.paid, None);
    assert_eq!(moved.lanes[0].1.capacity, 25);
    assert_eq!(moved.lanes[1].1.capacity, 35);
    assert_eq!(moved.lanes[0].1.units, 70);
    assert_eq!(moved.lanes[0].1.next_nonce, 2);
    Ok(())
}

#[test]
fn permanent_replay_keys_are_distinct_and_budgeted() -> Result<()> {
    let network = hash(1);
    let deposit = Key::Deposit {
        l1_genesis: hash(2),
        deposit: point(3, 0),
    };
    let retire = Key::Retirement {
        registry: hash(4),
        validator: hash(5),
        generation: 6,
    };
    let mut block = InsertionBudget::new(1, 1, 0)?;
    block.reserve(&network, &deposit, 40_000, |_| Ok(false))?;
    assert!(
        block
            .reserve(&network, &deposit, 40_000, |_| panic!(
                "duplicate consulted committed state"
            ))
            .is_err()
    );
    let other_deposit = Key::Deposit {
        l1_genesis: hash(2),
        deposit: point(3, 1),
    };
    assert!(
        block
            .reserve(&network, &other_deposit, 40_000, |_| Ok(false))
            .is_err()
    );
    block.reserve(&network, &retire, 40_005, |_| Ok(false))?;
    let (total, state) = block.into_writes();
    assert_eq!(total, 2);
    assert_eq!(fern_ledger::replay::logical_bytes(total)?, 80);
    assert_eq!(
        state[&deposit.commitment(&network)],
        40_000u64.to_le_bytes()
    );
    assert_eq!(state[&retire.commitment(&network)], 40_005u64.to_le_bytes());

    let mut restored = InsertionBudget::new(1, 1, total)?;
    assert!(
        restored
            .reserve(
                &network,
                &deposit,
                55_000,
                |key| Ok(state.contains_key(key))
            )
            .is_err()
    );
    assert!(
        restored
            .reserve(&network, &retire, 55_000, |key| Ok(state.contains_key(key)))
            .is_err()
    );
    restored.reserve(&network, &other_deposit, 55_000, |key| {
        Ok(state.contains_key(key))
    })?;
    let (total, writes) = restored.into_writes();
    assert_eq!(total, 3);
    assert_eq!(writes.len(), 1);
    assert!(fern_ledger::replay::logical_bytes(u64::MAX).is_err());
    Ok(())
}

#[test]
fn stake_snapshot_checks_signing_window_commitment_and_quorum() -> Result<()> {
    use fern_ledger::protocol::{
        SnapshotContext, snapshot_body, snapshot_commitment, snapshot_digest,
    };
    use fern_stake::{
        AcceptedSnapshot, Anchor, Registry, RegistryAnchor, StakeConfig, StakeData, StakeDomain,
        StakeSnapshot as RegistrySnapshot,
    };

    let network = hash(1);
    let generation = [7u8; 32];
    let domain = StakeDomain {
        fern_network_id: network.unpack(),
        ckb_genesis: [2; 32],
        registry_id: [3; 32],
    };
    let mut keys: Vec<_> = (1..=4)
        .map(|ix| SigningKey::from_bytes(&[ix; 32]))
        .collect();
    keys.sort_by_key(|key| key.verifying_key().to_bytes());
    let members: Vec<_> = keys
        .iter()
        .map(|key| Member {
            key: key.verifying_key().to_bytes(),
            power: 1,
        })
        .collect();
    let authority = Authority::new(generation, &members)?;
    let record = StakeData {
        validator_pubkey: members[0].key,
        bonded_amount: 200,
        activation_epoch: 0,
        exit_epoch: None,
        frozen: false,
        stake_cell_hash: [6; 32],
    };
    let registry = Registry::new(domain.fern_network_id, 1, generation, vec![record])?;
    let previous = AcceptedSnapshot {
        epoch: 0,
        anchor: RegistryAnchor {
            block: Anchor {
                height: 40,
                hash: [8; 32],
            },
            sequence: 0,
            commitment: [9; 32],
        },
    };
    let snapshot = RegistrySnapshot {
        domain,
        registry: RegistryAnchor {
            block: Anchor {
                height: 41,
                hash: [10; 32],
            },
            sequence: 1,
            commitment: registry.commitment(),
        },
        parent: Some(previous.anchor),
        target_epoch: 1,
        guardian_generation: generation,
    };
    assert_eq!(RegistrySnapshot::decode(&snapshot.encode())?, snapshot);
    let body = snapshot_body(&snapshot, &registry);
    let anchor = hash(11);
    let statement = StakeSnapshot {
        epoch: 1,
        signing_anchor: anchor.clone(),
        commitment: snapshot_commitment(&network, &body),
    };
    let sign = |statement: StakeSnapshot, body: Bytes, count: usize| -> Result<Transaction> {
        let votes = keys
            .iter()
            .take(count)
            .enumerate()
            .map(|(index, key)| Attestation {
                guardian: index as u16,
                signature: key
                    .sign(snapshot_digest(&network, &statement).as_slice())
                    .to_bytes(),
            })
            .collect();
        Transaction::control(
            Operation::StakeSnapshot(statement),
            Certificate::new(body, votes)?,
        )
    };
    let config = StakeConfig {
        minimum_bond: 100,
        shannons_per_power: 10,
        max_validators: 4,
        max_total_power: 1000,
        blocks_per_epoch: 10,
    };
    let context = SnapshotContext {
        height: 9,
        window: 4,
        signing_anchor_height: 6,
        signing_anchor: &anchor,
        domain: &domain,
        previous,
        authority: &authority,
        config: &config,
    };
    let mut budget = BlockBudget::new(limits(0), limits(4), limits(0));
    let tx = sign(statement.clone(), body.clone(), 3)?;
    let (accepted, committee) = context.verify(&tx, &mut budget)?;
    assert_eq!(accepted.epoch, 1);
    assert_eq!(
        committee,
        vec![Member {
            key: record.validator_pubkey,
            power: 20
        }]
    );
    assert!(
        SnapshotContext {
            height: 10,
            ..context
        }
        .verify(&tx, &mut budget)
        .is_err()
    );
    assert!(
        SnapshotContext {
            height: 6,
            ..context
        }
        .verify(&tx, &mut budget)
        .is_err()
    );
    assert!(
        SnapshotContext {
            previous: accepted,
            ..context
        }
        .verify(&tx, &mut budget)
        .is_err()
    );
    let changed = sign(
        StakeSnapshot {
            signing_anchor: hash(12),
            ..statement.clone()
        },
        body.clone(),
        3,
    )?;
    assert!(context.verify(&changed, &mut budget).is_err());
    let corrupted = sign(statement.clone(), Bytes::from_static(b"wrong body"), 3)?;
    assert!(context.verify(&corrupted, &mut budget).is_err());
    let minority = sign(statement, body, 2)?;
    assert!(context.verify(&minority, &mut budget).is_err());
    Ok(())
}

#[test]
fn native_mint_authenticates_exact_capacity_and_outputs() -> Result<()> {
    use ckb_types::{core::ScriptHashType, packed::Script};
    use fern_ledger::{
        Cell,
        protocol::{BridgeMint, MintObservation, VerifiedMint},
    };
    let network = hash(1);
    let genesis = hash(2);
    let guardian = SigningKey::from_bytes(&[70; 32]);
    let power = Member {
        key: guardian.verifying_key().to_bytes(),
        power: 1,
    };
    let authority = Authority::new([9; 32], &[power])?;
    let previous = L1Anchor {
        height: 3,
        hash: hash(3),
    };
    let deposit = BridgeMint {
        l1_genesis: genesis.clone(),
        deposits: vec![point(7, 0), point(8, 3)],
    };
    let observation = MintObservation {
        authority: hash(9),
        previous_anchor: previous.clone(),
        anchor: L1Anchor {
            height: 4,
            hash: hash(4),
        },
        vault: VaultUpdate {
            lane: hash(5),
            input: point(5, 0),
            output: point(6, 0),
        },
        capacity: 20_000_000_000,
        units: 0,
    };
    let lock = Script::new_builder()
        .hash_type(ScriptHashType::Data)
        .code_hash([42; 32])
        .build();
    let create = |capacity| Cell::new(capacity, lock.clone(), vec![], Bytes::new());
    let valid = create(observation.capacity)?;
    let digest = Transaction::bridge_mint_signing_digest(
        &network,
        &deposit,
        &observation,
        vec![],
        vec![valid.clone()],
    )?;
    let signature = Attestation {
        guardian: 0,
        signature: guardian.sign(digest.as_slice()).to_bytes(),
    };
    let certificate = Certificate::new(observation.encode(), vec![signature])?;
    let tx = Transaction::bridge_mint(
        deposit.clone(),
        certificate.clone(),
        vec![],
        vec![valid.clone()],
    )?;
    let inflated = Transaction::bridge_mint(
        deposit.clone(),
        certificate.clone(),
        vec![],
        vec![create(observation.capacity + 1)?],
    )?;
    let dup = BridgeMint {
        deposits: vec![point(7, 0), point(7, 0)],
        ..deposit.clone()
    };
    assert!(Transaction::bridge_mint(dup, certificate, vec![], vec![valid]).is_err());
    let mut budget = BlockBudget::new(limits(4), limits(0), limits(0));
    assert!(
        VerifiedMint::verify(
            &inflated,
            &network,
            &genesis,
            &previous,
            &authority,
            &mut budget
        )
        .is_err()
    );
    assert!(
        VerifiedMint::verify(&tx, &hash(8), &genesis, &previous, &authority, &mut budget).is_err()
    );
    let grant = VerifiedMint::verify(&tx, &network, &genesis, &previous, &authority, &mut budget)?;

    let header = HeaderContext {
        height: 1,
        timestamp_ms: 0,
        parent: hash(0),
        transaction_root: hash(0),
    };
    let context = BlockContext {
        candidate: &header,
        parent_timestamp_ms: 0,
        capacity: Default::default(),
    };
    let resolve = |tx| {
        ResolvedTransaction::resolve_mint(
            network.clone(),
            tx,
            context,
            &grant,
            None,
            |_| anyhow::bail!("unexpected Cell read"),
            |_| anyhow::bail!("unexpected Header read"),
        )
    };
    assert!(resolve(inflated).is_err());
    let resolved = resolve(tx)?;
    assert_eq!(resolved.fee(), 0);
    assert_eq!(resolved.outputs()[0].capacity(), observation.capacity);
    Ok(())
}
