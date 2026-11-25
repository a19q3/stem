use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::JsonBytes;
use ckb_types::H256;
use clap::Args;
use serde::Deserialize;
use stem::native_store::Store;
use stem_ledger::{block::Header, patricia::Proof};

use super::{print_json, read_json_limited};
use crate::{bridge_cli::parse_hash, native_spec};

#[derive(Args)]
pub struct RetirementWitness {
    #[arg(long)]
    genesis: PathBuf,
    #[arg(long, value_parser = parse_hash)]
    validator: H256,
    /// JSON hex for the native Header authenticated by the pinned CKB checkpoint.
    #[arg(long)]
    header: PathBuf,
    /// JSON response value of the /stem/stake-retirement query.
    #[arg(long)]
    marker: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    effective_height: u64,
    proof: JsonBytes,
}

impl RetirementWitness {
    pub(super) fn run(self) -> Result<()> {
        let spec = native_spec::load(&self.genesis)?;
        let staking = spec
            .stake
            .as_ref()
            .context("Stem genesis has no Stake Registry")?;
        let raw: JsonBytes = read_json_limited(&self.header, 1024)?;
        let header = Header::decode(raw.as_bytes())?;
        let marker: Marker = read_json_limited(&self.marker, 64 * 1024)?;
        let proof = Proof::decode(marker.proof.as_bytes())?;
        let validator = self.validator.as_bytes().try_into()?;
        ensure!(
            Store::verify_stake_retirement(
                &spec.network,
                &header,
                staking.statement.domain.registry_id,
                validator,
                marker.effective_height,
                &proof,
            ),
            "invalid or not yet effective Stem retirement marker",
        );
        print_json(&marker.proof)
    }
}
