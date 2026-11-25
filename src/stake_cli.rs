use std::{collections::HashSet, path::PathBuf};

use anyhow::{Context, Result, ensure};
use ckb_hash::new_blake2b;
use ckb_jsonrpc_types::{CellWithStatus, JsonBytes, OutPoint, Script as JsonScript, Uint64};
use ckb_types::{
    H256,
    core::{Capacity, ScriptHashType},
    packed::{CellOutput, Script, Transaction},
    prelude::*,
};
use clap::{Args, Subcommand};
use ed25519_dalek::Signer;
use fern_stake::{
    AcceptedSnapshot, Anchor, Attestation, Authority, MAX_RECORDS, Member, Registry,
    RegistryAnchor, StakeConfig, StakeDomain, StakeSnapshot,
    l1::{GuardianProof, Policy, StakeCell},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    bridge_cli::{print_json, read_json_limited, read_key},
    l1::{Journal, Rpc},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Validator {
    ed25519: JsonBytes,
    power: i64,
}

#[derive(Subcommand)]
pub enum StakeCommand {
    /// Verify every registry-backed live Stake Cell on a trusted full CKB node.
    Observe(Observe),
    /// Authenticate a threshold-certified snapshot against a pinned network and guardian set.
    Verify(Verify),
    /// Assemble a Stake Snapshot transaction from guardian observations.
    Snapshot {
        #[arg(long)]
        observations: PathBuf,
    },
}

#[derive(Args)]
pub struct Observe {
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    l1_rpc: String,
    #[arg(long)]
    journal: PathBuf,
    #[arg(long)]
    key_file: PathBuf,
    /// Raw Fern Header ID at the scheduled signing height.
    #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
    fern_signing_anchor: Option<H256>,
}

#[derive(Args)]
pub struct Verify {
    #[arg(long)]
    trust: PathBuf,
    #[arg(long)]
    observations: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Trust {
    fern_network_id: H256,
    ckb_genesis: H256,
    registry_id: H256,
    target_epoch: u64,
    previous: Option<Previous>,
    guardians: Vec<Validator>,
    minimum_bond: u64,
    shannons_per_power: u64,
    max_validators: usize,
    max_total_power: u64,
    blocks_per_epoch: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Previous {
    epoch: u64,
    registry: ObservedRegistry,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Signature {
    guardian: u16,
    signature: JsonBytes,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observation {
    fern_network_id: H256,
    ckb_genesis: H256,
    registry_id: H256,
    registry: ObservedRegistry,
    parent: Option<ObservedRegistry>,
    target_epoch: u64,
    guardian_generation: H256,
    registry_data: JsonBytes,
    attestation: Signature,
    native: Option<NativeVote>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct NativeVote {
    signing_anchor: H256,
    signature: JsonBytes,
}

impl Observation {
    fn snapshot(&self) -> StakeSnapshot {
        StakeSnapshot {
            domain: domain(&self.fern_network_id, &self.ckb_genesis, &self.registry_id),
            registry: (&self.registry).into(),
            parent: self.parent.as_ref().map(Into::into),
            target_epoch: self.target_epoch,
            guardian_generation: self
                .guardian_generation
                .as_bytes()
                .try_into()
                .expect("Hash32"),
        }
    }
}

fn domain(fern_network_id: &H256, ckb_genesis: &H256, registry_id: &H256) -> StakeDomain {
    StakeDomain {
        fern_network_id: fern_network_id.as_bytes().try_into().expect("Hash32"),
        ckb_genesis: ckb_genesis.as_bytes().try_into().expect("Hash32"),
        registry_id: registry_id.as_bytes().try_into().expect("Hash32"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    fern_network_id: H256,
    ckb_genesis: H256,
    registry_type: JsonScript,
    registry: OutPoint,
    stake_cells: Vec<OutPoint>,
    confirmations: u64,
    target_epoch: u64,
    parent: Option<ObservedRegistry>,
    guardians: Vec<Validator>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObservedRegistry {
    height: u64,
    block_hash: H256,
    sequence: u64,
    commitment: H256,
}

impl From<RegistryAnchor> for ObservedRegistry {
    fn from(value: RegistryAnchor) -> Self {
        Self {
            height: value.block.height,
            block_hash: value.block.hash.into(),
            sequence: value.sequence,
            commitment: value.commitment.into(),
        }
    }
}

impl From<&ObservedRegistry> for RegistryAnchor {
    fn from(value: &ObservedRegistry) -> Self {
        Self {
            block: Anchor {
                height: value.height,
                hash: value.block_hash.as_bytes().try_into().expect("Hash32"),
            },
            sequence: value.sequence,
            commitment: value.commitment.as_bytes().try_into().expect("Hash32"),
        }
    }
}

impl StakeCommand {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Observe(args) => args.run(),
            Self::Verify(args) => args.run(),
            Self::Snapshot { observations } => build_native_snapshot(&observations),
        }
    }
}

impl Verify {
    fn run(self) -> Result<()> {
        let trust: Trust = read_json_limited(&self.trust, 128 * 1024)?;
        let votes: Vec<Observation> = read_json_limited(&self.observations, 16 * 1024 * 1024)?;
        ensure!(
            (1..=MAX_RECORDS).contains(&votes.len()),
            "incorrect number of guardian observations"
        );
        let first = &votes[0];
        let snapshot = first.snapshot();
        ensure!(
            snapshot.target_epoch == trust.target_epoch,
            "observation names a different target epoch"
        );
        let registry = Registry::decode(first.registry_data.as_bytes())?;
        let guardians: Vec<_> = trust.guardians.iter().map(member).collect::<Result<_>>()?;
        let generation = GuardianProof {
            committee: &guardians,
            votes: &[],
        }
        .generation();
        let authority = Authority::new(generation, &guardians)?;
        let mut signatures = Vec::with_capacity(votes.len());
        for observation in &votes {
            ensure!(
                observation.snapshot() == snapshot
                    && observation.registry_data == first.registry_data,
                "guardians observed different snapshots"
            );
            signatures.push(Attestation {
                guardian: observation.attestation.guardian,
                signature: observation.attestation.signature.as_bytes().try_into()?,
            });
        }
        signatures.sort_unstable_by_key(|signature| signature.guardian);
        let config = StakeConfig {
            minimum_bond: trust.minimum_bond,
            shannons_per_power: trust.shannons_per_power,
            max_validators: trust.max_validators,
            max_total_power: trust.max_total_power,
            blocks_per_epoch: trust.blocks_per_epoch,
        };
        let accepted = trust.previous.map(|prior| AcceptedSnapshot {
            epoch: prior.epoch,
            anchor: (&prior.registry).into(),
        });
        let (accepted, validators) = snapshot.verify(
            &domain(
                &trust.fern_network_id,
                &trust.ckb_genesis,
                &trust.registry_id,
            ),
            accepted,
            &registry,
            &authority,
            &signatures,
            &config,
        )?;
        let height = config.effective_height(accepted.epoch)?;
        print_json(&json!({
            "epoch": accepted.epoch,
            "effective_height": height,
            "update_height": (accepted.epoch != 0).then(|| config.update_height(accepted.epoch)).transpose()?,
            "registry": ObservedRegistry::from(accepted.anchor),
            "validators": validators.iter().map(|validator| json!({
                "ed25519": H256::from(validator.key), "power": validator.power
            })).collect::<Vec<_>>(),
        }))
    }
}

impl Observe {
    fn run(self) -> Result<()> {
        let request: Request = read_json_limited(&self.request, 256 * 1024)?;
        ensure!(
            request.confirmations > 0 && request.stake_cells.len() <= MAX_RECORDS,
            "invalid observation size or confirmation depth"
        );
        let registry_type: Script = request.registry_type.clone().into();
        ensure!(
            ScriptHashType::try_from(registry_type.hash_type())? == ScriptHashType::Data2,
            "registry requires a Data2 Type Script"
        );
        let registry_id = registry_type.calc_script_hash().unpack();
        let args = registry_type.args().raw_data();
        let policy = Policy::decode(registry_id, &args)?;
        ensure!(
            policy.network_id.as_slice() == request.fern_network_id.as_bytes()
                && policy.ckb_genesis.as_slice() == request.ckb_genesis.as_bytes(),
            "Stake Registry names a different network"
        );
        let l1 = Rpc::new(&self.l1_rpc)?;
        ensure!(
            l1.block_hash(0)? == request.ckb_genesis,
            "incorrect L1 genesis"
        );

        let mut hasher = new_blake2b();
        hasher.update(b"Fern guardian Stake Journal\0");
        hasher.update(&policy.network_id);
        hasher.update(&registry_id);
        let mut id = [0; 32];
        hasher.finalize(&mut id);
        let journal = Journal::open(&self.journal, id.pack())?;
        let previous_height = journal.validate(&l1)?;
        let tip: Uint64 = l1.call("get_tip_block_number", json!([]))?;
        let height = tip
            .value()
            .checked_sub(request.confirmations)
            .context("L1 chain has insufficient confirmations")?;
        ensure!(
            previous_height.is_none_or(|previous| height >= previous),
            "confirmed L1 height regressed"
        );
        let block_hash = l1.block_hash(height)?;
        let parent = request.parent.as_ref().map(RegistryAnchor::from);
        if let Some(parent) = parent {
            ensure!(
                request.target_epoch > 0 && parent.block.height < height,
                "invalid snapshot parent height"
            );
            ensure!(
                l1.block_hash(parent.block.height)?.as_bytes() == &parent.block.hash,
                "snapshot parent was reorganized"
            );
        } else {
            ensure!(
                request.target_epoch == 0,
                "non-genesis snapshot requires a parent"
            );
        }

        let mut lock_args = registry_id.to_vec();
        lock_args.push(0);
        let expected_lock = registry_type
            .clone()
            .as_builder()
            .args(lock_args.pack())
            .build();
        let (output, data) = live_cell(&l1, &request.registry, height)?;
        ensure!(
            output.type_().to_opt().as_ref() == Some(&registry_type)
                && output.lock() == expected_lock,
            "incorrect L1 Registry scripts"
        );
        let registry = Registry::decode(data.as_bytes())?;
        ensure!(
            registry.network_id() == policy.network_id,
            "incorrect Registry network ID"
        );
        if let Some(parent) = parent {
            ensure!(
                registry.sequence() > parent.sequence
                    || (registry.sequence() == parent.sequence
                        && registry.commitment() == parent.commitment),
                "Registry sequence or commitment regressed"
            );
        }

        let live_records: Vec<_> = registry
            .records()
            .iter()
            .filter(|stake| stake.stake_cell_hash != [0; 32])
            .collect();
        ensure!(
            request.stake_cells.len() == live_records.len(),
            "incomplete Stake Cell set"
        );
        let mut points = HashSet::new();
        for (point, stake) in request.stake_cells.iter().zip(live_records) {
            let packed_point: ckb_types::packed::OutPoint = point.clone().into();
            ensure!(points.insert(packed_point), "duplicate Stake Cell OutPoint");
            let (output, data) = live_cell(&l1, point, height)?;
            let mut args = registry_id.to_vec();
            args.extend_from_slice(&stake.validator_pubkey);
            let expected = registry_type.clone().as_builder().args(args.pack()).build();
            ensure!(
                output.type_().to_opt().as_ref() == Some(&expected)
                    && output.lock() == expected_lock,
                "incorrect Stake Cell scripts or registry ordering"
            );
            StakeCell::verify_record(stake, data.as_bytes())?;
            let occupied = output
                .occupied_capacity(Capacity::bytes(data.len())?)?
                .as_u64();
            ensure!(
                occupied.checked_add(stake.bonded_amount) == Some(u64::from(output.capacity())),
                "Stake Cell capacity disagrees with the Registry"
            );
        }

        let guardians = request
            .guardians
            .iter()
            .map(member)
            .collect::<Result<Vec<_>>>()?;
        let generation = GuardianProof {
            committee: &guardians,
            votes: &[],
        }
        .generation();
        ensure!(
            generation == registry.guardian_generation(),
            "guardian committee does not match the live Registry"
        );
        Authority::new(generation, &guardians)?;
        let key = read_key(&self.key_file)?;
        let guardian = guardians
            .binary_search_by_key(&key.verifying_key().to_bytes(), |member| member.key)
            .map_err(|_| {
                anyhow::anyhow!("signing key is outside the registered guardian committee")
            })?;
        let snapshot = StakeSnapshot {
            domain: StakeDomain {
                fern_network_id: policy.network_id,
                ckb_genesis: policy.ckb_genesis,
                registry_id,
            },
            registry: RegistryAnchor {
                block: Anchor {
                    height,
                    hash: block_hash.as_bytes().try_into().expect("Hash32"),
                },
                sequence: registry.sequence(),
                commitment: registry.commitment(),
            },
            parent,
            target_epoch: request.target_epoch,
            guardian_generation: generation,
        };

        journal.pin(&l1, height, &block_hash)?;
        let signature = key.sign(&snapshot.signing_digest()).to_bytes();
        let native = self
            .fern_signing_anchor
            .map(|anchor| {
                ensure!(
                    snapshot.target_epoch != 0 && anchor != H256::default(),
                    "invalid native signing anchor"
                );
                let network = request.fern_network_id.pack();
                let body = fern_ledger::protocol::snapshot_body(&snapshot, &registry);
                let statement = fern_ledger::protocol::StakeSnapshot {
                    epoch: snapshot.target_epoch,
                    signing_anchor: anchor.pack(),
                    commitment: fern_ledger::protocol::snapshot_commitment(&network, &body),
                };
                let digest = fern_ledger::protocol::snapshot_digest(&network, &statement);
                Ok(NativeVote {
                    signing_anchor: anchor,
                    signature: JsonBytes::from_vec(key.sign(digest.as_slice()).to_bytes().to_vec()),
                })
            })
            .transpose()?;
        print_json(&json!({
            "fern_network_id": request.fern_network_id,
            "ckb_genesis": request.ckb_genesis,
            "registry_id": H256::from(registry_id),
            "registry": ObservedRegistry::from(snapshot.registry),
            "parent": request.parent,
            "target_epoch": snapshot.target_epoch,
            "guardian_generation": H256::from(generation),
            "registry_data": data,
            "attestation": { "guardian": guardian, "signature": JsonBytes::from_vec(signature.to_vec()) },
            "native": native,
        }))
    }
}

fn member(validator: &Validator) -> Result<Member> {
    Ok(Member {
        key: validator.ed25519.as_bytes().try_into()?,
        power: validator.power.try_into()?,
    })
}

fn live_cell(l1: &Rpc, point: &OutPoint, anchor_height: u64) -> Result<(CellOutput, JsonBytes)> {
    let status: CellWithStatus = l1.call("get_live_cell", json!([point, true]))?;
    ensure!(
        status.status == "live",
        "Registry or Stake Cell is not live"
    );
    let live = status.cell.context("missing L1 live Cell")?;
    let output: CellOutput = live.output.into();
    let data = live.data.context("missing L1 live Cell Data")?.content;
    let (raw, height, block_hash) = l1.committed(&point.tx_hash)?;
    ensure!(
        height <= anchor_height && l1.block_hash(height)? == block_hash,
        "Stake Cell is outside the confirmed L1 chain"
    );
    let tx = Transaction::from_slice(raw.as_bytes())?.into_view();
    let index = point.index.value() as usize;
    ensure!(
        tx.output(index).as_ref() == Some(&output)
            && tx
                .outputs_data()
                .get(index)
                .is_some_and(|value| value.raw_data().as_ref() == data.as_bytes()),
        "live Stake Cell differs from the committed transaction"
    );
    Ok((output, data))
}

pub(crate) fn native_genesis(
    trust_path: &std::path::Path,
    observations_path: &std::path::Path,
    signing_window: u64,
    limits: fern_ledger::protocol::Limits,
) -> Result<fern::native_store::StakeGenesis> {
    let trust: Trust = read_json_limited(trust_path, 256 * 1024)?;
    let votes: Vec<Observation> = read_json_limited(observations_path, 16 * 1024 * 1024)?;
    ensure!(
        (1..=MAX_RECORDS).contains(&votes.len())
            && trust.target_epoch == 0
            && trust.previous.is_none(),
        "invalid Stake Registry genesis observations"
    );
    let first = &votes[0];
    let statement = first.snapshot();
    ensure!(
        statement.domain
            == domain(
                &trust.fern_network_id,
                &trust.ckb_genesis,
                &trust.registry_id
            )
            && statement.target_epoch == 0
            && statement.parent.is_none(),
        "genesis observation identifies a different network or epoch"
    );
    let guardians = trust
        .guardians
        .iter()
        .map(member)
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        statement.guardian_generation
            == (GuardianProof {
                committee: &guardians,
                votes: &[]
            })
            .generation(),
        "genesis guardian generation differs from the trust file"
    );
    let mut signatures = votes
        .iter()
        .map(|observation| {
            ensure!(
                observation.snapshot() == statement
                    && observation.registry_data == first.registry_data,
                "guardians observed different L1 genesis snapshots"
            );
            Ok(Attestation {
                guardian: observation.attestation.guardian,
                signature: observation.attestation.signature.as_bytes().try_into()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    signatures.sort_unstable_by_key(|signature| signature.guardian);
    Ok(fern::native_store::StakeGenesis {
        config: StakeConfig {
            minimum_bond: trust.minimum_bond,
            shannons_per_power: trust.shannons_per_power,
            max_validators: trust.max_validators,
            max_total_power: trust.max_total_power,
            blocks_per_epoch: trust.blocks_per_epoch,
        },
        signing_window,
        limits,
        registry: Registry::decode(first.registry_data.as_bytes())?,
        statement,
        guardians,
        signatures,
    })
}

fn build_native_snapshot(observations: &std::path::Path) -> Result<()> {
    use fern_ledger::{
        Transaction,
        protocol::{Certificate, Operation, snapshot_body, snapshot_commitment},
    };
    let mut votes: Vec<Observation> = read_json_limited(observations, 16 * 1024 * 1024)?;
    ensure!(
        (1..=MAX_RECORDS).contains(&votes.len()),
        "invalid guardian observations"
    );
    votes.sort_unstable_by_key(|vote| vote.attestation.guardian);
    let first = &votes[0];
    let snapshot = first.snapshot();
    let anchor = &first
        .native
        .as_ref()
        .context("missing native Stake Snapshot signature")?
        .signing_anchor;
    let registry = Registry::decode(first.registry_data.as_bytes())?;
    let body = snapshot_body(&snapshot, &registry);
    let network = first.fern_network_id.pack();
    let statement = fern_ledger::protocol::StakeSnapshot {
        epoch: snapshot.target_epoch,
        signing_anchor: anchor.pack(),
        commitment: snapshot_commitment(&network, &body),
    };
    let signatures = votes
        .iter()
        .map(|observation| {
            let native = observation
                .native
                .as_ref()
                .context("missing native signature")?;
            ensure!(
                observation.snapshot() == snapshot
                    && observation.registry_data == first.registry_data
                    && &native.signing_anchor == anchor,
                "guardians observed different native Stake Snapshots"
            );
            Ok(Attestation {
                guardian: observation.attestation.guardian,
                signature: native.signature.as_bytes().try_into()?,
            })
        })
        .collect::<Result<_>>()?;
    let tx = Transaction::control(
        Operation::StakeSnapshot(statement),
        Certificate::new(body, signatures)?,
    )?;
    print_json(&JsonBytes::from_vec(tx.as_slice().to_vec()))
}
