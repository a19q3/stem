mod recovery;

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::JsonBytes;
use ckb_types::{H256, prelude::*};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::json;
use stem_bridge_wire::intercell::{Delivery, MAX_PAYLOAD, Message, record_key};
use stem_bridge_wire::spore::{CellTransfer, Handoff, MAX_CELL_SPORE_DATA, MeltReceipt, SporeData};
use stem_ledger::block::Header;
use stem_stake::{
    finality::NATIVE_HEADER_BYTES,
    intercell::{Inbox, consume, outbox_system_root},
    patricia::{Commitment, Namespace, decode_path},
};

use crate::bridge_cli::{print_json, read_json_limited};

#[derive(Subcommand)]
pub enum Intercell {
    /// Bind a CKB custody handoff to the spent native title and its complete successor.
    SporeHandoff {
        #[arg(long)]
        request: PathBuf,
    },
    /// Encode a CKB issuer's authorization for a complete native output Cell.
    SporePermit {
        #[arg(long)]
        request: PathBuf,
        /// Authorize creation from the first input. The default authorizes transfer from the object input.
        #[arg(long)]
        create: bool,
    },
    /// Build the settlement payload from a complete binary Molecule SporeData file.
    SporePayload {
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        original_type: H256,
        #[arg(long, value_parser = crate::bridge_cli::parse_hash)]
        recipient: H256,
        #[arg(long)]
        data: PathBuf,
        /// Validate a new deposit against the standard Spore settlement profile.
        #[arg(long, conflicts_with = "melt")]
        deposit: bool,
        /// JSON containing the live collection's Type and complete data for a Cluster member.
        #[arg(long, requires = "deposit")]
        cluster: Option<PathBuf>,
        /// Certify a holder's CKB melt and name the recipient of the native collateral.
        #[arg(long)]
        melt: bool,
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
struct PermitRequest {
    network: H256,
    object: H256,
    source: ckb_jsonrpc_types::OutPoint,
    output: JsonBytes,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffRequest {
    object: H256,
    #[serde(default)]
    original_type: Option<H256>,
    previous_owner: H256,
    source: ckb_jsonrpc_types::OutPoint,
    output: JsonBytes,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DepositCluster {
    #[serde(rename = "type")]
    type_script: ckb_jsonrpc_types::Script,
    data: JsonBytes,
}

fn standard_deposit(data: &[u8], cluster: Option<DepositCluster>) -> Result<()> {
    let spore = SporeData::from_cell(data)
        .context("Spore MIME or executable extension is outside the standard custody profile")?;
    let Some(id) = spore.cluster_id else {
        ensure!(cluster.is_none(), "unclustered Spore must omit --cluster");
        return Ok(());
    };
    let cluster =
        cluster.context("Cluster member deposit requires the live collection with --cluster")?;
    let script: ckb_types::packed::Script = cluster.type_script.into();
    const RELEASED_CLUSTER_CODE: &str =
        "7366a61534fa7c7e6225ecc0d828ea3b5366adec2b58206f2ee84995fe030075";
    ensure!(
        u8::from(script.hash_type()) == ckb_types::core::ScriptHashType::Data1 as u8
            && hex::encode(script.code_hash().as_slice()) == RELEASED_CLUSTER_CODE
            && script.args().raw_data().as_ref() == id,
        "collection differs from the member's released Cluster Type"
    );

    let data = cluster.data.as_bytes();
    let number = |offset: usize| -> Option<usize> {
        Some(u32::from_le_bytes(data.get(offset..offset + 4)?.try_into().ok()?) as usize)
    };
    let first = number(4).context("truncated Cluster data")?;
    ensure!(
        number(0) == Some(data.len()) && matches!(first, 12 | 16 | 20) && first <= data.len(),
        "unsupported Cluster data layout"
    );
    let mut offsets = (4..first)
        .step_by(4)
        .map(number)
        .collect::<Option<Vec<_>>>()
        .context("truncated Cluster fields")?;
    offsets.push(data.len());
    ensure!(offsets.is_sorted(), "invalid Cluster field offsets");
    let field = |i: usize| &data[offsets[i]..offsets[i + 1]];
    let body = |entry: &[u8]| {
        entry
            .get(..4)
            .and_then(|len| len.try_into().ok())
            .is_some_and(|len| u32::from_le_bytes(len) as usize == entry.len() - 4)
    };
    ensure!(
        body(field(0)) && field(0).len() > 4 && body(field(1)),
        "invalid Cluster name or description"
    );
    ensure!(
        first == 12 || field(2).is_empty(),
        "collection-level Mutant is outside the standard custody profile"
    );
    ensure!(
        first != 20 || (body(field(3)) && field(3)[4..].starts_with(b"ipfs://")),
        "unsupported Cluster metadata"
    );
    Ok(())
}

impl Intercell {
    pub fn run(self) -> Result<()> {
        match self {
            Self::SporeHandoff { request } => {
                let request: HandoffRequest =
                    read_json_limited(&request, 2 * stem_ledger::MAX_TRANSACTION_BYTES + 1024)?;
                let output = stem_ledger::Cell::from_slice(request.output.as_bytes())?;
                let owner = output.lock();
                ensure!(
                    u8::from(owner.hash_type()) == ckb_types::core::ScriptHashType::Data as u8
                        && owner.args().raw_data().len() == 20,
                    "the handoff holder needs a native Data lock with a 20-byte key argument"
                );
                let mut types = output.type_scripts();
                ensure!(
                    types.next().is_some_and(
                        |ty| ty.calc_script_hash().as_slice() == request.object.as_bytes()
                    ) && types.next().is_none(),
                    "handoff output must contain only the named title"
                );
                let source = ckb_types::packed::OutPoint::from(request.source);
                let bytes = Handoff::encode(
                    &request.original_type.unwrap_or(request.object).into(),
                    &owner.calc_script_hash().unpack(),
                    &request.previous_owner.into(),
                    source.as_slice().try_into()?,
                    output.content_hash().as_slice().try_into()?,
                    &output.data(),
                )
                .context("invalid registered Spore handoff")?;
                print_json(&json!({"payload": JsonBytes::from_vec(bytes.to_vec())}))
            }
            Self::SporePermit { request, create } => {
                use stem_bridge_wire::object::permit::{Action, Permit};
                let request: PermitRequest =
                    read_json_limited(&request, 2 * stem_ledger::MAX_TRANSACTION_BYTES + 1024)?;
                let output = stem_ledger::Cell::from_slice(request.output.as_bytes())?;
                ensure!(
                    output
                        .type_scripts()
                        .any(|ty| ty.calc_script_hash().as_slice() == request.object.as_bytes()),
                    "output does not contain the named native object"
                );
                let source = ckb_types::packed::OutPoint::from(request.source);
                let permit = Permit {
                    action: if create {
                        Action::Create
                    } else {
                        Action::Transfer
                    },
                    network: request.network.into(),
                    object: request.object.into(),
                    source: source.as_slice().try_into()?,
                    output_hash: output.content_hash().as_slice().try_into()?,
                };
                let bytes = permit.encode();
                Permit::decode(&bytes).context("invalid object permit")?;
                print_json(&json!({"record": JsonBytes::from_vec(bytes.to_vec())}))
            }
            Self::SporePayload {
                original_type,
                recipient,
                data,
                deposit,
                cluster,
                melt,
            } => {
                let bytes = crate::bridge_cli::read_limited(&data, MAX_CELL_SPORE_DATA)?;
                let (original, recipient) = (original_type.into(), recipient.into());
                if melt {
                    let receipt = MeltReceipt::encode(&original, &recipient, &bytes)
                        .context("invalid meltable SporeData or settlement identity")?;
                    return print_json(
                        &json!({ "payload": JsonBytes::from_vec(receipt.to_vec()) }),
                    );
                }
                if deposit {
                    standard_deposit(
                        &bytes,
                        cluster
                            .as_ref()
                            .map(|path| read_json_limited(path, 2 * MAX_CELL_SPORE_DATA + 2048))
                            .transpose()?,
                    )?;
                }
                let mut buffer = [0; MAX_PAYLOAD];
                let payload =
                    CellTransfer::encode_custody(&original, &recipient, &bytes, &mut buffer)
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
