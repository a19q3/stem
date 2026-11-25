use std::{path::PathBuf, process::Command};

use anyhow::{Context, Result, ensure};
use ckb_types::{packed::Byte32, prelude::*};
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::json;
use stem::{native_store::HistoryIndex, native_store::Store};
use stem_ledger::patricia::Proof;
use stem_stake::{
    BlockId,
    checkpoint::{Checkpoint, Domain, Update, ZERO_OUT_POINT, stake_snapshot_commitment},
    finality::{self, Tip, Trust},
};

use crate::{
    bridge_cli::{print_json, read_json_limited},
    native_spec,
};

#[derive(Subcommand)]
pub enum CheckpointCommand {
    /// Prepare a checkpoint from authenticated Stem state and a signed successor.
    Prepare(Box<Prepare>),
    /// Verify the canonical publication certificate before embedding it in a CKB witness.
    Verify(Box<Verify>),
    /// Verify a direct CometBFT bundle and its native checkpoint tip.
    VerifyDirect(Box<VerifyDirect>),
    /// Rebuild the archive index as needed and produce a historical Header proof.
    History {
        #[command(flatten)]
        archive: Archive,
        #[arg(long)]
        header: u64,
    },
}

#[derive(Args)]
pub struct Archive {
    /// Path to the archive Stem node's application database.
    #[arg(long)]
    archive_db: PathBuf,
    /// Path to a separate, rebuildable RocksDB proof index.
    #[arg(long)]
    index: PathBuf,
    #[arg(long)]
    network: String,
    #[arg(long)]
    height: u64,
}

impl Archive {
    fn open(&self) -> Result<(Byte32, HistoryIndex)> {
        let network = decode::<32>(&self.network)?.pack();
        let mut index = HistoryIndex::open(&self.archive_db, &self.index, network.clone())?;
        index.sync(self.height)?;
        Ok((network, index))
    }
}

#[derive(Args)]
pub struct Prepare {
    #[command(flatten)]
    archive: Archive,
    #[arg(long)]
    genesis: PathBuf,
    #[arg(long)]
    comet_genesis: PathBuf,
    #[arg(long)]
    signed_successor: PathBuf,
    #[arg(long)]
    proof_verifier: PathBuf,
    #[arg(long)]
    handoffs: Option<PathBuf>,
    #[arg(long)]
    stake_proof: Option<PathBuf>,
    #[arg(long)]
    l1_genesis: String,
    #[arg(long)]
    checkpoint_type: String,
    #[arg(long)]
    initial_generation: String,
    #[arg(long)]
    new_generation: Option<String>,
    /// JSON string containing the old CKB Cell data.
    #[arg(long, requires = "previous_out_point")]
    previous_data: Option<PathBuf>,
    /// The 36-byte canonical CKB OutPoint being consumed.
    #[arg(long, requires = "previous_data")]
    previous_out_point: Option<String>,
}

#[derive(Args)]
pub struct Verify {
    #[arg(long)]
    network: String,
    #[arg(long)]
    l1_genesis: String,
    #[arg(long)]
    checkpoint_type: String,
    #[arg(long)]
    initial_generation: String,
    #[arg(long)]
    proposal: PathBuf,
    /// JSON string containing the canonical Update bytes.
    #[arg(long)]
    update: PathBuf,
    #[arg(long)]
    previous_data: Option<PathBuf>,
}

#[derive(Args)]
pub struct VerifyDirect {
    #[arg(long)]
    network: String,
    #[arg(long)]
    chain_id: String,
    #[arg(long)]
    initial_committee: String,
    #[arg(long)]
    genesis_app_hash: String,
    /// Proof bundle exported by stem-proof -direct-bundle.
    #[arg(long)]
    bundle: PathBuf,
    /// JSON hex for previously authenticated CKB Checkpoint Cell data.
    #[arg(long)]
    previous_tip: Option<PathBuf>,
}

#[derive(Deserialize)]
struct DirectBundle {
    proof: String,
    tip: String,
    native_height: u64,
}

impl VerifyDirect {
    fn run(self) -> Result<()> {
        let trust = Trust {
            network: decode(&self.network)?,
            chain_id: &self.chain_id,
            initial_committee: decode(&self.initial_committee)?,
            genesis_app_hash: decode(&self.genesis_app_hash)?,
        };
        let bundle: DirectBundle =
            read_json_limited(&self.bundle, 2 * finality::MAX_PROOF_BYTES + 4096)?;
        let previous = self
            .previous_tip
            .map(|path| -> Result<Tip> {
                let raw: String = read_json_limited(&path, 2 * Tip::SIZE + 16)?;
                Ok(Tip::decode(&restricted_bytes(&raw, Tip::SIZE)?)?)
            })
            .transpose()?;
        let tip = finality::verify(
            &trust,
            previous.as_ref(),
            &restricted_bytes(&bundle.proof, finality::MAX_PROOF_BYTES)?,
        )
        .context("direct CometBFT finality proof rejected")?;
        ensure!(
            tip.encode() == restricted_bytes(&bundle.tip, Tip::SIZE)?
                && tip.height() == bundle.native_height,
            "exported bundle tip differs from verified execution"
        );
        print_json(&json!({
            "height": tip.height(),
            "app_hash": encode(tip.app_hash(&trust.network)),
            "next_committee": encode(tip.next_committee),
        }))
    }
}

#[derive(Deserialize)]
struct Metadata {
    block: CompleteBlockId,
    successor: CompleteBlockId,
    committee: String,
    next_committee: String,
    handoffs: String,
}

#[derive(Deserialize)]
struct CompleteBlockId {
    hash: String,
    parts: PartSet,
}

#[derive(Deserialize)]
struct PartSet {
    total: u32,
    hash: String,
}

impl TryFrom<CompleteBlockId> for BlockId {
    type Error = anyhow::Error;

    fn try_from(value: CompleteBlockId) -> Result<Self> {
        Ok(Self {
            hash: decode(&value.hash)?,
            parts: value.parts.total,
            part_set_hash: decode(&value.parts.hash)?,
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotProof {
    snapshot: Option<String>,
    nodes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Proposal {
    network: String,
    l1_genesis: String,
    checkpoint_type: String,
    initial_generation: String,
    height: u64,
    predecessor: String,
    data: String,
    commitment: String,
    signing_message: String,
    suffix: Vec<String>,
}

impl CheckpointCommand {
    pub fn run(self) -> Result<()> {
        match self {
            Self::Prepare(args) => args.run(),
            Self::Verify(args) => args.run(),
            Self::VerifyDirect(args) => args.run(),
            Self::History { archive, header } => {
                let (network, index) = archive.open()?;
                let historical = index.header(header)?;
                let id = historical.id(&network);
                let frontier = index.frontier_at(archive.height)?;
                let siblings = index.inclusion(header, archive.height)?;
                ensure!(
                    frontier.verify_inclusion(header, &id.unpack(), &siblings),
                    "archive generated an invalid history proof"
                );
                print_json(&json!({
                    "header": encode(historical.encode()),
                    "id": encode(id.as_slice()),
                    "frontier": encode(frontier.encode()),
                    "siblings": siblings.iter().map(encode).collect::<Vec<_>>(),
                }))
            }
        }
    }
}

impl Prepare {
    fn run(self) -> Result<()> {
        let (network, index) = self.archive.open()?;
        let l1_genesis = decode(&self.l1_genesis)?;
        let domain = Domain {
            network: network.unpack(),
            ckb_genesis: l1_genesis,
            checkpoint_type: decode(&self.checkpoint_type)?,
        };
        let initial_generation = decode(&self.initial_generation)?;
        let spec = native_spec::load(&self.genesis)?;
        ensure!(
            spec.network == network,
            "archive network differs from trusted Stem genesis"
        );
        let genesis = native_spec::verify_comet_genesis(&spec, &self.comet_genesis)?;
        ensure!(
            index.header(0)?.id(&network) == genesis,
            "archive belongs to a different Stem genesis"
        );
        let header = index.header(self.archive.height)?;
        let stake_snapshot = if let Some(stake) = spec.stake {
            ensure!(
                stake.statement.domain.ckb_genesis == l1_genesis,
                "wrong CKB genesis"
            );
            let path = self
                .stake_proof
                .as_ref()
                .context("missing authenticated Stake Snapshot proof")?;
            let proof: SnapshotProof = read_json_limited(path, 512 * 1024)?;
            ensure!(
                proof.nodes.len() <= 257,
                "too many Stake Snapshot proof nodes"
            );
            let nodes = proof
                .nodes
                .iter()
                .map(|node| restricted_bytes(node, 162))
                .collect::<Result<_>>()?;
            let snapshot = restricted_bytes(
                &proof.snapshot.context("missing Stake Snapshot record")?,
                128 * 1024,
            )?;
            ensure!(
                Store::verify_stake_snapshot(&network, &header, Some(&snapshot), &Proof { nodes }),
                "invalid Stake Snapshot proof"
            );
            stake_snapshot_commitment(&domain.network, &snapshot)
        } else {
            ensure!(
                self.stake_proof.is_none(),
                "development network has no Stake Snapshot"
            );
            [0; 32]
        };
        let mut verifier = Command::new(&self.proof_verifier);
        verifier
            .arg("-genesis")
            .arg(&self.comet_genesis)
            .arg("-stem-genesis")
            .arg(hex::encode(genesis.as_slice()))
            .arg("-commit")
            .arg(&self.signed_successor)
            .arg("-height")
            .arg(self.archive.height.to_string())
            .arg("-app-hash")
            .arg(hex::encode(header.app_hash(&network).as_slice()))
            .arg("-metadata");
        if let Some(handoffs) = &self.handoffs {
            verifier.arg("-handoffs").arg(handoffs);
        }
        let verified = verifier.output()?;
        ensure!(
            verified.status.success(),
            "checkpoint signed successor: {}",
            String::from_utf8_lossy(&verified.stderr)
        );
        let metadata: Metadata = serde_json::from_slice(&verified.stdout)?;
        let old = read_old(&domain, self.previous_data)?;
        let predecessor = self
            .previous_out_point
            .as_deref()
            .map(decode::<36>)
            .transpose()?
            .unwrap_or(ZERO_OUT_POINT);
        let (sequence, previous, suffix, generation) = if let Some(old) = &old {
            ensure!(
                old.height() < self.archive.height
                    && index.header(old.height())?.id(&network).as_slice() == old.header
                    && index.frontier_at(old.height())? == old.history,
                "previous checkpoint does not match the archive"
            );
            (
                old.sequence
                    .checked_add(1)
                    .context("checkpoint sequence overflow")?,
                old.commitment(&domain),
                index.extension(old.height() + 1, self.archive.height)?,
                old.generation,
            )
        } else {
            (0, [0; 32], Vec::new(), initial_generation)
        };
        let generation = self
            .new_generation
            .as_deref()
            .map(decode::<32>)
            .transpose()?
            .unwrap_or(generation);
        let record = Checkpoint {
            sequence,
            previous,
            header: header.id(&network).unpack(),
            block: metadata.block.try_into()?,
            successor: metadata.successor.try_into()?,
            committee: decode(&metadata.committee)?,
            next_committee: decode(&metadata.next_committee)?,
            stake_snapshot,
            handoffs: decode(&metadata.handoffs)?,
            generation,
            history: index.frontier_at(self.archive.height)?,
        };
        let data = record.encode();
        let record = Checkpoint::decode(domain.network, &data)?;
        record.verify_parent(&domain, old.as_ref(), &predecessor, &suffix)?;
        print_json(&Proposal {
            network: encode(domain.network),
            l1_genesis: encode(domain.ckb_genesis),
            checkpoint_type: encode(domain.checkpoint_type),
            initial_generation: encode(initial_generation),
            height: record.height(),
            predecessor: encode(predecessor),
            data: encode(data),
            commitment: encode(record.commitment(&domain)),
            signing_message: encode(record.signing_message(&domain, &predecessor)),
            suffix: suffix.iter().map(encode).collect(),
        })
    }
}

impl Verify {
    fn run(self) -> Result<()> {
        let proposal: Proposal = read_json_limited(&self.proposal, 32 * 1024)?;
        let domain = Domain {
            network: decode(&self.network)?,
            ckb_genesis: decode(&self.l1_genesis)?,
            checkpoint_type: decode(&self.checkpoint_type)?,
        };
        let initial_generation = decode(&self.initial_generation)?;
        ensure!(
            decode::<32>(&proposal.network)? == domain.network
                && decode::<32>(&proposal.l1_genesis)? == domain.ckb_genesis
                && decode::<32>(&proposal.checkpoint_type)? == domain.checkpoint_type
                && decode::<32>(&proposal.initial_generation)? == initial_generation,
            "checkpoint domain differs from configured trust"
        );
        let predecessor = decode(&proposal.predecessor)?;
        let old = read_old(&domain, self.previous_data)?;
        let record = Checkpoint::decode(
            domain.network,
            &restricted_bytes(&proposal.data, Checkpoint::MAX_SIZE)?,
        )?;
        let update: String = read_json_limited(
            &self.update,
            2 * stem_stake::checkpoint::MAX_UPDATE_BYTES + 16,
        )?;
        let update = Update::decode(&restricted_bytes(
            &update,
            stem_stake::checkpoint::MAX_UPDATE_BYTES,
        )?)?;
        let suffix = proposal
            .suffix
            .iter()
            .map(|hash| decode::<32>(hash))
            .collect::<Result<Vec<_>>>()?;
        ensure!(
            proposal.height == record.height()
                && proposal.commitment == encode(record.commitment(&domain))
                && proposal.signing_message
                    == encode(record.signing_message(&domain, &predecessor))
                && suffix == update.suffix,
            "checkpoint proposal metadata differs from its canonical bytes"
        );
        update.verify(
            &domain,
            initial_generation,
            old.as_ref(),
            &record,
            &predecessor,
        )?;
        print_json(&json!({
            "height": record.height(), "commitment": proposal.commitment,
            "app_hash": encode(record.app_hash(&domain.network)),
        }))
    }
}

fn read_old(domain: &Domain, path: Option<PathBuf>) -> Result<Option<Checkpoint>> {
    path.map(|path| {
        let data: String = read_json_limited(&path, 2 * Checkpoint::MAX_SIZE + 16)?;
        Ok(Checkpoint::decode(
            domain.network,
            &restricted_bytes(&data, Checkpoint::MAX_SIZE)?,
        )?)
    })
    .transpose()
}

fn restricted_bytes(hex: &str, max: usize) -> Result<Vec<u8>> {
    let value = hex.strip_prefix("0x").unwrap_or(hex);
    ensure!(
        value.len() <= 2 * max,
        "encoded value exceeds its size limit"
    );
    Ok(hex::decode(value)?)
}

fn decode<const N: usize>(hex: &str) -> Result<[u8; N]> {
    restricted_bytes(hex, N)?
        .try_into()
        .ok()
        .with_context(|| format!("expected {N} bytes"))
}

fn encode(bytes: impl AsRef<[u8]>) -> String {
    format!("0x{}", hex::encode(bytes))
}
