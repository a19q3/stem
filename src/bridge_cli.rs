use std::{
    fs::File,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{JsonBytes, Script as JsonScript};
use ckb_types::{H256, packed::Script, prelude::*};
use clap::Subcommand;
use ed25519_dalek::SigningKey;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;

use crate::{
    bridge::{self, Certificate, SignedObservation},
    genesis::MAX_TX_BYTES,
    l1::{Journal, Rpc},
    read_genesis,
};

#[derive(Subcommand)]
pub enum BridgeCommand {
    /// Pack the weighted membership for a guardian governance Cell.
    GuardianData {
        #[arg(long)]
        members: PathBuf,
    },
    /// Encode an L1 deposit lock and sidechain recipient memo.
    DepositCell {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        recipient: PathBuf,
        /// Amount in base units of the configured L1 xUDT.
        #[arg(long, value_name = "AMOUNT")]
        units: Option<u128>,
    },
    /// Encode the L1 escrow lock and empty xUDT carrier.
    EscrowCell {
        #[arg(long)]
        genesis: PathBuf,
    },
    /// Read a confirmed consolidation from an L1 node and attest the deposit.
    Attest {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        l1_rpc: String,
        /// The guardian's own synchronized sidechain replica for a rotatable bridge.
        #[arg(long)]
        sidechain_rpc: Option<String>,
        #[arg(long)]
        deposit_tx: String,
        #[arg(long)]
        output_index: u32,
        #[arg(long)]
        consolidation_tx: String,
        /// Private file containing a 32-byte Ed25519 seed.
        #[arg(long)]
        key_file: PathBuf,
        /// Dedicated private directory holding the last trusted L1 anchor.
        #[arg(long)]
        journal: PathBuf,
    },
    /// Check that a guardian's prior L1 confirmation anchor is still canonical.
    CheckAnchor {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        l1_rpc: String,
        #[arg(long)]
        journal: PathBuf,
    },
    /// Combine guardian observations into a packed sidechain peg-in transaction.
    Mint {
        #[arg(long)]
        genesis: PathBuf,
        /// Submit the mint and wait for execution on the selected sidechain replica.
        #[arg(long)]
        submit_rpc: Option<String>,
        /// Synchronized sidechain replica to read the active guardian configuration.
        #[arg(long)]
        sidechain_rpc: Option<String>,
        #[arg(required = true)]
        votes: Vec<PathBuf>,
    },
    /// Encode the lock and data for one sidechain withdrawal output.
    BurnCell {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        l1_recipient: PathBuf,
        /// Amount in base units of the configured wrapped xUDT.
        #[arg(long, value_name = "AMOUNT")]
        units: Option<u128>,
    },
    /// Check one unsigned L1 release against an executed burn and the live vault.
    CheckPayout(crate::payout::CheckPayout),
    /// Build an unsigned L1 release from an executed burn and committed L1 Cells.
    PreparePayout(crate::payout::PreparePayout),
    /// Build an unsigned L1 migration to a reviewed CKB multisig configuration.
    PrepareVaultRotation(crate::vault::RotationRequest),
    /// Check the exact unsigned L1 custody migration before signing it.
    CheckVaultRotation(crate::vault::CheckRotation),
}

impl BridgeCommand {
    pub fn run(self) -> Result<()> {
        match self {
            Self::GuardianData { members } => {
                let members = read_json::<Vec<crate::genesis::Validator>>(&members)?;
                print_json(&JsonBytes::from_vec(crate::governance::encode_guardians(
                    &members,
                )?))
            }
            Self::DepositCell {
                genesis,
                recipient,
                units,
            } => {
                let spec = read_genesis(&genesis)?;
                let anchor = spec.validate()?.0.hash();
                let config = spec.bridge.context("bridge is disabled")?;
                let recipient: Script = read_json::<JsonScript>(&recipient)?.into();
                if let Some(units) = units {
                    ensure!(units > 0, "xUDT deposit amount must be positive");
                    let lock: JsonScript = config.xudt_deposit_lock(&anchor, &recipient)?.into();
                    let asset = config.xudt.context("xUDT bridge is disabled")?;
                    return print_json(&json!({
                        "lock": lock, "type": asset.l1_type, "data": JsonBytes::from_vec(units.to_le_bytes().to_vec())
                    }));
                }
                let memo = bridge::deposit_memo(&anchor, &recipient);
                print_json(
                    &json!({"lock":config.deposit_lock, "type":null, "data":JsonBytes::from_bytes(memo)}),
                )
            }
            Self::EscrowCell { genesis } => {
                let spec = read_genesis(&genesis)?;
                spec.validate()?;
                let config = spec.bridge.context("bridge is disabled")?;
                let lock: JsonScript = config.xudt_escrow_lock()?.into();
                let asset = config.xudt.context("xUDT bridge is disabled")?;
                print_json(&json!({
                    "lock": lock, "type": asset.l1_type,
                    "data": JsonBytes::from_vec(0u128.to_le_bytes().to_vec())
                }))
            }
            Self::Attest {
                genesis,
                l1_rpc,
                sidechain_rpc,
                deposit_tx,
                output_index,
                consolidation_tx,
                key_file,
                journal,
            } => {
                let spec = read_genesis(&genesis)?;
                let anchor = spec.validate()?.0.hash();
                let config = spec.bridge.as_ref().context("bridge is disabled")?;
                let sidechain = sidechain_rpc
                    .as_deref()
                    .map(crate::comet_rpc::Rpc::new)
                    .transpose()?;
                let guardians = active_guardians(&spec, config, &anchor, sidechain.as_ref())?;
                let rpc = Rpc::new(&l1_rpc)?;
                ensure!(
                    rpc.block_hash(0)? == config.l1_genesis,
                    "incorrect L1 genesis"
                );
                let journal = Journal::open(&journal, anchor.clone())?;
                journal.validate(&rpc)?;
                if let Some(backing) = &guardians.backing_anchor {
                    journal
                        .pin(&rpc, backing.height, &backing.hash)
                        .context("guardian halted: committed backing anchor does not match L1")?;
                }
                let deposit = rpc.observe_deposit(
                    config,
                    &hash(deposit_tx)?,
                    output_index,
                    &hash(consolidation_tx)?,
                )?;
                let attestation =
                    config.attest(&anchor, &deposit, &read_key(&key_file)?, &guardians)?;
                journal.advance(&rpc, &deposit)?;
                print_json(&SignedObservation {
                    deposit,
                    attestation,
                    generation: guardians.generation,
                    previous_backing_anchor: guardians.backing_anchor,
                })
            }
            Self::CheckAnchor {
                genesis,
                l1_rpc,
                journal,
            } => {
                let spec = read_genesis(&genesis)?;
                let anchor = spec.validate()?.0.hash();
                let config = spec.bridge.context("bridge is disabled")?;
                let rpc = Rpc::new(&l1_rpc)?;
                ensure!(
                    rpc.block_hash(0)? == config.l1_genesis,
                    "incorrect L1 genesis"
                );
                let height = Journal::open(&journal, anchor)?
                    .validate(&rpc)?
                    .context("no attested L1 anchor in guardian journal")?;
                print_json(&json!({"height":height}))
            }
            Self::Mint {
                genesis,
                submit_rpc,
                sidechain_rpc,
                votes,
            } => {
                let spec = read_genesis(&genesis)?;
                let anchor = spec.validate()?.0.hash();
                let config = spec.bridge.as_ref().context("bridge is disabled")?;
                let sidechain = sidechain_rpc
                    .as_deref()
                    .or(submit_rpc.as_deref())
                    .map(crate::comet_rpc::Rpc::new)
                    .transpose()?;
                let guardians = active_guardians(&spec, config, &anchor, sidechain.as_ref())?;
                let mut votes = votes.iter();
                let first: SignedObservation =
                    read_json(votes.next().context("missing guardian votes")?)?;
                let mut certificate = Certificate {
                    deposit: first.deposit,
                    attestations: vec![first.attestation],
                    generation: first.generation,
                    previous_backing_anchor: first.previous_backing_anchor,
                };
                for path in votes {
                    let vote: SignedObservation = read_json(path)?;
                    ensure!(
                        vote.deposit == certificate.deposit,
                        "votes describe different L1 confirmations"
                    );
                    ensure!(
                        vote.generation == certificate.generation,
                        "votes name different guardian generations"
                    );
                    ensure!(
                        vote.previous_backing_anchor == certificate.previous_backing_anchor,
                        "votes name different L1 backing anchors"
                    );
                    certificate.attestations.push(vote.attestation);
                }
                certificate
                    .attestations
                    .sort_unstable_by_key(|vote| vote.guardian);
                if !certificate.uses_current_authority(&guardians)
                    && spec.guardian_governance.is_some()
                {
                    let payload = config.mint_payload(&anchor, &certificate.deposit)?;
                    if let Some((block, tx)) = sidechain
                        .as_ref()
                        .context("guardian policy requires a sidechain replica")?
                        .committed_mint(&payload)?
                    {
                        return print_json(&json!({
                            "transaction": JsonBytes::from_bytes(tx.data().as_bytes()),
                            "ckb_tx_hash": format!("{:#x}", tx.hash()),
                            "execution": {"block_hash": format!("{block:#x}"), "already_finalized": true},
                        }));
                    }
                }
                let tx = config.build_mint(&anchor, &certificate, &guardians)?;
                ensure!(
                    tx.data().as_slice().len() <= MAX_TX_BYTES,
                    "peg-in exceeds transaction limit"
                );
                let mut result = json!({
                    "transaction": JsonBytes::from_bytes(tx.data().as_bytes()),
                    "ckb_tx_hash": format!("{:#x}",tx.hash()),
                });
                if let Some(url) = submit_rpc {
                    result["execution"] =
                        crate::comet_rpc::Rpc::new(&url)?.submit_mint(&anchor, &tx)?;
                }
                print_json(&result)
            }
            Self::BurnCell {
                genesis,
                l1_recipient,
                units,
            } => {
                let spec = read_genesis(&genesis)?;
                let anchor = spec.validate()?.0.hash();
                let config = spec.bridge.context("bridge is disabled")?;
                let recipient: Script = read_json::<JsonScript>(&l1_recipient)?.into();
                config.validate_payout_lock(&recipient)?;
                if let Some(units) = units {
                    ensure!(units > 0, "xUDT burn amount must be positive");
                    let asset = config.xudt.context("xUDT bridge is disabled")?;
                    let lock: JsonScript = bridge::xudt_burn_lock(&anchor, &recipient).into();
                    let wrapped: JsonScript = asset.sidechain_type().into();
                    return print_json(&json!({
                        "lock": lock, "type": wrapped, "data": JsonBytes::from_vec(units.to_le_bytes().to_vec())
                    }));
                }
                let lock: JsonScript = bridge::burn_lock(&anchor).into();
                print_json(
                    &json!({"lock":lock, "type":null, "data":JsonBytes::from_bytes(bridge::payout_memo(&recipient))}),
                )
            }
            Self::CheckPayout(command) => command.run(),
            Self::PreparePayout(command) => command.run(),
            Self::PrepareVaultRotation(command) => command.run(),
            Self::CheckVaultRotation(command) => command.run(),
        }
    }
}

fn active_guardians(
    spec: &crate::genesis::Genesis,
    config: &bridge::Bridge,
    anchor: &ckb_types::packed::Byte32,
    rpc: Option<&crate::comet_rpc::Rpc>,
) -> Result<bridge::Guardians> {
    if spec.guardian_governance.is_none() {
        return Ok(bridge::Guardians::fixed(config));
    }
    let guardians = rpc
        .context("rotatable guardians require --sidechain-rpc or --submit-rpc")?
        .guardians(anchor)?;
    ensure!(
        guardians.generation.is_some(),
        "replica did not return a guardian governance generation"
    );
    crate::genesis::validate_committee(&guardians.members)?;
    Ok(guardians)
}

pub fn hash(value: String) -> Result<H256> {
    Ok(serde_json::from_value(value.into())?)
}

pub(crate) fn read_key(path: &Path) -> Result<SigningKey> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "bridge key file must be private"
    );
    let mut key = Vec::new();
    file.take(33).read_to_end(&mut key)?;
    Ok(SigningKey::try_from(key.as_slice())?)
}

pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    read_json_limited(path, MAX_TX_BYTES)
}

pub fn read_json_limited<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<T> {
    let mut raw = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut raw)?;
    ensure!(raw.len() <= limit, "JSON input file exceeds size limit");
    Ok(serde_json::from_slice(&raw)?)
}

pub fn print_json(value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(std::io::stdout().lock(), value)?;
    Ok(())
}
