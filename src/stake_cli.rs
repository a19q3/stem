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
use stem_ledger::protocol::ARRIVING_GUARDIAN;
use stem_stake::{
    AcceptedSnapshot, Anchor, Attestation, Authority, MAX_RECORDS, Member, Registry,
    RegistryAnchor, StakeConfig, StakeDomain, StakeSnapshot,
    l1::{GuardianProof, Policy, StakeCell, guardian_generation},
};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{
    bridge_cli::{print_json, read_json_limited, read_key},
    l1::{Journal, Rpc},
};

mod retirement;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Guardian {
    ed25519: JsonBytes,
    power: i64,
}

#[derive(Subcommand)]
pub enum StakeCommand {
    /// Verify every registry-backed live Stake Cell on a trusted full CKB node.
    Observe(Observe),
    /// Authenticate a threshold-certified snapshot against a pinned network and guardian set.
    Verify(Verify),
    /// Verify finalized Stem exclusion and authorize CKB Stake Cell retirement.
    AuthorizeRetirement(Box<retirement::AuthorizeRetirement>),
    /// Build the compact absence proof for a registration or withdrawal.
    RetirementProof {
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        network: H256,
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        validator: H256,
        #[arg(long)]
        retired: PathBuf,
    },
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
    /// Raw Stem Header ID at the scheduled signing height.
    #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
    stem_signing_anchor: Option<H256>,
    /// Sign a confirmed rotation as a member of the departing authority.
    #[arg(long, requires = "stem_signing_anchor")]
    departing: bool,
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
    stem_network_id: H256,
    ckb_genesis: H256,
    registry_id: H256,
    target_epoch: u64,
    previous: Option<Previous>,
    guardians: Vec<Guardian>,
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
    stem_network_id: H256,
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
    guardian: u16,
    arriving: Vec<Guardian>,
    signature: JsonBytes,
}

impl Observation {
    fn snapshot(&self) -> StakeSnapshot {
        StakeSnapshot {
            domain: domain(&self.stem_network_id, &self.ckb_genesis, &self.registry_id),
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

fn domain(stem_network_id: &H256, ckb_genesis: &H256, registry_id: &H256) -> StakeDomain {
    StakeDomain {
        stem_network_id: stem_network_id.as_bytes().try_into().expect("Hash32"),
        ckb_genesis: ckb_genesis.as_bytes().try_into().expect("Hash32"),
        registry_id: registry_id.as_bytes().try_into().expect("Hash32"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    stem_network_id: H256,
    ckb_genesis: H256,
    registry_type: JsonScript,
    registry: OutPoint,
    stake_cells: Vec<OutPoint>,
    confirmations: u64,
    anchor_height: Option<u64>,
    target_epoch: u64,
    parent: Option<ObservedRegistry>,
    guardians: Vec<Guardian>,
    rotation: Option<Rotation>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rotation {
    previous_registry: OutPoint,
    departing_guardians: Vec<Guardian>,
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
            Self::AuthorizeRetirement(retirement) => retirement.run(),
            Self::RetirementProof {
                network,
                validator,
                retired,
            } => {
                use stem_stake::retirement::{RetirementTree, insert};
                let network = network.as_bytes().try_into()?;
                let validator = validator.as_bytes().try_into()?;
                let mut tree = RetirementTree::new(network);
                let keys: Vec<H256> = read_json_limited(&retired, 16 * 1024 * 1024)?;
                for key in keys {
                    tree.retire(key.as_bytes().try_into()?)?;
                }
                let root = tree.root();
                let proof = tree.prove_absence(&validator)?;
                let next = insert(&network, &validator, &root, &proof)?;
                print_json(&json!({"root": H256::from(root), "next": H256::from(next),
                                  "proof": JsonBytes::from_vec(proof)}))
            }
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
                &trust.stem_network_id,
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
            policy.network_id.as_slice() == request.stem_network_id.as_bytes()
                && policy.ckb_genesis.as_slice() == request.ckb_genesis.as_bytes(),
            "Stake Registry names a different network"
        );
        let l1 = Rpc::new(&self.l1_rpc)?;
        ensure!(
            l1.block_hash(0)? == request.ckb_genesis,
            "incorrect L1 genesis"
        );

        let mut hasher = new_blake2b();
        hasher.update(b"Stem guardian Stake Journal\0");
        hasher.update(&policy.network_id);
        hasher.update(&registry_id);
        let mut id = [0; 32];
        hasher.finalize(&mut id);
        let journal = Journal::open(&self.journal, id.pack())?;
        let previous_height = journal.validate(&l1)?;
        let tip: Uint64 = l1.call("get_tip_block_number", json!([]))?;
        let confirmed_height = tip
            .value()
            .checked_sub(request.confirmations)
            .context("L1 chain has insufficient confirmations")?;
        let height = request.anchor_height.unwrap_or(confirmed_height);
        ensure!(
            height <= confirmed_height && previous_height.is_none_or(|previous| height >= previous),
            "requested L1 anchor is outside the confirmed signing window"
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

        let live_records = registry.records();
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
        let generation = guardian_generation(&guardians);
        ensure!(
            generation == registry.guardian_generation(),
            "guardian committee does not match the live Registry"
        );
        Authority::new(generation, &guardians)?;
        let leaving = if let Some(rotation) = &request.rotation {
            ensure!(
                self.stem_signing_anchor.is_some() && request.target_epoch != 0,
                "guardian rotation requires a native Stake Snapshot"
            );
            let parent = parent
                .as_ref()
                .context("rotation requires the prior accepted Registry")?;
            let (output, previous) =
                confirmed_cell(&l1, &rotation.previous_registry, parent.block.height)?;
            ensure!(
                output.type_().to_opt().as_ref() == Some(&registry_type)
                    && output.lock() == expected_lock,
                "prior guardian authority is not from the expected L1 Registry"
            );
            let previous = Registry::decode(previous.as_bytes())?;
            let members = rotation
                .departing_guardians
                .iter()
                .map(member)
                .collect::<Result<Vec<_>>>()?;
            let departing = guardian_generation(&members);
            ensure!(
                previous.sequence() == parent.sequence
                    && previous.commitment() == parent.commitment
                    && previous.guardian_generation() == departing
                    && departing != generation,
                "guardian rotation does not extend the accepted L1 Registry"
            );
            Authority::new(departing, &members)?;
            Some(members)
        } else {
            None
        };
        let key = read_key(&self.key_file)?;
        let signers = if self.departing {
            leaving
                .as_ref()
                .context("missing departing guardian committee")?
        } else {
            &guardians
        };
        let guardian = signers
            .binary_search_by_key(&key.verifying_key().to_bytes(), |member| member.key)
            .map_err(|_| {
                anyhow::anyhow!("signing key is outside the requested guardian authority")
            })?;
        let guardian = u16::try_from(guardian)?;
        let arriving = if leaving.is_some() {
            guardians.as_slice()
        } else {
            &[]
        };
        let native_guardian = if leaving.is_some() && !self.departing {
            guardian | ARRIVING_GUARDIAN
        } else {
            guardian
        };
        let snapshot = StakeSnapshot {
            domain: StakeDomain {
                stem_network_id: policy.network_id,
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

        let signature = key.sign(&snapshot.signing_digest()).to_bytes();
        let native = self
            .stem_signing_anchor
            .map(|anchor| {
                ensure!(
                    snapshot.target_epoch != 0 && anchor != H256::default(),
                    "invalid native signing anchor"
                );
                let network = request.stem_network_id.pack();
                let body = stem_ledger::protocol::snapshot_body(&snapshot, &registry, arriving)?;
                let statement = stem_ledger::protocol::StakeSnapshot {
                    epoch: snapshot.target_epoch,
                    signing_anchor: anchor.pack(),
                    commitment: stem_ledger::protocol::snapshot_commitment(&network, &body),
                };
                let digest = stem_ledger::protocol::snapshot_digest(&network, &statement);
                journal.pin_stake_snapshot(
                    &l1,
                    height,
                    &block_hash,
                    snapshot.target_epoch,
                    &digest,
                )?;
                Ok(NativeVote {
                    signing_anchor: anchor,
                    guardian: native_guardian,
                    arriving: if arriving.is_empty() {
                        vec![]
                    } else {
                        request.guardians.clone()
                    },
                    signature: JsonBytes::from_vec(key.sign(digest.as_slice()).to_bytes().to_vec()),
                })
            })
            .transpose()?;
        if native.is_none() {
            journal.pin(&l1, height, &block_hash)?;
        }
        print_json(&json!({
            "stem_network_id": request.stem_network_id,
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

fn member(validator: &Guardian) -> Result<Member> {
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
    let (committed, expected) = confirmed_cell(l1, point, anchor_height)?;
    ensure!(
        committed == output && expected == data,
        "live Stake Cell differs from the committed transaction"
    );
    Ok((output, data))
}

fn confirmed_cell(
    l1: &Rpc,
    point: &OutPoint,
    anchor_height: u64,
) -> Result<(CellOutput, JsonBytes)> {
    let (raw, height, block_hash) = l1.committed(&point.tx_hash)?;
    ensure!(
        height <= anchor_height && l1.block_hash(height)? == block_hash,
        "Stake Cell is outside the confirmed L1 chain"
    );
    let tx = Transaction::from_slice(raw.as_bytes())?.into_view();
    let index = point.index.value() as usize;
    let output = tx.output(index).context("missing Stake Cell output")?;
    let data = tx
        .outputs_data()
        .get(index)
        .context("missing Stake Cell Data")?;
    Ok((output, JsonBytes::from_bytes(data.raw_data())))
}

pub(crate) fn native_genesis(
    trust_path: &std::path::Path,
    observations_path: &std::path::Path,
    signing_window: u64,
    limits: stem_ledger::protocol::Limits,
) -> Result<stem::native_store::StakeGenesis> {
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
                &trust.stem_network_id,
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
    Ok(stem::native_store::StakeGenesis {
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
    use stem_ledger::{
        Transaction,
        protocol::{Certificate, Operation, snapshot_body, snapshot_commitment},
    };
    let mut votes: Vec<Observation> = read_json_limited(observations, 16 * 1024 * 1024)?;
    ensure!(
        (1..=2 * MAX_RECORDS).contains(&votes.len()),
        "invalid guardian observations"
    );
    votes.sort_unstable_by_key(|vote| vote.native.as_ref().map(|vote| vote.guardian));
    let first = &votes[0];
    let snapshot = first.snapshot();
    let reference = first
        .native
        .as_ref()
        .context("missing native Stake Snapshot signature")?;
    let anchor = &reference.signing_anchor;
    let arriving = reference
        .arriving
        .iter()
        .map(member)
        .collect::<Result<Vec<_>>>()?;
    let registry = Registry::decode(first.registry_data.as_bytes())?;
    let body = snapshot_body(&snapshot, &registry, &arriving)?;
    let network = first.stem_network_id.pack();
    let statement = stem_ledger::protocol::StakeSnapshot {
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
                    && &native.signing_anchor == anchor
                    && native.arriving == reference.arriving,
                "guardians observed different native Stake Snapshots"
            );
            Ok(Attestation {
                guardian: native.guardian,
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
