use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, ensure};
use ckb_types::bytes::Bytes;
use clap::Args;
use stem::native_store::Spec;
use stem_ledger::{
    block::{Header, Receipt},
    proof::InclusionProof,
    protocol::{BridgeSettlement, Operation, SettlementAction, settlement_digest},
};

use super::*;

#[derive(Args)]
pub struct WithdrawalWitness {
    #[arg(long)]
    genesis: PathBuf,
    #[arg(long, value_parser = parse_hash)]
    lane: H256,
    #[arg(long)]
    nonce: u64,
    /// JSON hex for the native Header already authenticated by the CKB Checkpoint Cell.
    #[arg(long)]
    header: PathBuf,
    /// Response body of the /stem/bridge-withdrawal query.
    #[arg(long)]
    proof: PathBuf,
}

#[derive(Deserialize)]
struct PendingProof {
    record: JsonBytes,
    proof: JsonBytes,
}

impl WithdrawalWitness {
    pub fn run(self) -> Result<()> {
        let spec = native_spec::load(&self.genesis)?;
        let header: JsonBytes = read_json_limited(&self.header, 1024)?;
        let header = Header::decode(header.as_bytes())?;
        let evidence: PendingProof = read_json_limited(&self.proof, 128 * 1024)?;
        let record = evidence.record.as_bytes();
        let path = stem_ledger::patricia::Proof::decode(evidence.proof.as_bytes())?;
        ensure!(
            stem::native_store::Store::verify_bridge_withdrawal(
                &spec.network,
                &header,
                &self.lane.pack(),
                self.nonce,
                record,
                &path,
            ),
            "withdrawal is absent from the authenticated SystemRoot",
        );
        let mut witness = u32::try_from(record.len())?.to_le_bytes().to_vec();
        witness.extend_from_slice(record);
        witness.extend_from_slice(evidence.proof.as_bytes());
        print_json(&JsonBytes::from_vec(witness))
    }
}

#[derive(Args)]
pub struct AttestPayout {
    #[arg(long)]
    genesis: PathBuf,
    /// Explicitly verify a CometBFT comparison-network proof using its fixed genesis.
    #[arg(long, requires = "proof_verifier")]
    comet_genesis: Option<PathBuf>,
    #[arg(long, requires = "comet_genesis")]
    proof_verifier: Option<PathBuf>,
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
    proof: JsonBytes,
    commit: PathBuf,
    handoffs: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JolteonProof {
    proof: JsonBytes,
    terminal: bool,
    history: Option<HeaderHistory>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HeaderHistory {
    frontier: JsonBytes,
    siblings: Vec<H256>,
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
        let header = Header::decode(request.header.as_bytes())?;
        self.authenticate_header(spec, request, &header)?;
        let raw = Transaction::from_slice(request.transaction.as_bytes())?;
        let receipt = Receipt::decode(request.receipt.as_bytes())?;
        let (tx_proof, receipt_proof) =
            InclusionProof::decode_execution_pair(request.proof.as_bytes())?;
        ensure!(
            tx_proof.verify_execution(
                &spec.network,
                &header.execution(),
                &raw,
                &receipt_proof,
                &receipt
            ),
            "Stem receipt is outside the authenticated execution roots"
        );
        let fund_claim = match raw.operation() {
            None => None,
            Some(stem_ledger::protocol::Operation::FeeClaim(claim)) => {
                let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
                let funds = bridge.fee_funds.as_ref().context("fee funds disabled")?;
                ensure!(
                    spec.stake.is_none()
                        || claim.fund != stem_ledger::protocol::FeeFund::Validators,
                    "stake-backed validator payouts require a reward claim"
                );
                Some((claim.amount, funds[claim.fund as usize].recipient.clone()))
            }
            Some(stem_ledger::protocol::Operation::RewardClaim(claim)) => {
                ensure!(spec.stake.is_some(), "CKB staking is disabled");
                ensure!(
                    claim.payout != 0,
                    "reward accrual did not request a withdrawal"
                );
                Some((claim.payout, claim.recipient))
            }
            Some(_) => anyhow::bail!("this protocol operation cannot issue a withdrawal ticket"),
        };
        if fund_claim.is_some() {
            let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
            ensure!(
                bridge.fee_funds.is_some()
                    && find_lane(bridge, &request.lane.pack())?.l1_type.is_none(),
                "fee claim requires the native CKB lane"
            );
        }
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
            if let Some((amount, destination)) = &fund_claim {
                ensure!(
                    capacity == *amount
                        && units == 0
                        && &recipient == destination
                        && item[72..76] == [0; 4],
                    "withdrawal ticket does not match the configured fee claim"
                );
            }
            let id = stem_bridge_wire::withdrawal_receipt_id(&spec.network.unpack(), item);
            return Ok(Ticket {
                id: id.into(),
                capacity,
                units,
                recipient,
            });
        }
        anyhow::bail!("proof has no matching withdrawal ticket")
    }

    fn authenticate_header(
        &self,
        spec: &Spec,
        request: &PayoutRequest,
        header: &Header,
    ) -> Result<()> {
        let directory = self.request.parent().unwrap_or_else(|| Path::new("."));
        if let Some((comet, proof_verifier)) = self
            .comet_genesis
            .as_ref()
            .zip(self.proof_verifier.as_ref())
        {
            let genesis = native_spec::verify_comet_genesis(spec, comet)?;
            let mut verifier = Command::new(proof_verifier);
            verifier
                .arg("-genesis")
                .arg(comet)
                .arg("-stem-genesis")
                .arg(hex::encode(genesis.as_slice()))
                .arg("-commit")
                .arg(directory.join(&request.commit))
                .arg("-height")
                .arg(header.context.height.to_string())
                .arg("-app-hash")
                .arg(hex::encode(header.app_hash(&spec.network).as_slice()));
            if let Some(handoffs) = &request.handoffs {
                verifier.arg("-handoffs").arg(directory.join(handoffs));
            }
            let verification = verifier.output()?;
            ensure!(
                verification.status.success(),
                "Stem signed successor: {}",
                String::from_utf8_lossy(&verification.stderr)
            );
        } else {
            let bundle: JolteonProof =
                read_json_limited(&directory.join(&request.commit), 128 * 1024)?;
            let handoffs: Vec<JsonBytes> = request
                .handoffs
                .as_ref()
                .map(|path| read_json_limited(&directory.join(path), 16 * 1024 * 1024))
                .transpose()?
                .unwrap_or_default();
            let handoffs: Vec<_> = handoffs
                .into_iter()
                .map(|proof| proof.as_bytes().to_vec())
                .collect();
            let home = tempfile::tempdir()?;
            let genesis = stem::native_store::Store::open(
                home.path(),
                spec,
                stem::native_store::Retention::Archive,
            )?;
            let proved = stem::jolteon::verify_finality(
                &genesis,
                &handoffs,
                bundle.proof.as_bytes(),
                bundle.terminal,
            )?;
            if proved != *header {
                let history = bundle
                    .history
                    .context("missing Jolteon withdrawal ancestry proof")?;
                let frontier = stem_stake::history::Frontier::decode(
                    spec.network.unpack(),
                    history.frontier.as_bytes(),
                )?;
                let siblings: Vec<_> = history.siblings.into_iter().map(|hash| hash.0).collect();
                ensure!(
                    frontier.count() == proved.context.height
                        && frontier.root().as_slice() == proved.state.history_root.as_slice()
                        && frontier.verify_inclusion(
                            header.context.height,
                            &header.id(&spec.network).unpack(),
                            &siblings
                        ),
                    "Jolteon proof does not authenticate the withdrawal Header"
                );
            }
        }
        Ok(())
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
            .is_some_and(|script| script == lane.vault_type),
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
        "L1 recipient differs from the authenticated Stem withdrawal"
    );
    let actual_units = if lane.l1_type.is_some() {
        stem_bridge_wire::amount(&data).context("invalid L1 payout units")?
    } else {
        ensure!(
            data.is_empty(),
            "native CKB payout must use empty Cell Data"
        );
        0
    };
    ensure!(
        actual_units == ticket.units,
        "L1 payout units differ from the authenticated Stem withdrawal"
    );

    let mut successors = tx
        .outputs_with_data_iter()
        .enumerate()
        .filter(|(_, (output, _))| {
            output
                .type_()
                .to_opt()
                .is_some_and(|script| script == lane.vault_type)
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
            && request.nonce.checked_add(1) == Some(nonce(&after_data)?)
            && after_data.get(114..146) == Some(ticket.id.as_bytes()),
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
