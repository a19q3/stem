use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{JsonBytes, OutPoint, Script as JsonScript};
use ckb_types::{H256, packed::Script, prelude::*};
use clap::Args;
use ed25519_dalek::Signer;
use stem::native_store::Store;
use stem_ledger::{block::Header, patricia::Proof};
use stem_stake::{
    Authority, Registry,
    l1::{Policy, StakeCell, guardian_generation, retirement_cell, retirement_digest},
};
use serde::Deserialize;
use serde_json::json;

use super::{Guardian, Journal, Rpc, live_cell, member, print_json, read_json_limited, read_key};
use crate::native_spec;

#[derive(Args)]
pub struct AuthorizeRetirement {
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    l1_rpc: String,
    #[arg(long)]
    journal: PathBuf,
    #[arg(long)]
    key_file: PathBuf,
    #[arg(long)]
    genesis: PathBuf,
    #[arg(long)]
    comet_genesis: PathBuf,
    #[arg(long)]
    proof_verifier: PathBuf,
    #[arg(long)]
    header: PathBuf,
    #[arg(long)]
    signed_successor: PathBuf,
    #[arg(long)]
    handoffs: PathBuf,
    #[arg(long)]
    marker: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    registry_type: JsonScript,
    registry: OutPoint,
    stake: OutPoint,
    confirmations: u64,
    guardians: Vec<Guardian>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Retirement {
    height: u64,
    validator: H256,
    effective_height: u64,
    nodes: Vec<String>,
}

impl AuthorizeRetirement {
    pub(super) fn run(self) -> Result<()> {
        let request: Request = read_json_limited(&self.request, 64 * 1024)?;
        let marker: Retirement = read_json_limited(&self.marker, 64 * 1024)?;
        let spec = native_spec::load(&self.genesis)?;
        let genesis = spec
            .stake
            .as_ref()
            .context("Stem genesis has no Stake Registry")?;
        let domain = genesis.statement.domain;
        let registry_type: Script = request.registry_type.into();
        let registry_id = registry_type.calc_script_hash().unpack();
        let args = registry_type.args().raw_data();
        let policy = Policy::decode(registry_id, &args)?;
        ensure!(
            domain.registry_id == registry_id
                && domain.ckb_genesis == policy.ckb_genesis
                && domain.stem_network_id == policy.network_id
                && spec.chain_id == policy.chain_id,
            "requested CKB Stake Registry differs from the trusted Stem configuration"
        );

        let raw: JsonBytes = read_json_limited(&self.header, 4096)?;
        let header = Header::decode(raw.as_bytes())?;
        let validator = marker.validator.as_bytes().try_into()?;
        ensure!(
            marker.height == header.context.height
                && (1..=marker.height).contains(&marker.effective_height)
                && marker.nodes.len() <= 257,
            "retirement has not reached its authenticated effective height"
        );
        let nodes = marker
            .nodes
            .iter()
            .map(|node| {
                ensure!(node.len() <= 196, "oversize retirement proof node");
                Ok(hex::decode(node)?)
            })
            .collect::<Result<_>>()?;
        ensure!(
            Store::verify_stake_retirement(
                &spec.network,
                &header,
                registry_id,
                validator,
                marker.effective_height,
                &Proof { nodes }
            ),
            "invalid Stem retirement marker proof"
        );

        let genesis = native_spec::verify_comet_genesis(&spec, &self.comet_genesis)?;
        let result = Command::new(&self.proof_verifier)
            .arg("-genesis")
            .arg(&self.comet_genesis)
            .arg("-stem-genesis")
            .arg(hex::encode(genesis.as_slice()))
            .arg("-commit")
            .arg(&self.signed_successor)
            .arg("-handoffs")
            .arg(&self.handoffs)
            .arg("-height")
            .arg(marker.height.to_string())
            .arg("-app-hash")
            .arg(hex::encode(header.app_hash(&spec.network).as_slice()))
            .arg("-excluded-validator")
            .arg(hex::encode(validator))
            .output()?;
        ensure!(
            result.status.success(),
            "Stem retirement signed successor: {}",
            String::from_utf8_lossy(&result.stderr)
        );

        let l1 = Rpc::new(&self.l1_rpc)?;
        ensure!(
            l1.block_hash(0)?.as_bytes() == policy.ckb_genesis,
            "wrong CKB genesis"
        );
        let tip: ckb_jsonrpc_types::Uint64 = l1.call("get_tip_block_number", json!([]))?;
        ensure!(
            request.confirmations > 0,
            "retirement needs CKB confirmations"
        );
        let height = tip
            .value()
            .checked_sub(request.confirmations)
            .context("insufficient CKB confirmations")?;
        let block_hash = l1.block_hash(height)?;
        let mut lock_args = registry_id.to_vec();
        lock_args.push(0);
        let lock = registry_type
            .clone()
            .as_builder()
            .args(lock_args.pack())
            .build();
        let (output, data) = live_cell(&l1, &request.registry, height)?;
        ensure!(
            output.type_().to_opt().as_ref() == Some(&registry_type) && output.lock() == lock,
            "incorrect confirmed CKB Stake Registry scripts"
        );
        let prior = Registry::decode(data.as_bytes())?;
        ensure!(
            prior.network_id() == policy.network_id,
            "incorrect Stake Registry network"
        );

        let mut stake_args = registry_id.to_vec();
        stake_args.extend_from_slice(&validator);
        let stake_type = registry_type.as_builder().args(stake_args.pack()).build();
        let (output, data) = live_cell(&l1, &request.stake, height)?;
        ensure!(
            output.type_().to_opt().as_ref() == Some(&stake_type) && output.lock() == lock,
            "incorrect confirmed CKB Stake Cell scripts"
        );
        let mut records = prior.records().to_vec();
        let index = records
            .binary_search_by_key(&validator, |record| record.validator_pubkey)
            .ok()
            .context("validator is missing from the CKB Stake Registry")?;
        let record = &mut records[index];
        let occupied = output
            .occupied_capacity(ckb_types::core::Capacity::bytes(data.len())?)?
            .as_u64();
        ensure!(
            occupied.checked_add(record.bonded_amount) == Some(output.capacity().unpack()),
            "Stake Cell Capacity differs from the registered bond"
        );
        let cell = StakeCell::verify_record(record, data.as_bytes())?;
        let retired = retirement_cell(&policy, record, cell, marker.effective_height.try_into()?)?;
        record.first_inactive_height = retired.first_inactive_height;
        record.stake_cell_hash = retired.commitment();
        let mut stake_data = data.as_bytes()[..24].to_vec();
        stake_data.extend_from_slice(&retired.encode());
        let sequence = prior
            .sequence()
            .checked_add(1)
            .context("Stake Registry sequence overflow")?;
        let next = Registry::new(
            prior.network_id(),
            sequence,
            prior.guardian_generation(),
            prior.retired_root(),
            records,
        )?;

        let guardians = request
            .guardians
            .iter()
            .map(member)
            .collect::<Result<Vec<_>>>()?;
        let generation = guardian_generation(&guardians);
        ensure!(
            generation == prior.guardian_generation(),
            "guardian authority does not match the current CKB Registry"
        );
        Authority::new(generation, &guardians)?;
        let key = read_key(&self.key_file)?;
        let guardian = guardians
            .binary_search_by_key(&key.verifying_key().to_bytes(), |member| member.key)
            .ok()
            .context("signing key is outside the current guardian authority")?;
        let journal_key = ckb_hash::blake2b_256(
            [
                b"Stem guardian Stake Retirement Journal\0".as_slice(),
                &domain.stem_network_id,
                &registry_id,
            ]
            .concat(),
        );
        let journal = Journal::open(&self.journal, journal_key.pack())?;
        ensure!(
            journal.validate(&l1)?.is_none_or(|pinned| height >= pinned),
            "confirmed CKB anchor regressed"
        );
        journal.pin(&l1, height, &block_hash)?;
        let signature = key
            .sign(&retirement_digest(&policy, &prior, &next))
            .to_bytes();
        print_json(&json!({
            "registry_data": JsonBytes::from_vec(next.encode()),
            "stake_data": JsonBytes::from_vec(stake_data),
            "guardian": guardian,
            "signature": JsonBytes::from_vec(signature.to_vec()),
        }))
    }
}
