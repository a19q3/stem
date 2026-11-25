use ed25519_dalek::{Signer, SigningKey};
use stem_stake::*;

fn bytes<const N: usize>(value: &str) -> [u8; N] {
    assert_eq!(value.len(), N * 2);
    core::array::from_fn(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap())
}

#[test]
fn verifies_real_cometbft_vote_sign_bytes() -> Result<()> {
    // Generated using CometBFT 0.38.26 types.VoteSignBytes and its Ed25519 signer.
    let chain_id = "stem-stake-fixture";
    let validator = bytes("fbf590e7a87e2f4453ed292b83ab5746fd445dc7d93dfbe62cff374c742e8674");
    let signatures = [
        bytes(
            "52a6ab0715103ac281a41a6d43ade2fe9133f91474dbabc40cc95c00453f67979441aa8fd2255022c8a2ed1a4e2485ef8a3b12b7c365e8b29c76eaee6c13280d",
        ),
        bytes(
            "c0e4198ece6bdcb89cb29a0c76a1d380a0a4d525ed177b1b2d683f914dfb22185e3c7ec638cf6882e4cc201897d760561857a9ee6810a782f1c677430a147a0d",
        ),
    ];
    let vote = |index: usize| SignedVote {
        kind: VoteKind::Precommit,
        height: 42,
        round: 0,
        block_id: Some(BlockId {
            hash: [index as u8 + 1; 32],
            parts: 2,
            part_set_hash: [index as u8 + 5; 32],
        }),
        seconds: 1_700_000_000,
        nanos: index as i32 + 13,
        signature: signatures[index],
    };
    let mut evidence = DoubleVote {
        first: vote(0),
        second: vote(1),
    };
    let liability = Liability {
        validator,
        first_active_height: 25,
        first_inactive_height: Some(43),
    };
    let expected: [u8; 116] = bytes(
        "730802112a0000000000000022480a20010101010101010101010101010101010101010101010101010101010101010112240802122005050505050505050505050505050505050505050505050505050505050505052a080880e2cfaa06100d32127374656d2d7374616b652d66697874757265",
    );
    assert_eq!(evidence.first.sign_bytes(chain_id)?, expected);
    let key = evidence.verify(&[5; 32], chain_id, &liability)?;
    core::mem::swap(&mut evidence.first, &mut evidence.second);
    assert_eq!(key, evidence.verify(&[5; 32], chain_id, &liability)?);
    assert_eq!(
        evidence.verify(&[5; 32], "other-chain", &liability),
        Err(Error::Signature)
    );
    assert_eq!(
        evidence.verify(
            &[5; 32],
            chain_id,
            &Liability {
                first_inactive_height: Some(42),
                ..liability
            },
        ),
        Err(Error::Evidence)
    );
    evidence.second.kind = VoteKind::Prevote;
    assert_eq!(
        evidence.verify(&[5; 32], chain_id, &liability),
        Err(Error::Evidence)
    );
    Ok(())
}

#[test]
fn authenticates_complete_registry_and_rejects_fork_or_replay() -> Result<()> {
    let mut keys: Vec<_> = (1..=3).map(|n| SigningKey::from_bytes(&[n; 32])).collect();
    keys.sort_by_key(|key| key.verifying_key().to_bytes());
    let guardians: Vec<_> = keys
        .iter()
        .map(|key| Member {
            key: key.verifying_key().to_bytes(),
            power: 1,
        })
        .collect();
    let domain = StakeDomain {
        stem_network_id: [11; 32],
        ckb_genesis: [12; 32],
        registry_id: [13; 32],
    };
    let authority = Authority::new([14; 32], &guardians)?;
    let config = StakeConfig {
        minimum_bond: 100,
        shannons_per_power: 50,
        max_validators: 2,
        max_total_power: 50,
        blocks_per_epoch: 10,
    };
    let records: Vec<_> = guardians
        .iter()
        .enumerate()
        .map(|(i, member)| StakeData {
            validator_pubkey: member.key,
            delegated_amount: 0,
            unbonding_amount: 0,
            bonded_amount: [100, 200, 200][i],
            activation_epoch: 0,
            exit_epoch: (i == 1).then_some(1),
            fault_height: 0,
            first_active_height: 1,
            first_inactive_height: 0,
            stake_cell_hash: [i as u8 + 1; 32],
        })
        .collect();
    let registry = Registry::new(
        domain.stem_network_id,
        0,
        [14; 32],
        [0; 32],
        records.clone(),
    )?;
    assert_eq!(registry, Registry::decode(&registry.encode())?);
    let mut over_cap = records.clone();
    over_cap[0].delegated_amount = 4_900;
    let over_cap = Registry::new(domain.stem_network_id, 0, [14; 32], [0; 32], over_cap)?;
    let weighted = config.select(&over_cap, 0)?;
    assert_eq!(
        weighted,
        vec![
            Member {
                key: guardians[0].key,
                power: 47
            },
            Member {
                key: guardians[1].key,
                power: 2
            }
        ]
    );
    let mut duplicate = records.clone();
    duplicate[1] = duplicate[0];
    assert_eq!(
        Registry::new(domain.stem_network_id, 0, [14; 32], [0; 32], duplicate),
        Err(Error::Encoding)
    );
    let mut not_eligible = records.clone();
    not_eligible[0].first_active_height = 0;
    not_eligible[1].first_active_height = 11;
    not_eligible[2].exit_epoch = Some(1);
    not_eligible[2].first_inactive_height = 11;
    let limited = Registry::new(domain.stem_network_id, 0, [14; 32], [0; 32], not_eligible)?;
    assert_eq!(config.select(&limited, 0), Err(Error::Committee));
    assert_eq!(config.select(&limited, 1), Err(Error::Committee));
    let snapshot = StakeSnapshot {
        domain,
        registry: RegistryAnchor {
            block: Anchor {
                height: 17,
                hash: [17; 32],
            },
            sequence: 0,
            commitment: registry.commitment(),
        },
        parent: None,
        target_epoch: 0,
        guardian_generation: [14; 32],
    };
    let certify = |snapshot: &StakeSnapshot| {
        keys.iter()
            .enumerate()
            .map(|(i, key)| Attestation {
                guardian: i as u16,
                signature: key.sign(&snapshot.signing_digest()).to_bytes(),
            })
            .collect::<Vec<_>>()
    };
    let votes = certify(&snapshot);
    assert_eq!(
        snapshot.verify(&domain, None, &registry, &authority, &votes[..2], &config),
        Err(Error::Quorum)
    );
    let omitted = Registry::new(
        domain.stem_network_id,
        0,
        [14; 32],
        [0; 32],
        records[..2].to_vec(),
    )?;
    assert_eq!(
        snapshot.verify(&domain, None, &omitted, &authority, &votes, &config),
        Err(Error::Sequence)
    );
    let (accepted, members) =
        snapshot.verify(&domain, None, &registry, &authority, &votes, &config)?;
    assert_eq!(
        members.iter().map(|m| m.key).collect::<Vec<_>>(),
        [guardians[1].key, guardians[2].key]
    );
    assert_eq!(config.update_height(1)?, 9);
    assert_eq!(config.effective_height(1)?, 11);
    let mut next = StakeSnapshot {
        registry: RegistryAnchor {
            block: Anchor {
                height: 21,
                hash: [21; 32],
            },
            ..snapshot.registry
        },
        parent: Some(accepted.anchor),
        target_epoch: 1,
        ..snapshot
    };
    let (updated, members) = next.verify(
        &domain,
        Some(accepted),
        &registry,
        &authority,
        &certify(&next),
        &config,
    )?;
    assert_eq!(
        members.iter().map(|m| m.key).collect::<Vec<_>>(),
        [guardians[0].key, guardians[2].key]
    );
    assert_eq!(
        next.verify(
            &domain,
            Some(updated),
            &registry,
            &authority,
            &certify(&next),
            &config
        ),
        Err(Error::Epoch)
    );
    next.parent.as_mut().unwrap().block.hash = [4; 32];
    assert_eq!(
        next.verify(
            &domain,
            Some(accepted),
            &registry,
            &authority,
            &certify(&next),
            &config
        ),
        Err(Error::Anchor)
    );
    Ok(())
}

#[test]
fn retirement_commitment_keeps_registry_slots_reusable() -> Result<()> {
    use stem_stake::retirement::{EMPTY, RetirementTree, insert};

    let network = [7; 32];
    let first = [12; 32];
    let mut next = first;
    next[30] ^= 0x20;
    let mut tree = RetirementTree::new(network);
    let path = tree.prove_absence(&first)?;
    let first_root = insert(&network, &first, &EMPTY, &path)?;
    tree.retire(first)?;
    assert_eq!(first_root, tree.root());
    assert_eq!(tree.prove_absence(&first), Err(Error::Sequence));
    assert_eq!(
        insert(&network, &first, &first_root, &path),
        Err(Error::Sequence)
    );
    let path = tree.prove_absence(&next)?;
    assert_eq!(path.len(), 64);
    let second_root = insert(&network, &next, &first_root, &path)?;
    tree.retire(next)?;
    assert_eq!(second_root, tree.root());
    let mut damaged = path;
    damaged[32] ^= 1;
    assert_eq!(
        insert(&network, &next, &first_root, &damaged),
        Err(Error::Sequence)
    );
    Ok(())
}

#[test]
fn pending_delegation_enforces_the_first_excluded_height() -> Result<()> {
    use stem_stake::l1::{
        DelegationAction, DelegationBond, Policy, RELATIVE_BLOCKS, verify_delegation,
    };

    let policy = Policy {
        registry_id: [4; 32],
        network_id: [5; 32],
        ckb_genesis: [6; 32],
        chain_id: "stake-reduction-test",
        minimum_bond: 100,
        blocks_per_epoch: 10,
        unbonding_blocks: 20,
        checkpoint: stem_stake::finality::CheckpointId {
            code_hash: [7; 32],
            type_id: [8; 32],
        },
    };
    let validator_pubkey = [9; 32];
    let bond = DelegationBond {
        active: 75,
        pending: 100,
        requested_sequence: 12,
        cutoff_height: 11,
        creation_sequence: 0,
        first_active_height: 1,
    };
    assert_eq!(DelegationBond::decode(&bond.encode())?, bond);
    assert!(DelegationBond::decode(&DelegationBond { pending: 0, ..bond }.encode()).is_err());
    let stake = StakeData {
        validator_pubkey,
        bonded_amount: 250,
        delegated_amount: bond.active,
        unbonding_amount: bond.pending,
        activation_epoch: 0,
        exit_epoch: None,
        fault_height: 10,
        first_active_height: 1,
        first_inactive_height: 0,
        stake_cell_hash: [10; 32],
    };
    let registry = |sequence, stake| {
        Registry::new(policy.network_id, sequence, [11; 32], [0; 32], vec![stake])
    };
    let prior = registry(13, stake)?;
    let next = registry(
        14,
        StakeData {
            unbonding_amount: 0,
            ..stake
        },
    )?;
    let verify = |action| verify_delegation(&policy, &prior, &next, &validator_pubkey, action);
    let withdraw = |cutoff, delay| DelegationAction::WithdrawReduction {
        amount: bond.pending,
        start: bond.first_active_height,
        cutoff,
        since: RELATIVE_BLOCKS | delay,
    };
    assert_eq!(verify(withdraw(11, 20)), Err(Error::Sequence));
    assert_eq!(verify(withdraw(10, 19)), Err(Error::Sequence));
    verify(withdraw(10, 20))?;
    let slash = |cutoff, fee| DelegationAction::SlashReduction {
        amount: bond.pending,
        start: bond.first_active_height,
        cutoff,
        fee,
        historical_evidence: false,
    };
    assert_eq!(verify(slash(10, 100)), Err(Error::Evidence));
    assert_eq!(verify(slash(11, 99)), Err(Error::Evidence));
    verify(DelegationAction::WithdrawReduction {
        amount: bond.pending,
        start: 11,
        cutoff: 21,
        since: RELATIVE_BLOCKS | 20,
    })?;
    assert_eq!(
        verify(DelegationAction::SlashReduction {
            amount: bond.pending,
            start: 11,
            cutoff: 21,
            fee: bond.pending,
            historical_evidence: false,
        }),
        Err(Error::Evidence)
    );
    verify(slash(11, 100))
}
