//! JSON input for the native network specification.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use ckb_jsonrpc_types::{JsonBytes, OutPoint as JsonOutPoint, Script as JsonScript};
use ckb_types::{H256, core::ScriptHashType, packed::Script, prelude::*};
use serde::Deserialize;
use stem::native_store::{BridgeGenesis, LaneGenesis, Limits, RecipientLock, ResourceFees, Spec};
use stem_ledger::{
    CapacityPolicy, Cell,
    protocol::{L1Anchor, Limits as ProtocolLimits},
};
use stem_stake::Member;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Network {
    chain_id: String,
    network_id: H256,
    genesis_time_ms: u64,
    capacity: StorageBond,
    limits: Limits,
    #[serde(default)]
    fees: ResourceFees,
    #[serde(default)]
    development_cells: Vec<Allocation>,
    #[serde(default)]
    development_validators: Vec<Voter>,
    stake: Option<Stake>,
    bridge: Option<Bridge>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StorageBond {
    bond_per_byte: u64,
    state_overhead: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Allocation {
    out_point: JsonOutPoint,
    cell: JsonBytes,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Voter {
    ed25519: H256,
    power: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Stake {
    trust: PathBuf,
    observations: PathBuf,
    signing_window: u64,
    limits: OperationLimits,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bridge {
    l1_genesis: H256,
    confirmations: u64,
    anchor_height: u64,
    anchor_hash: H256,
    guardian_generation: H256,
    guardians: Vec<Voter>,
    wrapped_code_hash: H256,
    burn_code_hash: H256,
    lanes: Vec<Lane>,
    recipient_locks: Vec<LockTemplate>,
    reserved_recipient_locks: Vec<JsonScript>,
    mint_limits: OperationLimits,
    settlement_limits: OperationLimits,
    deposits_per_block: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lane {
    vault_type_hash: H256,
    vault: JsonOutPoint,
    l1_type: Option<JsonScript>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LockTemplate {
    example: JsonScript,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperationLimits {
    operations: u32,
    bytes: u64,
    signatures: u32,
}

impl From<OperationLimits> for ProtocolLimits {
    fn from(limit: OperationLimits) -> Self {
        Self {
            operations: limit.operations,
            bytes: limit.bytes,
            signatures: limit.signatures,
        }
    }
}

pub fn load(path: &Path) -> Result<Spec> {
    let config: Network = crate::bridge_cli::read_json_limited(path, 64 * 1024 * 1024)
        .with_context(|| format!("load Stem genesis {}", path.display()))?;
    let root = path.parent().unwrap_or_else(|| Path::new("."));
    let stake = config
        .stake
        .map(|spec| {
            crate::stake_cli::native_genesis(
                &root.join(spec.trust),
                &root.join(spec.observations),
                spec.signing_window,
                spec.limits.into(),
            )
        })
        .transpose()?;
    let bridge = config.bridge.map(load_bridge).transpose()?;
    let development_cells = config
        .development_cells
        .into_iter()
        .map(|allocation| {
            Ok((
                allocation.out_point.into(),
                Cell::from_slice(allocation.cell.as_bytes())?,
            ))
        })
        .collect::<Result<_>>()?;
    let development_validators = config
        .development_validators
        .into_iter()
        .map(|voter| {
            Ok((
                voter.ed25519.as_bytes().try_into()?,
                i64::try_from(voter.power)?,
            ))
        })
        .collect::<Result<_>>()?;
    Ok(Spec {
        chain_id: config.chain_id,
        network: config.network_id.pack(),
        genesis_time_ms: config.genesis_time_ms,
        capacity: CapacityPolicy {
            bond_per_byte: config.capacity.bond_per_byte,
            state_overhead: config.capacity.state_overhead,
        },
        limits: config.limits,
        fees: config.fees,
        development_cells,
        development_validators,
        stake,
        bridge,
    })
}

fn load_bridge(bridge: Bridge) -> Result<BridgeGenesis> {
    let guardians = bridge
        .guardians
        .into_iter()
        .map(|voter| {
            Ok(Member {
                key: voter.ed25519.as_bytes().try_into()?,
                power: voter.power,
            })
        })
        .collect::<Result<_>>()?;
    let lanes = bridge
        .lanes
        .into_iter()
        .map(|lane| LaneGenesis {
            vault_type_hash: lane.vault_type_hash.pack(),
            vault: lane.vault.into(),
            l1_type: lane.l1_type.map(Into::into),
        })
        .collect();
    let recipient_locks = bridge
        .recipient_locks
        .into_iter()
        .map(|template| {
            let script: Script = template.example.into();
            Ok(RecipientLock {
                code_hash: script.code_hash(),
                hash_type: ScriptHashType::try_from(script.hash_type())?,
                args_len: u32::try_from(script.args().len())?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(BridgeGenesis {
        l1_genesis: bridge.l1_genesis.pack(),
        confirmations: bridge.confirmations,
        anchor: L1Anchor {
            height: bridge.anchor_height,
            hash: bridge.anchor_hash.pack(),
        },
        guardian_generation: bridge.guardian_generation.pack(),
        wrapped_code_hash: bridge.wrapped_code_hash.pack(),
        burn_code_hash: bridge.burn_code_hash.pack(),
        guardians,
        lanes,
        recipient_locks,
        reserved_recipient_locks: bridge
            .reserved_recipient_locks
            .into_iter()
            .map(Into::into)
            .collect(),
        mint_limits: bridge.mint_limits.into(),
        settlement_limits: bridge.settlement_limits.into(),
        deposits_per_block: bridge.deposits_per_block,
    })
}

pub(crate) fn verify_comet_genesis(spec: &Spec, path: &Path) -> Result<ckb_types::packed::Byte32> {
    use anyhow::ensure;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use stem::native_store::{Retention, Store};

    let genesis: serde_json::Value = crate::bridge_cli::read_json_limited(path, 2 * 1024 * 1024)?;
    let directory = tempfile::tempdir()?;
    let state = Store::open(directory.path(), spec, Retention::Archive)?;
    let stated = genesis["app_state"]["stem_genesis"]
        .as_str()
        .context("missing CometBFT application genesis")?;
    ensure!(
        crate::bridge_cli::parse_hash(stated)?.as_bytes() == state.genesis_id().as_slice()
            && genesis["chain_id"].as_str() == Some(spec.chain_id.as_str())
            && (genesis["initial_height"].is_null()
                || matches!(genesis["initial_height"].as_str(), Some("0" | "1"))),
        "CometBFT genesis differs from the native specification"
    );
    let mut voters = genesis["validators"]
        .as_array()
        .context("missing CometBFT genesis committee")?
        .iter()
        .map(|voter| {
            ensure!(
                voter["pub_key"]["type"].as_str() == Some("tendermint/PubKeyEd25519"),
                "CometBFT genesis requires Ed25519 public keys"
            );
            let key = STANDARD.decode(
                voter["pub_key"]["value"]
                    .as_str()
                    .context("missing CometBFT public key")?,
            )?;
            let power = voter["power"]
                .as_str()
                .context("invalid CometBFT voting power")?
                .parse()?;
            Ok((key.as_slice().try_into()?, power))
        })
        .collect::<Result<Vec<([u8; 32], i64)>>>()?;
    voters.sort_unstable();
    ensure!(
        voters == state.genesis_validators(),
        "CometBFT genesis committee differs from the native specification"
    );
    Ok(state.genesis_id().clone())
}
