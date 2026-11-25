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
use stem::socket;
#[cfg(unix)]
mod stake_cli;

#[cfg(unix)]
use std::{
    fs::{self, DirBuilder},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
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
    /// Build and sign native Stem transactions.
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
    /// Validate native application genesis and print its Stem Header ID.
    GenesisHash {
        #[arg(long)]
        genesis: PathBuf,
        /// Emit the canonical native genesis Header instead of its ID.
        #[arg(long)]
        header: bool,
    },
    /// Run Stem's weighted Jolteon replica.
    Jolteon {
        #[arg(long)]
        home: PathBuf,
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        configuration: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        /// Create a new durable validator journal.
        #[arg(long)]
        initialize: bool,
    },
    /// Verify a two-chain execution proof and any preceding committee handoffs.
    JolteonProof {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        proof: PathBuf,
        /// Earlier handoff proofs in chronological order.
        #[arg(long = "handoff")]
        handoffs: Vec<PathBuf>,
        /// The target proof itself closes its committee's epoch.
        #[arg(long)]
        terminal: bool,
    },
    /// Export or restore a finalized Jolteon state snapshot.
    JolteonSnapshot {
        #[command(subcommand)]
        command: SnapshotCommand,
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
#[derive(Subcommand)]
enum SnapshotCommand {
    /// Export the finalized state and committee evidence from a stopped replica.
    Export {
        #[arg(long)]
        home: PathBuf,
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        snapshot: PathBuf,
        #[arg(long)]
        certificate: PathBuf,
    },
    /// Restore a fresh observer using an independently pinned finalized Header ID.
    Restore {
        #[arg(long)]
        home: PathBuf,
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        snapshot: PathBuf,
        #[arg(long)]
        certificate: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        trusted_header: String,
        #[arg(long, default_value_t = 512 * 1024 * 1024)]
        max_bytes: u64,
    },
}

#[cfg(unix)]
impl SnapshotCommand {
    fn run(self) -> Result<()> {
        use ckb_types::prelude::Entity;
        use stem::{
            jolteon::snapshot,
            native_store::{Retention, StateSnapshot, Store},
        };
        match self {
            Self::Export {
                home,
                genesis,
                snapshot,
                certificate,
            } => {
                let spec = native_spec::load(&genesis)?;
                let store = Store::open(&home.join("db"), &spec, Retention::Archive)?;
                let (image, evidence) = snapshot::export(&store)?;
                image.encode(fs::File::create_new(snapshot)?)?;
                serde_json::to_writer(fs::File::create_new(certificate)?, &evidence)?;
                bridge_cli::print_json(&serde_json::json!({
                    "height": image.header.context.height,
                    "header_id": hex::encode(image.header.id(store.network()).as_slice()),
                }))
            }
            Self::Restore {
                home,
                genesis,
                snapshot,
                certificate,
                key_file,
                trusted_header,
                max_bytes,
            } => {
                let target =
                    hex::decode(trusted_header.strip_prefix("0x").unwrap_or(&trusted_header))?;
                let target = target
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("trusted Header ID must have 32 bytes"))?;
                let observer = bridge_cli::read_key(&key_file)?.verifying_key().to_bytes();
                let data = fs::File::open(snapshot)?;
                let length = data.metadata()?.len();
                ensure!(
                    length <= max_bytes,
                    "snapshot exceeds configured size limit"
                );
                let image = StateSnapshot::decode(data, length)?;
                let evidence = bridge_cli::read_json_limited(&certificate, 8 * 1024 * 1024)?;
                let spec = native_spec::load(&genesis)?;
                let parent = home
                    .parent()
                    .filter(|path| !path.as_os_str().is_empty())
                    .unwrap_or(Path::new("."));
                let staged = tempfile::TempDir::new_in(parent)?;
                let db = staged.path().join("db");
                let header = {
                    let mut store = Store::open(&db, &spec, Retention::Archive)?;
                    snapshot::install(&mut store, image, evidence, target, observer)?
                };
                DirBuilder::new().mode(0o700).create(&home)?;
                fs::rename(db, home.join("db"))?;
                bridge_cli::print_json(&serde_json::json!({
                    "height": header.context.height,
                    "header_id": hex::encode(target),
                    "observer_only": true,
                }))
            }
        }
    }
}

#[cfg(unix)]
fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Wallet { command } => command.run(),
        Command::CkbWallet { command } => command.run(),
        Command::Bridge { command } => command.run(),
        Command::Stake { command } => command.run(),
        Command::Checkpoint { command } => command.run(),
        Command::GenesisHash { genesis, header } => {
            use ckb_types::prelude::Entity;
            let spec = native_spec::load(&genesis)?;
            let directory = tempfile::tempdir()?;
            let store = stem::native_store::Store::open(
                directory.path(),
                &spec,
                stem::native_store::Retention::Archive,
            )?;
            let encoded = if header {
                hex::encode(store.tip().encode())
            } else {
                hex::encode(store.genesis_id().as_slice())
            };
            bridge_cli::print_json(&format!("0x{encoded}"))
        }
        Command::Jolteon {
            home,
            genesis,
            configuration,
            key_file,
            initialize,
        } => {
            let spec = native_spec::load(&genesis)?;
            let config = bridge_cli::read_json_limited(&configuration, 64 * 1024)?;
            let key = bridge_cli::read_key(&key_file)?;
            if initialize {
                DirBuilder::new().mode(0o700).create(&home)?;
            }
            let metadata = fs::symlink_metadata(&home)?;
            ensure!(
                metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
                "application home must be a private directory with mode 0700"
            );
            let store = stem::native_store::Store::open(
                &home.join("db"),
                &spec,
                stem::native_store::Retention::Archive,
            )?;
            stem::jolteon::run(store, key, config, initialize)
        }
        Command::JolteonProof {
            genesis,
            proof,
            handoffs,
            terminal,
        } => {
            use ckb_types::prelude::Entity;
            let spec = native_spec::load(&genesis)?;
            let read_proof = |path: &PathBuf| -> Result<Vec<u8>> {
                let raw: String = bridge_cli::read_json_limited(path, 128 * 1024)?;
                Ok(hex::decode(raw.strip_prefix("0x").unwrap_or(&raw))?)
            };
            let handoffs = handoffs
                .iter()
                .map(read_proof)
                .collect::<Result<Vec<_>>>()?;
            let directory = tempfile::tempdir()?;
            let store = stem::native_store::Store::open(
                directory.path(),
                &spec,
                stem::native_store::Retention::Archive,
            )?;
            let header =
                stem::jolteon::verify_finality(&store, &handoffs, &read_proof(&proof)?, terminal)?;
            bridge_cli::print_json(&serde_json::json!({
                "height": header.context.height,
                "header": hex::encode(header.encode()),
                "app_hash": hex::encode(header.app_hash(store.network()).as_slice()),
            }))
        }
        Command::JolteonSnapshot { command } => command.run(),
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
                stem::native_store::Retention::Archive
            } else {
                stem::native_store::Retention::Recent(history_window)
            };
            let store = stem::native_store::Store::open(&home.join("db"), &spec, retention)?;
            let app = stem::native_abci::App::new(
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
    eprintln!("stem requires Unix-domain sockets");
    std::process::ExitCode::FAILURE
}
