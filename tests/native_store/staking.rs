use std::{fs, path::PathBuf};

use anyhow::{Context, ensure};
use ckb_types::packed::Byte32;
use ed25519_dalek::{Signer, SigningKey};
use fern::native_store::StakeGenesis;
use fern_ledger::protocol::{
    self, Certificate, Operation, snapshot_body, snapshot_commitment, snapshot_digest,
};
use fern_stake::{
    Anchor, Attestation, Member, Registry, RegistryAnchor, StakeConfig, StakeData, StakeDomain,
    StakeSnapshot,
};
use serde_json::Value;

use super::*;

#[test]
fn snapshot_replay_and_deferred_committee_update_survive_restart() -> Result<()> {
    let mut fixture = Fixture::new()?;
    fixture.spec.development_cells.clear();
    let guardian = SigningKey::from_bytes(&[99; 32]);
    let sign = |digest: &[u8]| Attestation {
        guardian: 0,
        signature: guardian.sign(digest).to_bytes(),
    };
    let first = SigningKey::from_bytes(&[88; 32]).verifying_key().to_bytes();
    let second = SigningKey::from_bytes(&[77; 32]).verifying_key().to_bytes();
    let generation = [9; 32];
    let domain = StakeDomain {
        fern_network_id: fixture.spec.network.unpack(),
        ckb_genesis: [50; 32],
        registry_id: [51; 32],
    };
    let make_stake = |validator_pubkey, bonded_amount, activation_epoch, exit_epoch| StakeData {
        validator_pubkey,
        bonded_amount,
        activation_epoch,
        exit_epoch,
        frozen: false,
        first_active_height: 1,
        first_inactive_height: 0,
        stake_cell_hash: validator_pubkey,
    };
    let registry = Registry::new(
        domain.fern_network_id,
        1,
        generation,
        vec![make_stake(first, 10, 0, None)],
    )?;
    let registry_anchor = |state: &Registry, height, hash| RegistryAnchor {
        block: Anchor {
            height,
            hash: [hash; 32],
        },
        sequence: state.sequence(),
        commitment: state.commitment(),
    };
    let initial = StakeSnapshot {
        domain,
        guardian_generation: generation,
        parent: None,
        target_epoch: 0,
        registry: registry_anchor(&registry, 1, 11),
    };
    let config = StakeConfig {
        minimum_bond: 1,
        shannons_per_power: 1,
        max_validators: 2,
        max_total_power: 100,
        blocks_per_epoch: 5,
    };
    fixture.spec.stake = Some(StakeGenesis {
        config,
        signing_window: 4,
        limits: protocol::Limits {
            operations: 1,
            bytes: 65536,
            signatures: 1,
        },
        statement: initial,
        registry,
        guardians: vec![Member {
            key: guardian.verifying_key().to_bytes(),
            power: 10,
        }],
        signatures: vec![sign(&initial.signing_digest())],
    });
    let path = tempfile::tempdir()?;
    let mut store = Store::open(path.path(), &fixture.spec, Retention::Recent(4))?;
    assert_eq!(store.genesis_validators(), &[(first, 10)]);
    let mut stakes = vec![
        make_stake(first, 10, 0, Some(1)),
        make_stake(second, 20, 1, None),
    ];
    stakes.sort_unstable_by_key(|stake| stake.validator_pubkey);
    let next = Registry::new(domain.fern_network_id, 2, generation, stakes)?;
    let new = StakeSnapshot {
        domain,
        guardian_generation: generation,
        target_epoch: 1,
        parent: Some(initial.registry),
        registry: registry_anchor(&next, 2, 12),
    };
    let body = snapshot_body(&new, &next);
    let make_tx = |signing_anchor| {
        let statement = protocol::StakeSnapshot {
            epoch: 1,
            signing_anchor,
            commitment: snapshot_commitment(&fixture.spec.network, &body),
        };
        let digest = snapshot_digest(&fixture.spec.network, &statement);
        Transaction::control(
            Operation::StakeSnapshot(statement),
            Certificate::new(body.clone(), vec![sign(digest.as_slice())])?,
        )
    };
    let first_block = store.prepare(&[], TIME, false)?;
    let signing_anchor = first_block.header().id(store.network());
    let tx = make_tx(signing_anchor)?;
    assert!(store.check(&tx).is_err());
    store.commit(first_block)?;
    assert!(
        store
            .check(&make_tx(Byte32::from_slice(&[30; 32])?)?)
            .is_err()
    );
    assert!(
        store
            .prepare(&[tx.clone(), tx.clone()], TIME, false)
            .is_err()
    );
    let pending = store.prepare(std::slice::from_ref(&tx), TIME, false)?;
    assert_eq!(pending.receipts()[0].fee, 0);
    assert!(pending.validator_updates().is_empty());
    store.commit(pending)?;
    assert!(store.check(&tx).is_err());
    drop(store);

    let mut store = Store::open(path.path(), &fixture.spec, Retention::Recent(4))?;
    for height in 3..=4 {
        let block = store.prepare(&[], TIME, true)?;
        assert_eq!(block.header().context.height, height);
        if height + 2 == config.effective_height(1)? {
            assert_eq!(block.validator_updates().len(), 2);
            assert!(block.validator_updates().contains(&Member {
                key: first,
                power: 0
            }));
            assert!(block.validator_updates().contains(&Member {
                key: second,
                power: 20
            }));
        } else {
            assert!(block.validator_updates().is_empty());
        }
        store.commit(block)?;
    }
    let next = store.prepare(&[], TIME, true)?;
    assert!(next.validator_updates().is_empty());
    assert_eq!(store.fee_surplus()?, 0);
    Ok(())
}

#[test]
#[ignore = "uses actual CKB observations from tools/stake_localnet.py"]
fn opens_native_genesis_from_ckb_guardian_observations() -> Result<()> {
    let evidence =
        PathBuf::from(std::env::var_os("FERN_TEST_L1_STAKE").context("stake evidence directory")?);
    let trust: Value = serde_json::from_slice(&fs::read(evidence.join("snapshot-trust.json"))?)?;
    let observations: Value =
        serde_json::from_slice(&fs::read(evidence.join("genesis-observations.json"))?)?;
    let observations = observations.as_array().context("validator observations")?;
    let observation = observations.first().context("validator observation")?;
    let domain = StakeDomain {
        fern_network_id: hex_value(&trust["fern_network_id"])?
            .try_into()
            .ok()
            .context("network ID")?,
        ckb_genesis: hex_value(&trust["ckb_genesis"])?
            .try_into()
            .ok()
            .context("CKB genesis")?,
        registry_id: hex_value(&trust["registry_id"])?
            .try_into()
            .ok()
            .context("stake Registry ID")?,
    };
    let guardian_generation = hex_value(&observation["guardian_generation"])?
        .try_into()
        .ok()
        .context("guardian generation")?;
    let r = &observation["registry"];
    let registry = Registry::decode(&hex_value(&observation["registry_data"])?)?;
    let statement = StakeSnapshot {
        domain,
        guardian_generation,
        target_epoch: 0,
        parent: None,
        registry: RegistryAnchor {
            block: Anchor {
                height: number(&r["height"])?,
                hash: hex_value(&r["block_hash"])?
                    .try_into()
                    .ok()
                    .context("L1 block hash")?,
            },
            sequence: number(&r["sequence"])?,
            commitment: hex_value(&r["commitment"])?
                .try_into()
                .ok()
                .context("L1 registry commitment")?,
        },
    };
    let guardians = trust["guardians"]
        .as_array()
        .context("guardian list")?
        .iter()
        .map(|guardian| {
            Ok(Member {
                key: hex_value(&guardian["ed25519"])?
                    .try_into()
                    .ok()
                    .context("guardian key")?,
                power: number(&guardian["power"])?,
            })
        })
        .collect::<Result<_>>()?;
    let signatures = observations
        .iter()
        .map(|vote| {
            ensure!(
                vote["registry_data"] == observation["registry_data"] && vote["registry"] == *r,
                "guardian observations differ"
            );
            let signature = &vote["attestation"];
            Ok(Attestation {
                guardian: u16::try_from(number(&signature["guardian"])?)?,
                signature: hex_value(&signature["signature"])?
                    .try_into()
                    .ok()
                    .context("guardian signature")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut fixture = Fixture::new()?;
    fixture.spec.development_cells.clear();
    fixture.spec.network = domain.fern_network_id.pack();
    fixture.spec.stake = Some(StakeGenesis {
        config: StakeConfig {
            minimum_bond: number(&trust["minimum_bond"])?,
            shannons_per_power: number(&trust["shannons_per_power"])?,
            max_validators: usize::try_from(number(&trust["max_validators"])?)?,
            max_total_power: number(&trust["max_total_power"])?,
            blocks_per_epoch: number(&trust["blocks_per_epoch"])?,
        },
        signing_window: 5,
        limits: protocol::Limits {
            operations: 1,
            bytes: 64 * 1024,
            signatures: 16,
        },
        statement,
        registry,
        guardians,
        signatures,
    });
    let dir = tempfile::tempdir()?;
    let store = Store::open(dir.path(), &fixture.spec, Retention::Archive)?;
    assert_eq!(
        store
            .genesis_validators()
            .iter()
            .map(|(_, power)| *power)
            .collect::<Vec<_>>(),
        [250]
    );
    drop(store);
    fixture.spec.stake.as_mut().unwrap().signatures.pop();
    assert!(Store::open(dir.path(), &fixture.spec, Retention::Archive).is_err());
    Ok(())
}

fn hex_value(value: &Value) -> Result<Vec<u8>> {
    Ok(hex::decode(
        value
            .as_str()
            .context("hex JSON value")?
            .trim_start_matches("0x"),
    )?)
}

fn number(value: &Value) -> Result<u64> {
    value.as_u64().context("integer JSON value")
}
