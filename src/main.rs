#[cfg(unix)]
mod abci;
#[cfg(unix)]
mod bridge;
#[cfg(unix)]
mod bridge_cli;
#[cfg(unix)]
mod cell_proof;
#[cfg(unix)]
mod ckb_rpc;
#[cfg(unix)]
mod comet_rpc;
#[cfg(unix)]
mod dev;
#[cfg(unix)]
mod engine;
#[cfg(unix)]
mod genesis;
#[cfg(unix)]
mod governance;
#[cfg(unix)]
mod index;
#[cfg(unix)]
mod l1;
#[cfg(unix)]
mod payout;
#[cfg(unix)]
mod snapshots;
#[cfg(unix)]
use fern::socket;
#[cfg(unix)]
mod stake_cli;
#[cfg(unix)]
mod store;
#[cfg(all(unix, test))]
mod tests;
#[cfg(unix)]
mod vault;
#[cfg(unix)]
mod wallet;

#[cfg(unix)]
use std::{
    fs::{self, DirBuilder, File},
    io::Read,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
};

#[cfg(unix)]
use anyhow::{Context, Result, ensure};
#[cfg(unix)]
use clap::{Parser, Subcommand};

#[cfg(unix)]
use crate::genesis::{Genesis, MAX_GENESIS_BYTES};

#[cfg(unix)]
#[derive(Parser)]
#[command(about = "CKB Cell execution service for CometBFT", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(unix)]
#[derive(Subcommand)]
enum Command {
    /// Pack the committee for a Cell-based validator update.
    CommitteeData {
        /// JSON file containing an array of Ed25519 keys and weights.
        #[arg(long)]
        members: PathBuf,
    },
    /// Build and sign regular CKB transactions with local keys.
    Wallet {
        #[command(subcommand)]
        command: wallet::WalletCommand,
    },
    /// Bridge deposit and withdrawal transaction utilities.
    Bridge {
        #[command(subcommand)]
        command: bridge_cli::BridgeCommand,
    },
    /// Observe the confirmed CKB Stake Registry and sign a snapshot.
    Stake {
        #[command(subcommand)]
        command: stake_cli::StakeCommand,
    },
    /// Run a loopback CKB wallet RPC endpoint backed by a sidechain replica.
    Rpc(ckb_rpc::Serve),
    /// Validate application genesis and print its CKB header hash.
    GenesisHash {
        #[arg(long)]
        genesis: PathBuf,
    },
    /// Generate a chain and signed secp256k1 payments with public fixture keys.
    DevFixture {
        #[arg(long)]
        spec: PathBuf,
        #[arg(long)]
        payments: u32,
        #[arg(long)]
        governance_lock: Option<PathBuf>,
        /// Separate owner lock for rotating deposit guardians on a bridged chain.
        #[arg(long)]
        guardian_governance_lock: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "payments")]
        workload: dev::Workload,
        #[arg(long)]
        xudt: Option<PathBuf>,
        /// Generate signed funding splits for a long unbridged CKB payment run.
        #[arg(long)]
        prefund_payments: bool,
    },
    Run {
        #[arg(long)]
        home: PathBuf,
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long, default_value_t = 2)]
        workers: usize,
        /// Separate workers for mempool admission. Defaults to --workers.
        #[arg(long)]
        admission_workers: Option<usize>,
        /// Maximum queued responses per ABCI connection.
        #[arg(long, default_value_t = socket::MAX_PENDING_RESPONSES)]
        abci_response_queue: usize,
        /// Maintain the optional CKB RPC index on this replica.
        #[arg(long)]
        rpc_index: bool,
        /// Drop local ordinary transaction bodies and omit them from future writes.
        #[arg(long)]
        compact_transaction_history: bool,
        /// Preserve at least this many recent CometBFT blocks. Zero keeps the full consensus archive.
        #[arg(long, default_value_t = 0)]
        retain_consensus_blocks: u64,
    },
}

#[cfg(unix)]
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::CommitteeData { members } => {
            let committee: Vec<genesis::Validator> = bridge_cli::read_json(&members)?;
            let data = governance::encode(&committee)?;
            bridge_cli::print_json(&ckb_jsonrpc_types::JsonBytes::from_vec(data))
        }
        Command::Wallet { command } => command.run(),
        Command::Bridge { command } => command.run(),
        Command::Stake { command } => command.run(),
        Command::Rpc(serve) => serve.run(),
        Command::GenesisHash { genesis } => {
            let (header, _) = read_genesis(&genesis)?.validate()?;
            bridge_cli::print_json(&format!("{:#x}", header.hash()))
        }
        Command::DevFixture {
            spec,
            payments,
            governance_lock,
            guardian_governance_lock,
            workload,
            xudt,
            prefund_payments,
        } => {
            let xudt = xudt.map(fs::read).transpose()?;
            let mut fixture = dev::fixture(
                read_genesis(&spec)?,
                payments,
                workload,
                xudt,
                prefund_payments,
            )?;
            if let Some(path) = governance_lock {
                let lock = bridge_cli::read_json::<ckb_jsonrpc_types::Script>(&path)?;
                fixture.add_governance(lock.into())?;
            }
            if let Some(path) = guardian_governance_lock {
                let lock = bridge_cli::read_json::<ckb_jsonrpc_types::Script>(&path)?;
                fixture.add_guardian_governance(lock.into())?;
            }
            serde_json::to_writer(std::io::stdout().lock(), &fixture)?;
            Ok(())
        }
        Command::Run {
            home,
            genesis,
            workers,
            admission_workers,
            abci_response_queue,
            rpc_index,
            compact_transaction_history,
            retain_consensus_blocks,
        } => {
            ensure!(
                (1..=64).contains(&workers),
                "workers must be between 1 and 64"
            );
            let admission_workers = admission_workers.unwrap_or(workers);
            ensure!(
                (1..=64).contains(&admission_workers),
                "admission workers must be between 1 and 64"
            );
            ensure!(
                (1..=socket::MAX_PENDING_RESPONSES).contains(&abci_response_queue),
                "ABCI response queue must be between 1 and {}",
                socket::MAX_PENDING_RESPONSES
            );
            let spec = read_genesis(&genesis)?;
            ensure!(
                !compact_transaction_history || spec.checkpoint_compact_transactions,
                "compact transaction history requires a format-3 or format-4 genesis"
            );
            ensure!(
                retain_consensus_blocks == 0
                    || (spec.checkpoint_interval > 0
                        && retain_consensus_blocks >= spec.checkpoint_interval.saturating_mul(2)),
                "consensus pruning requires snapshots and at least two checkpoint intervals of history"
            );
            match DirBuilder::new().mode(0o700).create(&home) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            let metadata = fs::symlink_metadata(&home)?;
            ensure!(
                metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
                "application home must be a private directory with mode 0700"
            );
            let app = abci::App::open(
                &home,
                spec,
                workers,
                retain_consensus_blocks,
                rpc_index,
                compact_transaction_history,
            )?;
            socket::serve(
                &home.canonicalize()?.join("abci.sock"),
                app,
                admission_workers,
                abci_response_queue,
            )
        }
    }
}

#[cfg(unix)]
fn read_genesis(path: &std::path::Path) -> Result<Genesis> {
    let mut raw = Vec::new();
    File::open(path)
        .with_context(|| format!("open {}", path.display()))?
        .take(MAX_GENESIS_BYTES as u64 + 1)
        .read_to_end(&mut raw)?;
    Genesis::decode(&raw)
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("fern requires Unix-domain sockets");
    std::process::ExitCode::FAILURE
}
