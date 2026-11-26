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
use ckb_jsonrpc_types::{
    HeaderView, JsonBytes, Transaction as JsonTransaction, TransactionAndWitnessProof,
};
use ckb_types::{
    bytes::Bytes,
    core::TransactionView,
    packed::{self, WitnessArgs},
    prelude::*,
};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::json;

use crate::bridge_cli::{parse_hash, print_json, read_json_limited, read_key};

mod ceiling;
mod metatype_history;
mod workflow;

#[derive(Subcommand)]
pub enum CkbWallet {
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
    /// Verify a historical CKB output against a trusted block hash.
    VerifyInclusion {
        /// File containing a JSON hex packed CKB transaction.
        #[arg(long)]
        transaction: PathBuf,
        /// File with the CKB RPC header and transaction-and-witness proof.
        #[arg(long)]
        proof: PathBuf,
        #[arg(long, value_parser = parse_hash)]
        block_hash: ckb_types::H256,
        #[arg(long, value_parser = parse_hash)]
        tx_hash: ckb_types::H256,
        #[arg(long)]
        output_index: u32,
    },
    /// Recover a Metatype workflow return from witnessed CKB custody history.
    RecoverMetatypeWorkflow {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        l1_rpc: String,
    },
    /// Recover an oversized Metatype return's custody proof from anchored CKB history.
    RecoverMetatypeCeiling {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        l1_rpc: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inclusion {
    header: HeaderView,
    proof: TransactionAndWitnessProof,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    inputs: Vec<usize>,
    keys: Vec<PathBuf>,
    #[serde(default)]
    lock_prefix: JsonBytes,
}

impl CkbWallet {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Key { key_file, ed25519 } => {
                if ed25519 {
                    let public = read_key(&key_file)?.verifying_key().to_bytes();
                    return print_json(&json!({"ed25519": JsonBytes::from_vec(public.to_vec())}));
                }
                let (pubkey, args) = public_key(&key_file)?;
                print_json(&json!({
                    "public_key": JsonBytes::from_vec(pubkey),
                    "blake160": JsonBytes::from_vec(args.to_vec()),
                }))
            }
            Self::Sign {
                transaction,
                groups,
            } => {
                let tx: packed::Transaction =
                    read_json_limited::<JsonTransaction>(&transaction, 4 * 1024 * 1024)?.into();
                let tx = tx.into_view();
                let groups = read_json_limited::<Vec<Group>>(&groups, 4 * 1024 * 1024)?;
                let signed = sign(&tx, &groups)?;
                let packed = signed.data();
                print_json(&json!({
                    "hash": format!("{:#x}", signed.hash()),
                    "json": JsonTransaction::from(packed.clone()),
                    "molecule": JsonBytes::from_bytes(packed.as_bytes()),
                }))
            }
            Self::VerifyInclusion {
                transaction,
                proof,
                block_hash,
                tx_hash,
                output_index,
            } => {
                let bytes: JsonBytes = read_json_limited(&transaction, 8 * 1024 * 1024)?;
                let tx = packed::Transaction::from_slice(bytes.as_bytes())?.into_view();
                let Inclusion { header, proof } = read_json_limited(&proof, 16 * 1024)?;
                let header: ckb_types::core::HeaderView = header.into();
                let point = packed::OutPoint::new(tx_hash.into(), output_index);
                let (output, data) = stem::l1_proof::verify_output(
                    &(&block_hash).into(),
                    &header,
                    &point,
                    &tx,
                    &proof,
                )?;
                print_json(&json!({
                    "block_number": ckb_jsonrpc_types::BlockNumber::from(header.number()),
                    "block_hash": block_hash,
                    "out_point": ckb_jsonrpc_types::OutPoint::from(point),
                    "output": ckb_jsonrpc_types::CellOutput::from(output),
                    "data": JsonBytes::from_bytes(data),
                }))
            }
            Self::RecoverMetatypeWorkflow { request, l1_rpc } => {
                workflow::recover(&request, &l1_rpc)
            }
            Self::RecoverMetatypeCeiling { request, l1_rpc } => ceiling::recover(&request, &l1_rpc),
        }
    }
}

pub fn public_key(key: &Path) -> Result<(Vec<u8>, [u8; 20])> {
    let pubkey = secret_key(key)?.pubkey()?.serialize();
    let hash = blake2b_256(&pubkey);
    Ok((pubkey, hash[..20].try_into()?))
}

pub fn sign_single(tx: &TransactionView, input: usize, key: &Path) -> Result<TransactionView> {
    sign(
        tx,
        &[Group {
            inputs: vec![input],
            keys: vec![key.to_path_buf()],
            lock_prefix: JsonBytes::default(),
        }],
    )
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
