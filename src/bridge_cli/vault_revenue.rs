use std::path::PathBuf;

use anyhow::{Result, ensure};
use ckb_jsonrpc_types::{JsonBytes, Uint64};
use ckb_types::{H256, prelude::*};
use clap::Args;
use serde::Deserialize;
use serde_json::json;
use stem::native_store::Store;
use stem_ledger::{block::Header, patricia::Proof, protocol::VaultRevenue};

use super::{parse_hash, print_json, read_json_limited};

#[derive(Args)]
pub struct Verify {
    #[arg(long, value_parser = parse_hash)]
    network: H256,
    /// JSON hex execution Header independently authenticated by the caller.
    #[arg(long)]
    header: PathBuf,
    /// Decoded JSON value of the /stem/vault-revenue proof query.
    #[arg(long)]
    accounting: PathBuf,
}

#[derive(Deserialize)]
struct Accounting {
    record: JsonBytes,
    proof: JsonBytes,
    extinguished_shannons: Option<Uint64>,
    fee_proof: JsonBytes,
}

impl Verify {
    pub(super) fn run(self) -> Result<()> {
        let raw: JsonBytes = read_json_limited(&self.header, 4096)?;
        let header = Header::decode(raw.as_bytes())?;
        let accounting: Accounting = read_json_limited(&self.accounting, 16 * 1024 * 1024)?;
        let network = self.network.pack();
        let revenue = VaultRevenue::decode(accounting.record.as_bytes())?;
        ensure!(
            Store::verify_vault_revenue(
                &network,
                &header,
                &revenue,
                &Proof::decode(accounting.proof.as_bytes())?
            ),
            "invalid vault revenue proof"
        );
        let fees = accounting.extinguished_shannons.map(|value| value.value());
        ensure!(
            Store::verify_extinguished_fees(
                &network,
                &header,
                fees,
                &Proof::decode(accounting.fee_proof.as_bytes())?
            ),
            "invalid extinguished fee proof"
        );
        let total = revenue.total()?;
        ensure!(
            total == fees.unwrap_or_default(),
            "vault revenue differs from the extinguished fee tally"
        );
        let entries: Vec<_> = revenue.entries().iter().map(|entry| {
            json!({"lane": H256::from(entry.lane.clone()), "attributed_shannons": Uint64::from(entry.attributed)})
        }).collect();
        print_json(&json!({
            "height": header.context.height,
            "attributed_shannons": Uint64::from(total),
            "entries": entries,
        }))
    }
}
