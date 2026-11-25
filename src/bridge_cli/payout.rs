use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use ckb_types::bytes::Bytes;
use clap::Args;
use fern::native_store::{Retention, Spec, Store};
use fern_ledger::{
    block::{Header, Receipt},
    proof::InclusionProof,
    protocol::{BridgeSettlement, Operation, SettlementAction, settlement_digest},
};

use super::*;

#[derive(Args)]
pub struct AttestPayout {
    #[arg(long)]
    genesis: PathBuf,
    #[arg(long)]
    comet_genesis: PathBuf,
    #[arg(long)]
    proof_verifier: PathBuf,
    #[arg(long)]
    l1_rpc: String,
    #[arg(long)]
    request: PathBuf,
    #[arg(long)]
    key_file: PathBuf,
    #[arg(long)]
    journal: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PayoutRequest {
    lane: H256,
    nonce: u64,
    payout: H256,
    vault_input: JsonPoint,
    previous_l1_height: u64,
    previous_l1_hash: H256,
    header: JsonBytes,
    transaction: JsonBytes,
    receipt: JsonBytes,
    proof: Paths,
    commit: PathBuf,
    handoffs: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Paths {
    index: u32,
    count: u32,
    transaction_siblings: Vec<HexHash>,
    receipt_siblings: Vec<HexHash>,
}

#[derive(Deserialize)]
#[serde(transparent)]
struct HexHash(String);

impl HexHash {
    fn decode(&self) -> Result<Byte32> {
        Ok(parse_hash(&self.0)?.pack())
    }
}

#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PayoutNotice {
    l1_genesis: H256,
    authority: H256,
    previous_height: u64,
    previous_hash: H256,
    confirmed_height: u64,
    confirmed_hash: H256,
    lane: H256,
    vault_input: JsonPoint,
    vault_output: JsonPoint,
    nonce: u64,
    receipt: H256,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SignedSettlement {
    statement: JsonBytes,
    confirmed_height: u64,
    confirmed_hash: H256,
    signer: u16,
    signature: JsonBytes,
}

pub(super) fn sign_settlement(
    spec: &Spec,
    bridge: &BridgeGenesis,
    statement: BridgeSettlement,
    key_file: &Path,
) -> Result<()> {
    let key = read_key(key_file)?;
    let signer = bridge
        .guardians
        .iter()
        .position(|member| member.key == key.verifying_key().to_bytes())
        .context("key is absent from the configured bridge guardians")?;
    let digest = settlement_digest(&spec.network, &statement);
    print_json(&SignedSettlement {
        statement: JsonBytes::from_vec(statement.encode().to_vec()),
        confirmed_height: statement.anchor.height,
        confirmed_hash: statement.anchor.hash.unpack(),
        signer: u16::try_from(signer)?,
        signature: JsonBytes::from_vec(key.sign(digest.as_slice()).to_bytes().to_vec()),
    })
}

impl PayoutNotice {
    fn statement(&self) -> BridgeSettlement {
        BridgeSettlement {
            l1_genesis: self.l1_genesis.pack(),
            authority: self.authority.pack(),
            previous_anchor: L1Anchor {
                height: self.previous_height,
                hash: self.previous_hash.pack(),
            },
            anchor: L1Anchor {
                height: self.confirmed_height,
                hash: self.confirmed_hash.pack(),
            },
            action: SettlementAction::WithdrawalPaid {
                vault: VaultUpdate {
                    lane: self.lane.pack(),
                    input: self.vault_input.clone().into(),
                    output: self.vault_output.clone().into(),
                },
                receipt: self.receipt.pack(),
                nonce: self.nonce,
            },
        }
    }
}

impl AttestPayout {
    pub fn run(self) -> Result<()> {
        let spec = native_spec::load(&self.genesis)?;
        let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
        let request: PayoutRequest = read_json_limited(&self.request, 4 * 1024 * 1024)?;
        let lane = find_lane(bridge, &request.lane.pack())?;
        let ticket = self.verify_ticket(&spec, &request)?;
        let rpc = Rpc::new(&self.l1_rpc)?;
        check_l1(&rpc, bridge)?;
        let journal = Journal::open(&self.journal, spec.network.clone())?;
        journal.pin(&rpc, request.previous_l1_height, &request.previous_l1_hash)?;
        let notice = inspect_payout(&rpc, bridge, lane, request, ticket)?;
        journal.pin(&rpc, notice.confirmed_height, &notice.confirmed_hash)?;
        sign_settlement(&spec, bridge, notice.statement(), &self.key_file)
    }

    fn verify_ticket(&self, spec: &Spec, request: &PayoutRequest) -> Result<Ticket> {
        let genesis: serde_json::Value = read_json_limited(&self.comet_genesis, 2 * 1024 * 1024)?;
        let directory = tempfile::tempdir()?;
        let state = Store::open(directory.path(), spec, Retention::Archive)?;
        let stated = genesis["app_state"]["fern_genesis"]
            .as_str()
            .context("missing CometBFT application genesis")?;
        ensure!(
            parse_hash(stated)?.as_bytes() == state.genesis_id().as_slice()
                && genesis["chain_id"].as_str() == Some(spec.chain_id.as_str()),
            "CometBFT genesis differs from the native specification"
        );
        let mut voters = genesis["validators"]
            .as_array()
            .context("missing CometBFT genesis committee")?
            .iter()
            .map(|voter| {
                let key = STANDARD.decode(
                    voter["pub_key"]["value"]
                        .as_str()
                        .context("missing CometBFT public key")?,
                )?;
                let power = voter["power"]
                    .as_str()
                    .context("invalid CometBFT voting power")?
                    .parse()?;
                Ok((key.as_slice().try_into()?, power))
            })
            .collect::<Result<Vec<([u8; 32], i64)>>>()?;
        voters.sort_unstable();
        ensure!(
            voters == state.genesis_validators(),
            "CometBFT genesis committee differs from the native specification"
        );
        let header = Header::decode(request.header.as_bytes())?;
        let app_hash = header.app_hash(&spec.network);
        let directory = self.request.parent().unwrap_or_else(|| Path::new("."));
        let mut verifier = Command::new(&self.proof_verifier);
        verifier
            .arg("-genesis")
            .arg(&self.comet_genesis)
            .arg("-commit")
            .arg(directory.join(&request.commit))
            .arg("-height")
            .arg(header.context.height.to_string())
            .arg("-app-hash")
            .arg(hex::encode(app_hash.as_slice()));
        if let Some(handoffs) = &request.handoffs {
            verifier.arg("-handoffs").arg(directory.join(handoffs));
        }
        let verification = verifier.output()?;
        ensure!(
            verification.status.success(),
            "Fern signed successor: {}",
            String::from_utf8_lossy(&verification.stderr)
        );
        let raw = Transaction::from_slice(request.transaction.as_bytes())?;
        let receipt = Receipt::decode(request.receipt.as_bytes())?;
        let inclusion = |siblings: &[HexHash]| -> Result<InclusionProof> {
            ensure!(siblings.len() <= 32, "oversized Merkle proof");
            Ok(InclusionProof {
                position: request.proof.index,
                count: request.proof.count,
                siblings: siblings
                    .iter()
                    .map(HexHash::decode)
                    .collect::<Result<_>>()?,
            })
        };
        let tx_proof = inclusion(&request.proof.transaction_siblings)?;
        let receipt_proof = inclusion(&request.proof.receipt_siblings)?;
        ensure!(
            tx_proof.verify_execution(
                &spec.network,
                &header.execution(),
                &raw,
                &receipt_proof,
                &receipt
            ),
            "Fern receipt is outside the authenticated execution roots"
        );
        ensure!(
            raw.operation().is_none(),
            "a bridge control operation cannot issue a withdrawal ticket"
        );
        let tx_id = raw.id(&spec.network);
        let expected_lane = request.lane.as_bytes();
        let mut data = receipt.protocol_result.as_ref();
        while !data.is_empty() {
            let (length, rest) = data
                .split_at_checked(4)
                .context("truncated withdrawal ticket size")?;
            let size = u32::from_le_bytes(length.try_into()?) as usize;
            let (item, rest) = rest
                .split_at_checked(size)
                .context("truncated withdrawal ticket")?;
            data = rest;
            ensure!(item.len() >= 100, "invalid withdrawal ticket");
            let (lane, nonce) = (&item[..32], u64::from_le_bytes(item[32..40].try_into()?));
            if lane != expected_lane || nonce != request.nonce {
                continue;
            }
            ensure!(
                &item[40..72] == tx_id.as_slice(),
                "withdrawal ticket names a different transaction"
            );
            let capacity = u64::from_le_bytes(item[76..84].try_into()?);
            let units = u128::from_le_bytes(item[84..100].try_into()?);
            let recipient = Script::from_slice(&item[100..])?;
            let mut hash = ckb_hash::new_blake2b();
            hash.update(b"Fern withdrawal receipt\0");
            hash.update(spec.network.as_slice());
            hash.update(item);
            let mut id = [0; 32];
            hash.finalize(&mut id);
            return Ok(Ticket {
                id: id.into(),
                capacity,
                units,
                recipient,
            });
        }
        anyhow::bail!("proof has no matching withdrawal ticket")
    }
}

struct Ticket {
    id: H256,
    capacity: u64,
    units: u128,
    recipient: Script,
}

fn inspect_payout(
    rpc: &Rpc,
    bridge: &BridgeGenesis,
    lane: &LaneGenesis,
    request: PayoutRequest,
    ticket: Ticket,
) -> Result<PayoutNotice> {
    let input: OutPoint = request.vault_input.clone().into();
    let (before, before_data, vault_height) = l1_cell(rpc, &input)?;
    ensure!(
        before
            .type_()
            .to_opt()
            .is_some_and(|script| script.calc_script_hash() == lane.vault_type_hash),
        "payout did not spend the configured vault"
    );
    let (tx, height, block) = committed(rpc, &request.payout)?;
    ensure!(
        height > vault_height && tx.input_pts_iter().any(|point| point == input),
        "payout does not consume the vault input"
    );
    let (paid, data) = tx
        .outputs_with_data_iter()
        .next()
        .context("payout transaction has no recipient output")?;
    ensure!(
        paid.lock() == ticket.recipient
            && u64::from(paid.capacity()) == ticket.capacity
            && paid.type_().to_opt().as_ref() == lane.l1_type.as_ref(),
        "L1 recipient differs from the authenticated Fern withdrawal"
    );
    let actual_units = if lane.l1_type.is_some() {
        fern_bridge_wire::amount(&data).context("invalid L1 payout units")?
    } else {
        ensure!(
            data.is_empty(),
            "native CKB payout must use empty Cell Data"
        );
        0
    };
    ensure!(
        actual_units == ticket.units,
        "L1 payout units differ from the authenticated Fern withdrawal"
    );

    let mut successors = tx
        .outputs_with_data_iter()
        .enumerate()
        .filter(|(_, (output, _))| {
            output
                .type_()
                .to_opt()
                .is_some_and(|script| script.calc_script_hash() == lane.vault_type_hash)
        });
    let (index, (successor, after_data)) = successors
        .next()
        .context("payout transaction has no vault successor")?;
    ensure!(
        successors.next().is_none()
            && successor.lock() == before.lock()
            && u64::from(before.capacity()).checked_sub(ticket.capacity)
                == Some(u64::from(successor.capacity())),
        "payout exceeds its vault reservation"
    );
    ensure!(
        after_data.get(1) == Some(&1)
            && nonce(&before_data)? == request.nonce
            && request.nonce.checked_add(1) == Some(nonce(&after_data)?),
        "wrong L1 payout nonce"
    );
    let confirmed_height = height
        .checked_add(bridge.confirmations)
        .context("L1 confirmation height overflow")?
        .max(
            request
                .previous_l1_height
                .checked_add(1)
                .context("L1 anchor height overflow")?,
        );
    let confirmed_hash = rpc.block_hash(confirmed_height)?;
    ensure!(
        rpc.block_hash(height)? == block,
        "payout was reorganized during observation"
    );
    Ok(PayoutNotice {
        l1_genesis: bridge.l1_genesis.unpack(),
        authority: bridge.guardian_generation.unpack(),
        previous_height: request.previous_l1_height,
        previous_hash: request.previous_l1_hash,
        confirmed_height,
        confirmed_hash,
        lane: request.lane,
        vault_input: request.vault_input,
        vault_output: OutPoint::new(tx.hash(), u32::try_from(index)?).into(),
        nonce: request.nonce,
        receipt: ticket.id,
    })
}

fn nonce(data: &[u8]) -> Result<u64> {
    Ok(u64::from_le_bytes(
        data.get(2..10)
            .context("invalid L1 vault cell state")?
            .try_into()?,
    ))
}

pub fn settlement(observations: &Path) -> Result<()> {
    let mut signed: Vec<SignedSettlement> = read_json_limited(observations, 4 * 1024 * 1024)?;
    ensure!(!signed.is_empty(), "no settlement approvals");
    signed.sort_unstable_by_key(|approval| approval.signer);
    let statement = BridgeSettlement::decode(signed[0].statement.as_bytes())?;
    let approvals = signed
        .iter()
        .map(|approval| {
            ensure!(
                approval.statement == signed[0].statement
                    && approval.confirmed_height == statement.anchor.height
                    && approval.confirmed_hash.as_bytes() == statement.anchor.hash.as_slice(),
                "guardians approved different settlements"
            );
            Ok(Attestation {
                guardian: approval.signer,
                signature: approval.signature.as_bytes().try_into()?,
            })
        })
        .collect::<Result<_>>()?;
    let tx = Transaction::control(
        Operation::BridgeSettlement(statement),
        Certificate::new(Bytes::new(), approvals)?,
    )?;
    print_json(&JsonBytes::from_vec(tx.as_slice().to_vec()))
}
