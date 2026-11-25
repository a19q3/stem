use std::{
    collections::HashSet,
    fs::File,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use ckb_crypto::secp::Privkey;
use ckb_hash::{blake2b_256, new_blake2b};
use ckb_jsonrpc_types::{JsonBytes, Transaction as JsonTransaction};
use ckb_types::{
    bytes::Bytes,
    core::{Capacity, DepType, TransactionBuilder, TransactionView},
    packed::{self, CellDep, CellInput, CellOutput, OutPoint, Script, WitnessArgs},
    prelude::*,
};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::json;

use crate::bridge_cli::{print_json, read_json, read_json_limited, read_key};

#[derive(Subcommand)]
pub enum WalletCommand {
    /// Derive the lock argument for a CKB secp256k1 key or an Ed25519 public key.
    Key {
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        ed25519: bool,
    },
    /// Pack and sign CKB secp256k1 input groups using local private key files.
    Sign {
        #[arg(long)]
        transaction: PathBuf,
        #[arg(long)]
        groups: PathBuf,
    },
    /// Sign separate one-input CKB payments funded by consecutive Cell outputs.
    Payments {
        #[arg(long)]
        funding_hash: String,
        #[arg(long)]
        dep_group_hash: String,
        #[arg(long)]
        recipient: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        payments: u32,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    inputs: Vec<usize>,
    keys: Vec<PathBuf>,
    #[serde(default)]
    lock_prefix: JsonBytes,
}

impl WalletCommand {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Key { key_file, ed25519 } => {
                if ed25519 {
                    let public = read_key(&key_file)?.verifying_key().to_bytes();
                    return print_json(&json!({"ed25519": JsonBytes::from_vec(public.to_vec())}));
                }
                let pubkey = secret_key(&key_file)?.pubkey()?.serialize();
                let hash = blake2b_256(&pubkey);
                print_json(&json!({
                    "public_key": JsonBytes::from_vec(pubkey),
                    "blake160": JsonBytes::from_vec(hash[..20].to_vec()),
                }))
            }
            Self::Sign {
                transaction,
                groups,
            } => {
                let tx: packed::Transaction =
                    read_json_limited::<JsonTransaction>(&transaction, 4 * 1024 * 1024)?.into();
                let tx = tx.into_view();
                let groups = read_json::<Vec<Group>>(&groups)?;
                let signed = sign(&tx, &groups)?;
                let packed = signed.data();
                print_json(&json!({
                    "hash": format!("{:#x}", signed.hash()),
                    "json": JsonTransaction::from(packed.clone()),
                    "molecule": JsonBytes::from_bytes(packed.as_bytes()),
                }))
            }
            Self::Payments {
                funding_hash,
                dep_group_hash,
                recipient,
                key_file,
                payments,
            } => {
                ensure!((1..=65_536).contains(&payments), "invalid payment count");
                let funding = crate::bridge_cli::hash(funding_hash)?.pack();
                let dep_hash = crate::bridge_cli::hash(dep_group_hash)?.pack();
                let dep = CellDep::new_builder()
                    .dep_type(DepType::DepGroup)
                    .out_point(OutPoint::new(dep_hash, 0))
                    .build();
                let lock: Script = read_json::<ckb_jsonrpc_types::Script>(&recipient)?.into();
                let capacity = Capacity::bytes(100)?.safe_sub(1000u64)?;
                let output = CellOutput::new_builder()
                    .lock(lock)
                    .capacity(capacity)
                    .build();
                let key = secret_key(&key_file)?;
                let payments = (0..payments).map(|index| {
                    let tx = TransactionBuilder::default()
                        .input(CellInput::new(OutPoint::new(funding.clone(), index), 0))
                        .output(output.clone()).output_data(Bytes::new()).cell_dep(dep.clone()).build();
                    let signed = crate::dev::sign_fixture_lock_group(&tx, &key)?;
                    Ok(json!({"hash": format!("{:#x}", signed.hash()), "tx": JsonTransaction::from(signed.data())}))
                }).collect::<Result<Vec<_>>>()?;
                print_json(&payments)
            }
        }
    }
}

fn sign(tx: &TransactionView, groups: &[Group]) -> Result<TransactionView> {
    let mut claimed = HashSet::new();
    let mut witnesses: Vec<_> = tx.witnesses().into_iter().collect();
    let inputs = tx.inputs().len();
    if witnesses.len() < inputs {
        witnesses.resize_with(inputs, packed::Bytes::default);
    }
    for group in groups {
        ensure!(
            !group.inputs.is_empty() && (1..=255).contains(&group.keys.len()),
            "invalid signing group"
        );
        ensure!(
            group.inputs.is_sorted()
                && group
                    .inputs
                    .iter()
                    .all(|i| *i < inputs && claimed.insert(*i)),
            "group inputs must be ordered and disjoint"
        );
        let first = group.inputs[0];
        let witness = if witnesses[first].raw_data().is_empty() {
            WitnessArgs::default()
        } else {
            WitnessArgs::from_slice(&witnesses[first].raw_data())?
        };
        let prefix = group.lock_prefix.as_bytes();
        ensure!(
            prefix.len() <= 4 + 255 * 20,
            "oversized multisig lock prefix"
        );
        let mut placeholder = prefix.to_vec();
        placeholder.resize(prefix.len() + 65 * group.keys.len(), 0);
        let blank = witness
            .clone()
            .as_builder()
            .lock(Some(Bytes::from(placeholder)).pack())
            .build();
        let mut hash = new_blake2b();
        hash.update(tx.hash().as_slice());
        let mut add = |witness: &[u8]| {
            hash.update(&(witness.len() as u64).to_le_bytes());
            hash.update(witness);
        };
        add(blank.as_slice());
        for index in &group.inputs[1..] {
            add(&witnesses[*index].raw_data());
        }
        for extra in &witnesses[inputs..] {
            add(&extra.raw_data());
        }
        let mut message = [0; 32];
        hash.finalize(&mut message);
        let message = message.into();
        let mut lock = prefix.to_vec();
        for path in &group.keys {
            lock.extend(secret_key(path)?.sign_recoverable(&message)?.serialize());
        }
        witnesses[first] = witness
            .as_builder()
            .lock(Some(Bytes::from(lock)).pack())
            .build()
            .as_bytes()
            .pack();
    }
    Ok(tx.as_advanced_builder().set_witnesses(witnesses).build())
}

fn secret_key(path: &Path) -> Result<Privkey> {
    let file = File::open(path)?;
    let meta = file.metadata()?;
    ensure!(
        meta.is_file() && meta.permissions().mode() & 0o077 == 0,
        "private key file must be private"
    );
    let mut data = Vec::new();
    file.take(33).read_to_end(&mut data)?;
    let key: [u8; 32] = data
        .try_into()
        .ok()
        .context("expected a 32-byte secp256k1 secret key")?;
    Ok(Privkey::from_slice(&key))
}
