use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::JsonBytes;
use ckb_types::{H256, prelude::*};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::json;
use stem::native_store::Store;
use stem_bridge_wire::intercell::{MAX_PAYLOAD, Message, delivery};
use stem_ledger::{block::Header, patricia::Proof};
use stem_stake::intercell::Inbox;

use crate::bridge_cli::{print_json, read_json_limited};

#[derive(Subcommand)]
pub enum Intercell {
    /// Encode a canonical message and return its stable nonce identity.
    Encode {
        #[arg(long)]
        request: PathBuf,
    },
    /// Decode a canonical message.
    Decode {
        #[arg(long)]
        record: PathBuf,
    },
    /// Verify the Stem outbox path and build the CKB input_type witness.
    DeliveryWitness {
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        header: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    source: H256,
    destination: H256,
    sender: H256,
    receiver: H256,
    nonce: u64,
    payload: JsonBytes,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Delivery {
    record: JsonBytes,
    proof: JsonBytes,
    consumed: Vec<H256>,
    expected_root: H256,
}

impl Intercell {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Encode { request } => {
                let request: Request = read_json_limited(&request, 2 * MAX_PAYLOAD + 1024)?;
                let message = Message {
                    source: request.source.into(),
                    destination: request.destination.into(),
                    sender: request.sender.into(),
                    receiver: request.receiver.into(),
                    nonce: request.nonce,
                    payload: request.payload.as_bytes(),
                };
                let record = [&message.header()[..], message.payload].concat();
                Message::decode(&record).context("invalid Intercell message")?;
                print_json(
                    &json!({"id": H256::from(message.id()), "record": JsonBytes::from_vec(record)}),
                )
            }
            Self::Decode { record } => {
                let bytes: JsonBytes = read_json_limited(&record, 2 * MAX_PAYLOAD + 1024)?;
                let message =
                    Message::decode(bytes.as_bytes()).context("invalid Intercell message")?;
                print_json(&json!({
                    "id": H256::from(message.id()), "source": H256::from(message.source),
                    "destination": H256::from(message.destination), "sender": H256::from(message.sender),
                    "receiver": H256::from(message.receiver),
                    "nonce": message.nonce, "payload": JsonBytes::from_vec(message.payload.to_vec()),
                }))
            }
            Self::DeliveryWitness { request, header } => {
                let request: Delivery = read_json_limited(&request, 64 * 1024 * 1024)?;
                let raw_header: JsonBytes = read_json_limited(&header, 4096)?;
                let header = Header::decode(raw_header.as_bytes())?;
                let raw = request.record.as_bytes();
                let message = Message::decode(raw).context("invalid Intercell message")?;
                let id = message.id();
                let proof = request.proof.as_bytes();
                ensure!(
                    Store::verify_intercell(
                        &message.source.pack(),
                        &header,
                        false,
                        &id,
                        Some(raw),
                        &Proof::decode(proof)?
                    ),
                    "invalid Intercell outbox proof"
                );
                let mut inbox = Inbox::new(message.receiver);
                for consumed in request.consumed {
                    inbox.insert(consumed.into())?;
                }
                ensure!(
                    inbox.root() == request.expected_root.as_bytes(),
                    "stale Intercell inbox root"
                );
                let path = inbox.path(&id);
                inbox.insert(id)?;
                let bytes = [
                    &u32::try_from(raw.len())?.to_le_bytes()[..],
                    &u32::try_from(proof.len())?.to_le_bytes(),
                    raw,
                    proof,
                    &path,
                ]
                .concat();
                delivery(&bytes).context("Intercell witness exceeds limits")?;
                print_json(&json!({
                    "id": H256::from(id),
                    "input_type": JsonBytes::from_vec(bytes),
                    "successor_root": H256::from(inbox.root()),
                }))
            }
        }
    }
}
