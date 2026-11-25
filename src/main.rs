#[cfg(unix)]
mod bridge_cli;
#[cfg(unix)]
mod checkpoint_cli;
#[cfg(unix)]
mod ckb_wallet;
#[cfg(unix)]
mod l1;
#[cfg(unix)]
mod native_spec;
#[cfg(unix)]
mod native_wallet;
#[cfg(unix)]
use fern::socket;
#[cfg(unix)]
mod stake_cli;

#[cfg(unix)]
use std::{
    fs::{self, DirBuilder},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
};

#[cfg(unix)]
use anyhow::{Result, ensure};
#[cfg(unix)]
use clap::{Parser, Subcommand};

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
    /// Build and sign native Fern transactions.
    Wallet {
        #[command(subcommand)]
        command: native_wallet::Wallet,
    },
    /// Manage local keys and sign CKB L1 transactions.
    CkbWallet {
        #[command(subcommand)]
        command: ckb_wallet::CkbWallet,
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
    /// Prepare CKB checkpoints and query native history proofs.
    Checkpoint {
        #[command(subcommand)]
        command: checkpoint_cli::CheckpointCommand,
    },
    /// Validate native application genesis and print its Fern Header ID.
    GenesisHash {
        #[arg(long)]
        genesis: PathBuf,
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
        /// Keep the complete application execution and Header archive.
        #[arg(long)]
        archive: bool,
        /// How many recent application blocks to retain in ordinary mode.
        #[arg(long, default_value_t = 256)]
        history_window: u64,
        /// Schedule native state snapshots every N blocks. Zero disables export.
        #[arg(long, default_value_t = 32)]
        snapshot_interval: u64,
        /// Local maximum snapshot size in bytes.
        #[arg(long, default_value_t = 512 * 1024 * 1024)]
        snapshot_bytes: u64,
    },
}

#[cfg(unix)]
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Wallet { command } => command.run(),
        Command::CkbWallet { command } => command.run(),
        Command::Bridge { command } => command.run(),
        Command::Stake { command } => command.run(),
        Command::Checkpoint { command } => command.run(),
        Command::GenesisHash { genesis } => {
            use ckb_types::prelude::Entity;
            let spec = native_spec::load(&genesis)?;
            let directory = tempfile::tempdir()?;
            let store = fern::native_store::Store::open(
                directory.path(),
                &spec,
                fern::native_store::Retention::Archive,
            )?;
            bridge_cli::print_json(&format!("0x{}", hex::encode(store.genesis_id().as_slice())))
        }
        Command::Run {
            home,
            genesis,
            workers,
            admission_workers,
            abci_response_queue,
            archive,
            history_window,
            snapshot_interval,
            snapshot_bytes,
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
            let spec = native_spec::load(&genesis)?;
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
            let retention = if archive {
                fern::native_store::Retention::Archive
            } else {
                fern::native_store::Retention::Recent(history_window)
            };
            let store = fern::native_store::Store::open(&home.join("db"), &spec, retention)?;
            let app = fern::native_abci::App::new(
                store,
                workers,
                &home.join("snapshots"),
                snapshot_interval,
                snapshot_bytes,
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

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("fern requires Unix-domain sockets");
    std::process::ExitCode::FAILURE
}
