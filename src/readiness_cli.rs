//! Operator-side verification of a common finalized checkpoint.

use std::{collections::HashSet, net::SocketAddr, path::PathBuf, time::Duration};

use anyhow::{Context, Result, ensure};
use ckb_types::prelude::Entity;
use clap::Args;
use serde::Serialize;
use stem::{
    jolteon::{Client, verify_finality},
    native_store::{Retention, Store},
};

use crate::{bridge_cli, native_spec};

const MAX_REPLICAS: usize = 32;
const MAX_HANDOFFS: u64 = 64;
const MAX_PROOF_BYTES: usize = 64 * 1024;
const MAX_ANCESTRY: u64 = 192;

#[derive(Args)]
pub(crate) struct Readiness {
    /// Locally trusted genesis specification.
    #[arg(long)]
    genesis: PathBuf,
    /// Replica peer address. Repeat to check a group of replicas.
    #[arg(long = "replica", required = true)]
    replicas: Vec<SocketAddr>,
    /// Smallest acceptable common finalized height.
    #[arg(long, default_value = "1", value_parser = clap::value_parser!(u64).range(1..))]
    min_height: u64,
    /// Socket timeout in milliseconds.
    #[arg(long, default_value = "3000", value_parser = clap::value_parser!(u64).range(1..=30_000))]
    timeout_ms: u64,
}

#[derive(Serialize)]
struct ReplicaHeight {
    address: SocketAddr,
    reported_height: u64,
}

impl Readiness {
    pub(crate) fn run(self) -> Result<()> {
        ensure!(
            self.replicas.len() <= MAX_REPLICAS,
            "at most {MAX_REPLICAS} replicas may be checked"
        );
        let mut unique = HashSet::new();
        ensure!(
            self.replicas.iter().all(|address| unique.insert(address)),
            "replica addresses must be distinct"
        );

        let timeout = Duration::from_millis(self.timeout_ms);
        let mut replicas = Vec::with_capacity(self.replicas.len());
        let mut common_height = u64::MAX;
        let mut proof_peer = self.replicas[0];
        for address in self.replicas {
            let status = Client::new(address, timeout)
                .status()
                .with_context(|| format!("read status from {address}"))?;
            ensure!(!status.catching_up, "replica {address} is catching up");
            if status.height < common_height {
                common_height = status.height;
                proof_peer = address;
            }
            replicas.push(ReplicaHeight {
                address,
                reported_height: status.height,
            });
        }
        ensure!(
            common_height >= self.min_height,
            "common height {common_height} is below requested minimum {}",
            self.min_height
        );

        let client = Client::new(proof_peer, timeout);
        let certificate = client.finality(common_height).with_context(|| {
            format!("read finality at height {common_height} from {proof_peer}")
        })?;
        ensure!(
            certificate.epoch <= MAX_HANDOFFS,
            "certificate needs more than {MAX_HANDOFFS} committee handoffs"
        );
        ensure!(
            certificate.proof.len() <= MAX_PROOF_BYTES,
            "finality proof exceeds {MAX_PROOF_BYTES} bytes"
        );
        let handoffs = (0..certificate.epoch)
            .map(|epoch| {
                let proof = client.handoff(epoch).with_context(|| {
                    format!("read handoff from epoch {epoch} from {proof_peer}")
                })?;
                ensure!(
                    proof.len() <= MAX_PROOF_BYTES,
                    "epoch {epoch} handoff proof exceeds {MAX_PROOF_BYTES} bytes"
                );
                Ok(proof)
            })
            .collect::<Result<Vec<_>>>()?;

        let spec = native_spec::load(&self.genesis)?;
        let directory = tempfile::tempdir()?;
        let trust = Store::open(directory.path(), &spec, Retention::Archive)?;
        let network = trust.network();
        let mut header =
            verify_finality(&trust, &handoffs, &certificate.proof, certificate.handoff)
                .with_context(|| {
                    format!("verify finality from {proof_peer} against trusted genesis")
                })?;
        let anchor_height = header.context.height;
        let distance = anchor_height
            .checked_sub(common_height)
            .context("finality proof predates the common height")?;
        ensure!(
            distance <= MAX_ANCESTRY,
            "finality proof is more than {MAX_ANCESTRY} Headers beyond the common height"
        );
        for height in (common_height..anchor_height).rev() {
            let parent = client.header(height).with_context(|| {
                format!("read certified ancestry at {height} from {proof_peer}")
            })?;
            ensure!(
                parent.id(network) == header.context.parent,
                "Header {height} from {proof_peer} is outside the certified ancestry"
            );
            header = parent;
        }

        for replica in &replicas {
            let address = replica.address;
            let observed = Client::new(address, timeout)
                .header(common_height)
                .with_context(|| format!("read common Header at {common_height} from {address}"))?;
            ensure!(
                observed == header,
                "replica {address} disagrees with the certified Header at {common_height}"
            );
        }

        bridge_cli::print_json(&serde_json::json!({
            "network_id": format!("0x{}", hex::encode(network.as_slice())),
            "genesis_id": format!("0x{}", hex::encode(trust.genesis_id().as_slice())),
            "height": common_height,
            "header_id": format!("0x{}", hex::encode(header.id(network).as_slice())),
            "app_hash": format!("0x{}", hex::encode(header.app_hash(network).as_slice())),
            "certificate_epoch": certificate.epoch,
            "certificate_height": anchor_height,
            "replicas": replicas,
        }))
    }
}
