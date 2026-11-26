mod recovery;

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::JsonBytes;
use ckb_types::{H256, prelude::*};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::json;
use stem_bridge_wire::intercell::{Delivery, MAX_PAYLOAD, Message, record_key};
use stem_bridge_wire::spore::{CellTransfer, MAX_CELL_SPORE_DATA};
use stem_ledger::block::Header;
use stem_stake::{
    finality::NATIVE_HEADER_BYTES,
    intercell::{Inbox, consume, outbox_system_root},
    patricia::{Commitment, Namespace, decode_path},
};

use crate::bridge_cli::{print_json, read_json_limited};

#[derive(Subcommand)]
pub enum Intercell {
    /// Build the settlement payload from a complete binary Molecule SporeData file.
    SporePayload {
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        original_type: H256,
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        recipient: H256,
        #[arg(long)]
        data: PathBuf,
    },
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
    /// Rebuild the replay index from a live CKB mailbox head.
    RecoverInbox(recovery::Recover),
    /// Verify the Stem outbox path and build the CKB input_type witness.
    DeliveryWitness {
        #[arg(long)]
        request: PathBuf,
        /// Canonical execution Header in the live checkpoint dependency.
        #[arg(long)]
        header: PathBuf,
        /// Output of checkpoint history for an older Outbox proof.
        #[arg(long)]
        history: Option<PathBuf>,
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
struct DeliveryRequest {
    record: JsonBytes,
    proof: JsonBytes,
    expected_root: H256,
    consumed: Option<Vec<H256>>,
    replay_path: Option<JsonBytes>,
}

impl DeliveryRequest {
    fn replay_path(&self, message: &Message<'_>, id: &[u8; 32]) -> Result<Vec<u8>> {
        match (&self.consumed, &self.replay_path) {
            (Some(consumed), None) => {
                let mut inbox = Inbox::new(message.receiver);
                for consumed in consumed {
                    inbox.insert(consumed.clone().into())?;
                }
                ensure!(
                    inbox.root() == self.expected_root.as_bytes(),
                    "stale Intercell inbox root"
                );
                Ok(inbox.path(id))
            }
            (None, Some(path)) => Ok(path.as_bytes().to_vec()),
            _ => anyhow::bail!("provide exactly one of consumed and replay_path"),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistoryProof {
    header: JsonBytes,
    id: H256,
    frontier: JsonBytes,
    siblings: Vec<H256>,
}

fn historical_evidence(path: PathBuf, network: &[u8; 32]) -> Result<Vec<u8>> {
    if path.extension().is_some_and(|ext| ext == "strand") {
        let bytes = crate::bridge_cli::read_limited(&path, 16 * 1024)?;
        let proof = stem_ledger::history::HistoricalHeaderProof::decode(&bytes)?;
        return Ok(proof.witness());
    }
    let proof: HistoryProof = read_json_limited(&path, 16 * 1024)?;
    let header = Header::decode(proof.header.as_bytes())?;
    ensure!(
        header.id(&network.pack()).as_slice() == proof.id.as_bytes() && proof.siblings.len() <= 63,
        "invalid Intercell history proof"
    );
    let mut bytes = proof.header.into_bytes().to_vec();
    bytes.extend_from_slice(proof.frontier.as_bytes());
    for sibling in proof.siblings {
        bytes.extend_from_slice(sibling.as_bytes());
    }
    Ok(bytes)
}

impl Intercell {
    pub fn run(self) -> Result<()> {
        match self {
            Self::SporePayload {
                original_type,
                recipient,
                data,
            } => {
                let bytes = crate::bridge_cli::read_limited(&data, MAX_CELL_SPORE_DATA)?;
                let mut buffer = [0; MAX_PAYLOAD];
                let payload = CellTransfer::encode(
                    &original_type.into(),
                    &recipient.into(),
                    &bytes,
                    &mut buffer,
                )
                .context("invalid SporeData or settlement identity")?;
                print_json(&json!({ "payload": JsonBytes::from_vec(payload.to_vec()) }))
            }
            Self::RecoverInbox(args) => args.run(),
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
            Self::DeliveryWitness {
                request,
                header,
                history,
            } => {
                let request: DeliveryRequest = read_json_limited(&request, 64 * 1024 * 1024)?;
                let raw_header: JsonBytes = read_json_limited(&header, 4096)?;
                Header::decode(raw_header.as_bytes())?;
                let checkpoint: &[u8; NATIVE_HEADER_BYTES] = raw_header
                    .as_bytes()
                    .try_into()
                    .context("invalid Intercell checkpoint Header")?;
                let raw = request.record.as_bytes();
                let message = Message::decode(raw).context("invalid Intercell message")?;
                let history = history
                    .map(|path| historical_evidence(path, &message.source))
                    .transpose()?
                    .unwrap_or_default();
                let root = outbox_system_root(&message.source, checkpoint, &history)
                    .context("invalid Intercell checkpoint ancestry")?;
                let id = message.id();
                let proof = request.proof.as_bytes();
                let commitment = Commitment::new(message.source, Namespace::System);
                ensure!(
                    commitment.verify(
                        &root,
                        &commitment.record_key(&record_key(false, &id)),
                        Some(&commitment.value_hash(raw)),
                        decode_path(proof)?.into_iter(),
                    ),
                    "invalid Intercell outbox proof"
                );
                let path = request.replay_path(&message, &id)?;
                let expected_root = request.expected_root.into();
                let successor_root = consume(&message.receiver, &id, &expected_root, &path)
                    .context("invalid Intercell replay path for expected inbox root")?;
                let bytes = [
                    &u32::try_from(raw.len())?.to_le_bytes()[..],
                    &u32::try_from(proof.len())?.to_le_bytes(),
                    &u32::try_from(history.len())?.to_le_bytes(),
                    raw,
                    proof,
                    &history,
                    &path,
                ]
                .concat();
                Delivery::decode(&bytes).context("Intercell witness exceeds limits")?;
                print_json(&json!({
                    "id": H256::from(id),
                    "input_type": JsonBytes::from_vec(bytes),
                    "successor_root": H256::from(successor_root),
                }))
            }
        }
    }
}
