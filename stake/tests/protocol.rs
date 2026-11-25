use ed25519_dalek::{Signer, SigningKey};
use fern_stake::*;

fn bytes<const N: usize>(value: &str) -> [u8; N] {
    assert_eq!(value.len(), N * 2);
    core::array::from_fn(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).unwrap())
}

#[test]
fn verifies_real_cometbft_vote_sign_bytes() -> Result<()> {
    // Generated using CometBFT 0.38.26 types.VoteSignBytes and its Ed25519 signer.
    let chain_id = "fern-stake-fixture";
    let validator = bytes("8b143b3cd645c965157b8ace56bcf2605a087d1195db38010b745976ce63724a");
    let signatures = [
        bytes(
            "3591fefa13b2ed821b48ad848eb506a2a6c157b7fa6b374dff53dbc71f104a458f744c417177e93bc6ca9ca83ff9b43b7c595dd77dd0873ae4c9014892f2f70d",
        ),
        bytes(
            "577942d5ad51fce0149f6be35f5b8ffbe7ee95bf2f63b32417833831e80b1bcd481b670d78ab0d5f969bbc50e7a211757f1c9b045c27ec759267ee1ceef0d508",
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
        "730802112a0000000000000022480a20010101010101010101010101010101010101010101010101010101010101010112240802122005050505050505050505050505050505050505050505050505050505050505052a080880e2cfaa06100d32126665726e2d7374616b652d66697874757265",
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
        fern_network_id: [11; 32],
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
            bonded_amount: [100, 200, 200][i],
            activation_epoch: 0,
            exit_epoch: (i == 1).then_some(1),
            frozen: false,
            stake_cell_hash: [i as u8 + 1; 32],
        })
        .collect();
    let registry = Registry::new(domain.fern_network_id, 0, [14; 32], records.clone())?;
    assert_eq!(registry, Registry::decode(&registry.encode())?);
    let mut duplicate = records.clone();
    duplicate[1] = duplicate[0];
    assert_eq!(
        Registry::new(domain.fern_network_id, 0, [14; 32], duplicate),
        Err(Error::Encoding)
    );
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
    let omitted = Registry::new(domain.fern_network_id, 0, [14; 32], records[..2].to_vec())?;
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
