use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{HeaderView, Uint64};
use ckb_types::{H256, packed::Transaction, prelude::*};
use clap::Args;
use serde_json::json;
use stem::security_audit::{self, Entry, Scope, Through};
use tempfile::NamedTempFile;

use crate::{
    bridge_cli::{print_json, read_json_limited},
    l1::Rpc,
};

const MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRANSACTION_BYTES: usize = 20 * 1024 * 1024;
const MAX_TRANSITIONS: u32 = 2048;

#[derive(Args)]
pub struct Collect {
    #[arg(long)]
    l1_rpc: String,
    /// JSON scope containing the pinned chain, domain identity and target.
    #[arg(long)]
    scope: PathBuf,
    /// Minimum number of CKB blocks after the selected transition.
    #[arg(long)]
    confirmations: u64,
    #[arg(long, default_value_t = 256, value_parser = clap::value_parser!(u32).range(1..=i64::from(MAX_TRANSITIONS)))]
    max_transactions: u32,
    /// Require the selected Cell to be live at the observing CKB node.
    #[arg(long)]
    require_live: bool,
    /// Write the inclusion proofs to a new file for offline verification.
    #[arg(long)]
    proof_output: Option<PathBuf>,
}

#[derive(Args)]
pub struct Verify {
    #[arg(long)]
    scope: PathBuf,
    #[arg(long)]
    proof: PathBuf,
    /// JSON map from decimal block heights to caller-authenticated block hashes.
    #[arg(long)]
    anchors: PathBuf,
}

impl Collect {
    pub(super) fn run(self) -> Result<()> {
        let scope: Scope = read_json_limited(&self.scope, 4096)?;
        let live = match (&scope.through, self.require_live) {
            (Through::Cell(point), true) => Some(point),
            (Through::Closure(_), true) => anyhow::bail!("closure has no live CKB domain Cell"),
            (_, false) => None,
        };
        let rpc = Rpc::new(&self.l1_rpc)?;
        let mut blocks = BTreeMap::from([(0, rpc.block_hash(0)?)]);
        ensure!(blocks[&0] == scope.ckb_genesis, "wrong trusted CKB genesis");
        let mut hash = scope.through.transaction().clone();
        let mut history = Vec::new();
        let mut transactions = BTreeSet::new();
        let mut used_bytes = 0;
        let mut head = None;
        loop {
            ensure!(
                history.len() < self.max_transactions as usize,
                "CKB security domain audit transaction budget exhausted"
            );
            ensure!(
                transactions.insert(hash.clone()),
                "cyclic security domain history"
            );
            let (transaction, height, block) = rpc.committed(&hash)?;
            used_bytes += transaction.as_bytes().len();
            ensure!(
                used_bytes <= MAX_TRANSACTION_BYTES,
                "CKB security domain audit byte budget exhausted"
            );
            let (head_height, _) = head.get_or_insert((height, block.clone()));
            ensure!(
                height <= *head_height,
                "CKB security domain predecessor is newer than the target"
            );
            if let Some(existing) = blocks.insert(height, block.clone()) {
                ensure!(existing == block, "multiple CKB blocks at one audit height");
            }
            let header: HeaderView = rpc.call("get_header", json!([block]))?;
            ensure!(
                header.inner.number.value() == height && header.hash == block,
                "CKB audit Header differs from committed status"
            );
            let proof = rpc.call("get_transaction_and_witness_proof", json!([[hash], block]))?;
            let previous =
                scope.predecessor(&Transaction::from_slice(transaction.as_bytes())?.into_view())?;
            history.push(Entry {
                header,
                transaction,
                proof,
            });
            let Some(previous) = previous else { break };
            hash = previous;
        }
        history.reverse();
        let (height, block) = head.context("missing CKB security domain target")?;
        let tip: Uint64 = rpc.call("get_tip_block_number", json!([]))?;
        ensure!(
            tip.value()
                .checked_sub(height)
                .is_some_and(|depth| depth >= self.confirmations),
            "CKB security domain target lacks required confirmations"
        );
        for (height, hash) in &blocks {
            ensure!(
                rpc.block_hash(*height)? == *hash,
                "CKB security domain history was reorganized"
            );
        }
        let report = security_audit::verify(&scope, &history, |height| {
            blocks
                .get(&height)
                .cloned()
                .context("missing observed CKB audit block hash")
        })?;
        if let Some(point) = live {
            rpc.live_cell(point)?;
        }
        ensure!(
            rpc.block_hash(height)? == block,
            "CKB security domain target was reorganized"
        );
        if let Some(path) = self.proof_output {
            let parent = path.parent().filter(|dir| !dir.as_os_str().is_empty());
            let mut file = NamedTempFile::new_in(parent.unwrap_or_else(|| Path::new(".")))?;
            {
                let mut out = BufWriter::new(file.as_file_mut());
                serde_json::to_writer(&mut out, &history)?;
                out.write_all(b"\n")?;
                out.flush()?;
            }
            file.persist_noclobber(path)
                .context("publish new security audit proof")?;
        }
        print_json(&report)
    }
}

impl Verify {
    pub(super) fn run(self) -> Result<()> {
        let scope: Scope = read_json_limited(&self.scope, 4096)?;
        let anchors: BTreeMap<u64, H256> = read_json_limited(&self.anchors, 512 * 1024)?;
        let history: Vec<Entry> = read_json_limited(&self.proof, MAX_FILE_BYTES)?;
        ensure!(
            history.len() <= MAX_TRANSITIONS as usize,
            "CKB security domain audit transaction budget exhausted"
        );
        let report = security_audit::verify(&scope, &history, |height| {
            anchors
                .get(&height)
                .cloned()
                .with_context(|| format!("missing trusted CKB audit block hash at height {height}"))
        })?;
        print_json(&report)
    }
}
