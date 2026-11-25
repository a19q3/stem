mod audit;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{JsonBytes, OutPoint, Script as JsonScript};
use ckb_types::{
    H256,
    core::{Capacity, ScriptHashType},
    packed::{Byte32, CellInput, CellOutput, Script},
    prelude::*,
};
use clap::Subcommand;
use ed25519_dalek::Signer;
use serde::Deserialize;
use serde_json::json;
use stem_stake::{
    Hash32,
    security::{self, Domain, Policy, Position, Request},
};

use crate::{
    bridge_cli::{parse_hash, print_json, read_json_limited, read_key},
    l1::{Journal, Rpc},
};

#[derive(Subcommand)]
pub enum Command {
    /// Audit a CKB domain through a specified Cell or closure.
    Audit(audit::Collect),
    /// Verify a portable audit against independently trusted CKB block hashes.
    VerifyAudit(audit::Verify),
    /// Derive a unique CKB domain Type ID, bond encoding and minimum funding.
    Domain {
        #[arg(long)]
        request: PathBuf,
    },
    /// Derive the escrow Lock for a paid CKB Live Cell observation.
    Request {
        #[arg(long)]
        request: PathBuf,
    },
    /// Observe the current CKB subject and sign a domain-separated service report.
    Sign {
        #[arg(long)]
        l1_rpc: String,
        #[arg(long)]
        domain: PathBuf,
        #[arg(long)]
        request: PathBuf,
        #[arg(long, value_parser = parse_hash)]
        code_hash: H256,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        journal: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DomainDraft {
    code_hash: H256,
    seed: OutPoint,
    output_index: u64,
    ckb_genesis: H256,
    network: H256,
    operator: JsonBytes,
    penalty: JsonBytes,
    quorum_bps: u16,
    challenge_blocks: u64,
    members: Vec<MemberDraft>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberDraft {
    public_key: H256,
    beneficiary: JsonBytes,
    bond: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestDraft {
    code_hash: H256,
    domain: H256,
    buyer: JsonBytes,
    subject: OutPoint,
    refund_height: u64,
    receipt_capacity: u64,
}

impl Command {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Audit(args) => args.run(),
            Self::VerifyAudit(args) => args.run(),
            Self::Domain { request } => make_domain(&request),
            Self::Request { request } => make_request(&request),
            Self::Sign {
                l1_rpc,
                domain,
                request,
                code_hash,
                key_file,
                journal,
            } => sign(
                &Rpc::new(&l1_rpc)?,
                &domain,
                &request,
                &code_hash,
                &key_file,
                &journal,
            ),
        }
    }
}

fn make_domain(path: &Path) -> Result<()> {
    let draft: DomainDraft = read_json_limited(path, 16 * 1024)?;
    let seed = CellInput::new(draft.seed.into(), 0);
    let type_id =
        ckb_hash::blake2b_256([seed.as_slice(), &draft.output_index.to_le_bytes()].concat());
    let policy = Policy {
        type_id,
        ckb_genesis: draft.ckb_genesis.as_bytes().try_into()?,
        network: draft.network.as_bytes().try_into()?,
        operator: draft.operator.as_bytes().as_ref().try_into()?,
        penalty: draft.penalty.as_bytes().as_ref().try_into()?,
        quorum_bps: draft.quorum_bps,
        challenge_blocks: draft.challenge_blocks,
    };
    let args = policy.encode();
    Policy::decode(&args)?;
    let type_ = program(&draft.code_hash, &args)?;
    let id = type_.calc_script_hash();
    let lock = program(&draft.code_hash, id.as_slice())?;
    let mut positions = draft
        .members
        .into_iter()
        .map(|member| {
            Ok(Position {
                key: member.public_key.as_bytes().try_into()?,
                beneficiary: member.beneficiary.as_bytes().as_ref().try_into()?,
                bond: member.bond,
                slashed: false,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        (1..=security::MAX_MEMBERS).contains(&positions.len()),
        "invalid security domain roster size"
    );
    positions.sort_unstable_by_key(|position| position.key);
    let data = Domain {
        retired: false,
        positions,
    }
    .encode();
    let domain = Domain::decode(&data)?;
    let occupied = CellOutput::new_builder()
        .lock(lock.clone())
        .type_(Some(type_.clone()).pack())
        .build()
        .occupied_capacity(Capacity::bytes(data.len())?)?
        .as_u64();
    let bond = domain.bonded()?;
    let capacity = occupied
        .checked_add(bond)
        .context("domain capacity overflow")?;
    print_json(&json!({
        "type_id": H256::from(type_id), "domain": H256::from(id), "lock": JsonScript::from(lock),
        "type": JsonScript::from(type_), "state": JsonBytes::from_vec(data),
        "bonded_shannons": bond, "occupied_shannons": occupied, "capacity": capacity,
    }))
}

fn make_request(path: &Path) -> Result<()> {
    let draft: RequestDraft = read_json_limited(path, 4096)?;
    let subject: ckb_types::packed::OutPoint = draft.subject.into();
    let request = Request {
        domain: draft.domain.as_bytes().try_into()?,
        buyer: draft.buyer.as_bytes().as_ref().try_into()?,
        subject: subject.as_slice().try_into()?,
        refund_height: draft.refund_height,
        receipt_capacity: draft.receipt_capacity,
    };
    let args = request.encode();
    Request::decode(&args)?;
    print_json(&JsonScript::from(program(&draft.code_hash, &args)?))
}

fn sign(
    rpc: &Rpc,
    domain_path: &Path,
    request_path: &Path,
    code_hash: &H256,
    key_file: &Path,
    journal_path: &Path,
) -> Result<()> {
    let domain_point: OutPoint = read_json_limited(domain_path, 4096)?;
    let request_point: OutPoint = read_json_limited(request_path, 4096)?;
    let domain_cell = rpc.live_cell(&domain_point)?;
    let domain_type: Script = domain_cell
        .output
        .type_
        .context("missing domain Type Script")?
        .into();
    let policy = Policy::decode(&domain_type.args().raw_data())?;
    let domain_id: Hash32 = domain_type.calc_script_hash().unpack();
    ensure!(
        domain_type == program(code_hash, &policy.encode())?
            && Script::from(domain_cell.output.lock) == program(code_hash, &domain_id)?,
        "domain differs from the selected CKB program"
    );
    ensure!(
        rpc.block_hash(0)?.as_bytes() == policy.ckb_genesis,
        "domain uses a different CKB genesis"
    );
    let domain = Domain::decode(
        domain_cell
            .data
            .context("missing domain state")?
            .content
            .as_bytes(),
    )?;
    ensure!(!domain.retired, "security domain is retired");
    let requester = rpc.live_cell(&request_point)?;
    ensure!(
        requester.output.type_.is_none()
            && requester.data.is_some_and(|data| data.content.is_empty()),
        "service request must be a plain CKB Cell"
    );
    let escrow: Script = requester.output.lock.into();
    let request = Request::decode(&escrow.args().raw_data())?;
    ensure!(
        escrow == program(code_hash, &request.encode())? && request.domain == domain_id,
        "request targets a different domain"
    );
    ensure!(
        requester
            .output
            .capacity
            .value()
            .checked_sub(request.receipt_capacity)
            .is_some_and(|reward| reward >= security::MIN_PAYMENT),
        "request has no usable service fee"
    );
    let subject = OutPoint::from(ckb_types::packed::OutPoint::from_slice(&request.subject)?);
    ensure!(
        subject != request_point && subject != domain_point,
        "service subject would be consumed"
    );
    let live = rpc.live_cell(&subject)?;
    let output: CellOutput = live.output.into();
    let data = live.data.context("missing Cell Dep data")?;
    let data_hash = if data.content.is_empty() {
        [0; 32]
    } else {
        ckb_hash::blake2b_256(data.content.as_bytes())
    };
    ensure!(
        data_hash == data.hash.as_bytes(),
        "CKB data differs from the reported hash"
    );
    let result = security::live_value(&request.subject, output.as_slice(), &data_hash);
    let point: ckb_types::packed::OutPoint = request_point.into();
    let request_id: &[u8; security::OUT_POINT] = point.as_slice().try_into()?;
    let key = read_key(key_file)?;
    let public = key.verifying_key().to_bytes();
    let index = domain
        .positions
        .iter()
        .position(|position| position.key == public && !position.slashed)
        .context("key does not represent an active domain position")?;
    let journal_key = ckb_hash::blake2b_256(
        [
            b"Stem security domain signer journal\0".as_slice(),
            &domain_id,
            request_id,
            &public,
        ]
        .concat(),
    );
    let journal = Journal::open(journal_path, journal_key.pack())?;
    if let Some(previous) = journal.read_record::<H256>()? {
        ensure!(
            previous.as_bytes() == result,
            "signer refuses a conflicting service report"
        );
    }
    journal.write_record(&H256::from(result))?;
    let digest = security::signing_digest(&domain_id, request_id, &result);
    print_json(&json!({
        "index": index, "public_key": H256::from(public), "result": H256::from(result),
        "digest": H256::from(digest), "signature": JsonBytes::from_vec(key.sign(&digest).to_bytes().to_vec()),
    }))
}

fn program(code: &H256, args: &[u8]) -> Result<Script> {
    ensure!(
        *code != H256::default(),
        "missing security domain program hash"
    );
    Ok(Script::new_builder()
        .code_hash(Byte32::from(code))
        .hash_type(ScriptHashType::Data2)
        .args(args.pack())
        .build())
}
