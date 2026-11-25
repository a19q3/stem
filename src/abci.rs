use std::{
    path::Path,
    sync::{
        Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ckb_jsonrpc_types::{CellWithStatus, IndexerOrder, IndexerSearchKey, JsonBytes, Uint32};
use ckb_types::{
    core::cell::{CellProvider, CellStatus},
    packed::{Byte32, OutPoint},
    prelude::*,
};
use serde_json::json;
use tendermint_proto::{
    google::protobuf::Timestamp,
    v0_38::{abci::*, crypto::public_key::Sum},
};

use crate::{
    bridge,
    engine::{Block, Engine},
    genesis::{Genesis, script_consensus},
    snapshots::{Archive, Captured, Incoming},
    store::{Store, fatal},
};

// CometBFT's state-sync provider verifies H+1 for AppHash and H+2 for the next committee.
const STATE_SYNC_LOOKAHEAD: u64 = 2;

pub struct App {
    state: RwLock<Store>,
    engine: Engine,
    genesis: Genesis,
    proposal: Mutex<Option<Proposal>>,
    pending: Mutex<Option<Pending>>,
    archive: Archive,
    incoming: Mutex<Option<Incoming>>,
    retain_consensus_blocks: u64,
    metrics: Metrics,
}

struct Proposal {
    raw: Vec<Bytes>,
    block: Block,
}

struct Pending {
    block: Block,
    checkpoint: Option<Captured>,
}

#[derive(Default)]
struct Metrics {
    admitted: AtomicU64,
    rejected: AtomicU64,
    finalized: AtomicU64,
    sync_writes: AtomicU64,
    last_sync_micros: AtomicU64,
    sync_micros: AtomicU64,
    commit_wait_micros: AtomicU64,
    max_commit_wait_micros: AtomicU64,
    abci_flushes: AtomicU64,
    abci_flush_wait_micros: AtomicU64,
    max_abci_flush_wait_micros: AtomicU64,
}

impl App {
    pub fn record_abci_flush(&self, received: Instant) {
        let micros = received.elapsed().as_micros() as u64;
        let metrics = &self.metrics;
        metrics.abci_flushes.fetch_add(1, Ordering::Relaxed);
        metrics
            .abci_flush_wait_micros
            .fetch_add(micros, Ordering::Relaxed);
        metrics
            .max_abci_flush_wait_micros
            .fetch_max(micros, Ordering::Relaxed);
    }

    pub fn open(
        home: &Path,
        genesis: Genesis,
        workers: usize,
        retain_consensus_blocks: u64,
        rpc_index: bool,
        compact_transaction_history: bool,
    ) -> Result<Self> {
        let mut store = Store::open(&Archive::active_store(home)?, &genesis)?;
        if compact_transaction_history {
            store.prune_ordinary_history()?;
        }
        if rpc_index {
            store.enable_rpc_index()?;
        }
        let engine = Engine::new(script_consensus()?, genesis.epoch_length, workers)?;
        let archive = Archive::open(home)?;
        Ok(Self {
            state: RwLock::new(store),
            engine,
            genesis,
            proposal: Mutex::default(),
            pending: Mutex::default(),
            archive,
            incoming: Mutex::default(),
            retain_consensus_blocks,
            metrics: Metrics::default(),
        })
    }

    pub fn dispatch(&self, req: request::Value) -> response::Value {
        use request::Value;
        use response::Value as Out;
        match req {
            Value::Echo(r) => Out::Echo(ResponseEcho { message: r.message }),
            Value::Flush(_) => Out::Flush(ResponseFlush {}),
            Value::Info(_) => Out::Info(self.info()),
            Value::InitChain(r) => Out::InitChain(self.init_chain(r).unwrap_or_else(|e| fatal(e))),
            Value::Query(r) => Out::Query(self.query(r)),
            Value::CheckTx(r) => Out::CheckTx(self.check_tx(r)),
            Value::PrepareProposal(r) => {
                Out::PrepareProposal(self.prepare(r).unwrap_or_else(|e| fatal(e)))
            }
            Value::ProcessProposal(r) => Out::ProcessProposal(self.process(r)),
            Value::FinalizeBlock(r) => {
                Out::FinalizeBlock(self.finalize(r).unwrap_or_else(|e| fatal(e)))
            }
            Value::Commit(_) => Out::Commit(self.commit().unwrap_or_else(|e| fatal(e))),
            Value::ExtendVote(_) => Out::ExtendVote(ResponseExtendVote::default()),
            Value::VerifyVoteExtension(r) => {
                use response_verify_vote_extension::VerifyStatus;
                let status = if r.vote_extension.is_empty() {
                    VerifyStatus::Accept
                } else {
                    VerifyStatus::Reject
                };
                Out::VerifyVoteExtension(ResponseVerifyVoteExtension {
                    status: status as i32,
                })
            }
            Value::ListSnapshots(_) => Out::ListSnapshots(self.list_snapshots()),
            Value::OfferSnapshot(r) => Out::OfferSnapshot(self.offer_snapshot(r)),
            Value::LoadSnapshotChunk(r) => Out::LoadSnapshotChunk(ResponseLoadSnapshotChunk {
                chunk: self
                    .archive
                    .chunk(r.height, r.format, r.chunk)
                    .unwrap_or_default(),
            }),
            Value::ApplySnapshotChunk(r) => Out::ApplySnapshotChunk(self.apply_snapshot_chunk(r)),
        }
    }

    fn info(&self) -> ResponseInfo {
        let state = self.state.read().expect("state lock");
        ResponseInfo {
            data: "fern".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            app_version: 1,
            last_block_height: state.tip.number() as i64,
            last_block_app_hash: Bytes::copy_from_slice(state.app_hash.as_slice()),
        }
    }

    fn init_chain(&self, req: RequestInitChain) -> Result<ResponseInitChain> {
        let state = self.state.read().expect("state lock");
        ensure!(
            req.initial_height == 1
                && req.chain_id == self.genesis.chain_id
                && timestamp(req.time)? == self.genesis.timestamp_ms,
            "consensus genesis identity mismatch"
        );
        ensure!(
            Genesis::decode(&req.app_state_bytes)? == self.genesis,
            "consensus genesis state mismatch"
        );
        let mut validators = req
            .validators
            .into_iter()
            .map(|v| match v.pub_key.and_then(|pk| pk.sum) {
                Some(Sum::Ed25519(key)) => Ok((key, v.power)),
                _ => anyhow::bail!("expected Ed25519 validator key"),
            })
            .collect::<Result<Vec<_>>>()?;
        let mut expected: Vec<_> = self
            .genesis
            .validators
            .iter()
            .map(|v| (v.ed25519.as_bytes().to_vec(), v.power))
            .collect();
        validators.sort_unstable();
        expected.sort_unstable();
        ensure!(
            validators == expected,
            "consensus genesis committee mismatch"
        );
        Ok(ResponseInitChain {
            app_hash: Bytes::copy_from_slice(state.genesis_hash.as_slice()),
            ..Default::default()
        })
    }

    fn check_tx(&self, req: RequestCheckTx) -> ResponseCheckTx {
        let state = self.state.read().expect("state lock");
        match self.engine.check_tx(&state, req.tx) {
            Ok(cycles) => {
                if req.r#type == CheckTxType::New as i32 {
                    self.metrics.admitted.fetch_add(1, Ordering::Relaxed);
                }
                ResponseCheckTx {
                    gas_wanted: cycles as i64,
                    ..Default::default()
                }
            }
            Err(error) => {
                if req.r#type == CheckTxType::New as i32 {
                    self.metrics.rejected.fetch_add(1, Ordering::Relaxed);
                }
                ResponseCheckTx {
                    code: 1,
                    log: error.to_string(),
                    codespace: "ckb".to_owned(),
                    ..Default::default()
                }
            }
        }
    }

    fn prepare(&self, req: RequestPrepareProposal) -> Result<ResponsePrepareProposal> {
        let state = self.state.read().expect("state lock");
        let height = u64::try_from(req.height)?;
        let time = timestamp(req.time)?;
        let max_bytes = usize::try_from(req.max_tx_bytes)?;
        let raw = if self.genesis.checkpoint(height) {
            Vec::new()
        } else {
            req.txs
        };
        let (txs, block) = self.engine.prepare(&state, height, time, raw, max_bytes)?;
        *self.proposal.lock().expect("proposal lock") = Some(Proposal {
            raw: txs.clone(),
            block,
        });
        Ok(ResponsePrepareProposal { txs })
    }

    fn process(&self, req: RequestProcessProposal) -> ResponseProcessProposal {
        use response_process_proposal::ProposalStatus;
        let decision = self.verify_proposal(req);
        let status = match decision {
            Ok(()) => ProposalStatus::Accept,
            Err(error) => {
                eprintln!(
                    "level=warn event=proposal_rejected reason={:?}",
                    error.to_string()
                );
                ProposalStatus::Reject
            }
        };
        ResponseProcessProposal {
            status: status as i32,
        }
    }

    fn verify_proposal(&self, req: RequestProcessProposal) -> Result<()> {
        let state = self.state.read().expect("state lock");
        let height = u64::try_from(req.height)?;
        let time = timestamp(req.time)?;
        ensure!(
            !self.genesis.checkpoint(height) || req.txs.is_empty(),
            "checkpoint slot requires an empty block"
        );
        let mut proposal = self.proposal.lock().expect("proposal lock");
        if proposal
            .as_ref()
            .is_some_and(|p| p.matches(height, time, &req.txs))
        {
            return Ok(());
        }
        let raw = req.txs;
        let block = self.engine.verify(&state, height, time, raw.clone())?;
        *proposal = Some(Proposal { raw, block });
        Ok(())
    }

    fn finalize(&self, req: RequestFinalizeBlock) -> Result<ResponseFinalizeBlock> {
        let state = self.state.read().expect("state lock");
        let height = u64::try_from(req.height)?;
        let time = timestamp(req.time)?;
        let is_checkpoint = self.genesis.checkpoint(height);
        ensure!(
            !is_checkpoint || req.txs.is_empty(),
            "finalized checkpoint block contains transactions"
        );
        let proposal = self.proposal.lock().expect("proposal lock").take();
        let block = match proposal {
            Some(p) if p.matches(height, time, &req.txs) => p.block,
            _ => self
                .engine
                .verify(&state, height, time, req.txs)
                .context("execute finalized block during replay")?,
        };
        ensure!(
            block.header.parent_hash() == state.tip.hash(),
            "finalized block has the wrong parent"
        );
        let mut next_withdrawal = state.next_withdrawal;
        let tx_results = block
            .transactions
            .iter()
            .zip(&block.cycles)
            .map(|(tx, cycles)| -> Result<ExecTxResult> {
                let mut data = tx.hash().as_slice().to_vec();
                if self.genesis.authenticate_withdrawals {
                    let count = bridge::withdrawal_outputs(tx).count() as u64;
                    if count != 0 {
                        data.extend_from_slice(&next_withdrawal.to_le_bytes());
                        next_withdrawal = next_withdrawal
                            .checked_add(count)
                            .context("withdrawal counter overflow")?;
                    }
                }
                Ok(ExecTxResult {
                    gas_wanted: *cycles as i64,
                    gas_used: *cycles as i64,
                    data: data.into(),
                    ..Default::default()
                })
            })
            .collect::<Result<_>>()?;
        let checkpoint = if is_checkpoint {
            let start = std::time::Instant::now();
            let image = self.archive.capture(&state, block.header.clone())?;
            eprintln!(
                "level=info event=checkpoint_prepared height={height} archive_bytes={} capture_micros={} live_cell_proofs={}",
                image.image_size(),
                start.elapsed().as_micros(),
                self.genesis.checkpoint_cell_proofs,
            );
            Some(image)
        } else {
            None
        };
        let hash = checkpoint.as_ref().map_or_else(
            || state.next_app_hash(&block.header, !block.transactions.is_empty()),
            Captured::app_hash,
        );
        let app_hash = Bytes::copy_from_slice(hash.as_slice());
        let validator_updates = state.committee_updates(&block.transactions)?;
        let mut pending = self.pending.lock().expect("pending commit lock");
        ensure!(
            pending.is_none(),
            "finalized block has an outstanding commit"
        );
        *pending = Some(Pending { block, checkpoint });
        Ok(ResponseFinalizeBlock {
            tx_results,
            app_hash,
            validator_updates,
            ..Default::default()
        })
    }

    fn commit(&self) -> Result<ResponseCommit> {
        let waiting = Instant::now();
        let mut state = self.state.write().expect("state lock");
        let commit_wait_micros = waiting.elapsed().as_micros() as u64;
        self.metrics
            .commit_wait_micros
            .fetch_add(commit_wait_micros, Ordering::Relaxed);
        self.metrics
            .max_commit_wait_micros
            .fetch_max(commit_wait_micros, Ordering::Relaxed);
        let Pending { block, checkpoint } = self
            .pending
            .lock()
            .expect("pending commit lock")
            .take()
            .context("commit without finalized block")?;
        let count = block.transactions.len();
        let latency = if let Some(checkpoint) = checkpoint {
            let app_hash = checkpoint.app_hash();
            let latency =
                state.commit_with_hash(block.header, &block.transactions, block.cells, app_hash)?;
            if let Err(error) = self.archive.publish(checkpoint) {
                eprintln!(
                    "level=warn event=snapshot_publish_failed reason={:?}",
                    error.to_string()
                );
            }
            latency
        } else {
            state.commit(block.header, &block.transactions, block.cells)?
        };
        let sync_micros = latency.as_micros() as u64;
        self.metrics
            .finalized
            .fetch_add(count as u64, Ordering::Relaxed);
        self.metrics.sync_writes.fetch_add(1, Ordering::Relaxed);
        self.metrics
            .last_sync_micros
            .store(sync_micros, Ordering::Relaxed);
        self.metrics
            .sync_micros
            .fetch_add(sync_micros, Ordering::Relaxed);
        if count > 0 {
            eprintln!(
                "level=info event=block_committed height={} transactions={count} app_hash={} sync_micros={sync_micros} commit_wait_micros={commit_wait_micros}",
                state.tip.number(),
                state.app_hash,
            );
        }
        let retain_height = if self.retain_consensus_blocks > 0 {
            let height = state.tip.number();
            self.archive
                .list(height.saturating_sub(STATE_SYNC_LOOKAHEAD))
                .ok()
                .and_then(|snapshots| {
                    snapshots.last().map(|oldest| {
                        height
                            .saturating_sub(self.retain_consensus_blocks - 1)
                            .min(oldest.height)
                    })
                })
                .unwrap_or(0) as i64
        } else {
            0
        };
        Ok(ResponseCommit { retain_height })
    }

    fn list_snapshots(&self) -> ResponseListSnapshots {
        let state = self.state.read().expect("state lock");
        let snapshots = self
            .archive
            .list(state.tip.number().saturating_sub(STATE_SYNC_LOOKAHEAD))
            .unwrap_or_else(|error| {
                eprintln!(
                    "level=warn event=snapshot_list_failed reason={:?}",
                    error.to_string()
                );
                Vec::new()
            });
        ResponseListSnapshots { snapshots }
    }

    fn offer_snapshot(&self, req: RequestOfferSnapshot) -> ResponseOfferSnapshot {
        use response_offer_snapshot::Result as Status;
        let state = self.state.read().expect("state lock");
        let status = match req.snapshot {
            Some(snapshot)
                if state.tip.number() < snapshot.height
                    || (state.tip.number() == snapshot.height
                        && state.app_hash.as_slice() == req.app_hash) =>
            {
                match self
                    .archive
                    .incoming(&self.genesis, snapshot, &req.app_hash)
                {
                    Ok(incoming) => {
                        *self.incoming.lock().expect("snapshot lock") = Some(incoming);
                        Status::Accept
                    }
                    Err(error) => {
                        eprintln!(
                            "level=warn event=snapshot_offer_rejected reason={:?}",
                            error.to_string()
                        );
                        Status::Reject
                    }
                }
            }
            _ => Status::Reject,
        };
        ResponseOfferSnapshot {
            result: status as i32,
        }
    }

    fn apply_snapshot_chunk(&self, req: RequestApplySnapshotChunk) -> ResponseApplySnapshotChunk {
        use response_apply_snapshot_chunk::Result as Status;
        let mut state = self.state.write().expect("state lock");
        let mut incoming = self.incoming.lock().expect("snapshot lock");
        let result = (|| -> Result<()> {
            let image = incoming
                .as_mut()
                .context("received chunk without a snapshot offer")?;
            ensure!(
                state.tip.number() <= image.height(),
                "snapshot would rewind the local ledger"
            );
            if image.apply(req.index, &req.chunk)? {
                let loaded = self.archive.install(
                    incoming.take().expect("completed snapshot"),
                    &self.genesis,
                    &self.engine,
                    state.rpc_index_enabled,
                    state.compact_transaction_history,
                )?;
                eprintln!(
                    "level=info event=snapshot_installed height={}",
                    loaded.tip.number()
                );
                *state = loaded;
            }
            Ok(())
        })();
        let status = match result {
            Ok(()) => Status::Accept,
            Err(error) => {
                *incoming = None;
                eprintln!(
                    "level=warn event=snapshot_rejected reason={:?}",
                    error.to_string()
                );
                Status::RejectSnapshot
            }
        };
        ResponseApplySnapshotChunk {
            result: status as i32,
            ..Default::default()
        }
    }

    fn query(&self, req: RequestQuery) -> ResponseQuery {
        let state = self.state.read().expect("state lock");
        let height = state.tip.number() as i64;
        let result = if req.prove || (req.height != 0 && req.height != height) {
            Err(anyhow::anyhow!(
                "query requires the latest height and prove=false"
            ))
        } else {
            self.query_value(&state, &req.path, &req.data)
        };
        match result {
            Ok(value) => ResponseQuery {
                height,
                key: req.data,
                value,
                ..Default::default()
            },
            Err(error) => ResponseQuery {
                height,
                code: 1,
                log: error.to_string(),
                ..Default::default()
            },
        }
    }

    fn query_value(&self, state: &Store, path: &str, key: &[u8]) -> Result<Bytes> {
        let bytes = match path {
            "/tip" => serde_json::to_vec(&json!({
                "chain_id": self.genesis.chain_id,
                "height": state.tip.number(),
                "timestamp_ms": state.tip.timestamp(),
                "app_hash": format!("{:#x}", state.app_hash),
                "header_hash": format!("{:#x}", state.tip.hash()),
                "genesis_hash": format!("{:#x}", state.genesis_hash),
            }))?,
            "/metrics" => {
                let m = &self.metrics;
                let (script_checks, script_cache_hits) = self.engine.script_metrics();
                serde_json::to_vec(&json!({
                    "admitted": m.admitted.load(Ordering::Relaxed),
                    "rejected": m.rejected.load(Ordering::Relaxed),
                    "finalized": m.finalized.load(Ordering::Relaxed),
                    "sync_writes": m.sync_writes.load(Ordering::Relaxed),
                    "last_sync_micros": m.last_sync_micros.load(Ordering::Relaxed),
                    "sync_micros": m.sync_micros.load(Ordering::Relaxed),
                    "commit_wait_micros": m.commit_wait_micros.load(Ordering::Relaxed),
                    "max_commit_wait_micros": m.max_commit_wait_micros.load(Ordering::Relaxed),
                    "abci_flushes": m.abci_flushes.load(Ordering::Relaxed),
                    "abci_flush_wait_micros": m.abci_flush_wait_micros.load(Ordering::Relaxed),
                    "max_abci_flush_wait_micros": m.max_abci_flush_wait_micros.load(Ordering::Relaxed),
                    "script_checks": script_checks,
                    "script_cache_hits": script_cache_hits,
                }))?
            }
            "/committee" => {
                if let Some(point) = &state.governance {
                    let CellStatus::Live(cell) = state.cell(point, true) else {
                        fatal("missing live governance Cell");
                    };
                    let members =
                        crate::governance::decode(&cell.mem_cell_data.expect("governance data"))?;
                    let height = cell
                        .transaction_info
                        .expect("governance origin")
                        .block_number;
                    serde_json::to_vec(&json!({
                        "members": members,
                        "out_point": ckb_jsonrpc_types::OutPoint::from(point.clone()),
                        "effective_height": if height == 0 { 1 } else { height + 2 }
                    }))?
                } else {
                    serde_json::to_vec(
                        &json!({"members": self.genesis.validators, "effective_height": 1}),
                    )?
                }
            }
            "/bridge_guardians" => {
                serde_json::to_vec(state.guardians.as_ref().context("bridge is disabled")?)?
            }
            "/storage_stats" => serde_json::to_vec(&state.storage_stats()?)?,
            "/rpc/tip_header" => state.tip.data().as_slice().to_vec(),
            "/rpc/live_cell" => {
                let (with_data, out_point) =
                    key.split_first().context("missing live Cell query")?;
                ensure!(*with_data <= 1, "invalid data flag");
                let point = OutPoint::from_slice(out_point)?;
                let mut result = CellWithStatus::from(state.cell(&point, *with_data == 1));
                if *with_data == 0
                    && let Some(cell) = &mut result.cell
                {
                    cell.data = None;
                }
                serde_json::to_vec(&result)?
            }
            "/rpc/live_cell_proof" => {
                let (height, point) = key
                    .split_at_checked(8)
                    .context("missing checkpoint height")?;
                let height = u64::from_be_bytes(height.try_into()?);
                ensure!(
                    self.genesis.checkpoint_cell_proofs
                        && self.genesis.checkpoint(height)
                        && height <= state.tip.number(),
                    "height is not a committed Live Cell checkpoint"
                );
                let point = OutPoint::from_slice(point)?;
                serde_json::to_vec(&self.archive.cell_proof(height, &point)?)?
            }
            "/rpc/transaction" => state
                .transaction_or_origin(&Byte32::from_slice(key)?)
                .context("unknown transaction")?,
            "/rpc/header_at" => {
                let number = u64::from_be_bytes(key.try_into()?);
                let hash = state
                    .rpc_index()?
                    .header_hash(number)?
                    .context("unknown header")?;
                state
                    .header(&hash)
                    .context("indexed header is missing")?
                    .data()
                    .as_slice()
                    .to_vec()
            }
            "/rpc/indexer_tip" => {
                state.rpc_index()?;
                serde_json::to_vec(&crate::index::tip(&state.tip))?
            }
            "/rpc/cells" => {
                let (search, order, limit, after): (
                    IndexerSearchKey,
                    IndexerOrder,
                    Uint32,
                    Option<JsonBytes>,
                ) = serde_json::from_slice(key)?;
                let index = state.rpc_index()?;
                serde_json::to_vec(&index.cells(
                    search,
                    order,
                    limit.value() as usize,
                    after,
                    |point| state.indexed_cell(point),
                )?)?
            }
            "/rpc/cells_capacity" => {
                let search: IndexerSearchKey = serde_json::from_slice(key)?;
                let index = state.rpc_index()?;
                serde_json::to_vec(
                    &index.capacity(search, &state.tip, |point| state.indexed_cell(point))?,
                )?
            }
            "/header" => state
                .header(&Byte32::from_slice(key)?)
                .context("unknown header")?
                .data()
                .as_slice()
                .to_vec(),
            "/transaction" => state
                .transaction(&Byte32::from_slice(key)?)
                .context("unknown transaction")?
                .to_vec(),
            "/withdrawal" => {
                let bridge_config = state.bridge.as_ref().context("peg-out is disabled")?;
                let nonce = u64::from_be_bytes(key.try_into()?);
                let point = state
                    .withdrawal(nonce)
                    .context("unknown withdrawal sequence")?;
                let raw = state
                    .transaction(&point.tx_hash())
                    .context("missing withdrawal transaction")?;
                let tx = ckb_types::packed::Transaction::from_slice(&raw[32..])?.into_view();
                let index = u32::from(point.index()) as usize;
                let output = tx.output(index).context("missing burn Cell")?;
                let data = tx
                    .outputs_data()
                    .get(index)
                    .context("missing payout memo")?;
                let payout =
                    bridge_config.payout(&output, &data.raw_data(), &state.genesis_hash)?;
                let recipient: ckb_jsonrpc_types::Script = payout.recipient.into();
                let asset = payout.units.and(bridge_config.xudt.as_ref());
                serde_json::to_vec(&json!({
                    "nonce": nonce,
                    "withdrawal_id": format!("{:#x}", bridge::withdrawal_id(&bridge_config.vault_type_hash, nonce)),
                    "out_point": ckb_jsonrpc_types::OutPoint::from(point),
                    "block_hash": format!("{:#x}", Byte32::from_slice(&raw[..32])?),
                    "capacity": u64::from(output.capacity()),
                    "l1_lock": recipient,
                    "l1_type": asset.map(|asset| &asset.l1_type),
                    "units": payout.units.map(|units| units.to_string()),
                }))?
            }
            "/live_cell" => {
                let point = OutPoint::from_slice(key)?;
                let CellStatus::Live(cell) = state.cell(&point, true) else {
                    anyhow::bail!("cell is not live");
                };
                let info = cell.transaction_info.context("missing cell origin")?;
                let output: ckb_jsonrpc_types::CellOutput = cell.cell_output.into();
                serde_json::to_vec(&json!({
                    "output": output,
                    "data": JsonBytes::from_bytes(cell.mem_cell_data.context("missing cell data")?),
                    "block_hash": format!("{:#x}", info.block_hash),
                    "block_number": info.block_number,
                }))?
            }
            _ => anyhow::bail!("unknown query path"),
        };
        Ok(bytes.into())
    }
}

impl Proposal {
    fn matches(&self, height: u64, time: u64, raw: &[Bytes]) -> bool {
        self.block.header.number() == height
            && self.block.header.timestamp() == time
            && self.raw == raw
    }
}

fn timestamp(time: Option<Timestamp>) -> Result<u64> {
    let Timestamp { seconds, nanos } = time.context("missing consensus timestamp")?;
    ensure!(
        (0..1_000_000_000).contains(&nanos),
        "invalid timestamp nanoseconds"
    );
    u64::try_from(seconds)?
        .checked_mul(1000)
        .and_then(|ms| ms.checked_add(nanos as u64 / 1_000_000))
        .context("timestamp overflow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_snapshot_after_two_successors() {
        let mut genesis = crate::tests::payments(1).genesis;
        genesis.checkpoint_interval = 4;
        genesis.checkpoint_cell_proofs = true;
        genesis.checkpoint_compact_transactions = true;
        let seconds = (genesis.timestamp_ms / 1000) as i64;
        let home = tempfile::tempdir().unwrap();
        let app = App::open(home.path(), genesis, 1, 0, false, false).unwrap();
        for height in 1..7 {
            app.finalize(RequestFinalizeBlock {
                height,
                time: Some(Timestamp {
                    seconds: seconds + height,
                    nanos: 0,
                }),
                ..Default::default()
            })
            .unwrap();
            app.commit().unwrap();
            if height == 4 {
                assert_eq!(app.archive.list(4).unwrap()[0].format, 3);
            }
            let offered = app.list_snapshots().snapshots;
            if height < 6 {
                assert!(offered.is_empty());
            } else {
                assert_eq!(offered.len(), 1);
                assert_eq!(offered[0].height, 4);
            }
        }
    }
}

impl fern::socket::Handler for App {
    fn dispatch(&self, request: request::Value) -> response::Value {
        App::dispatch(self, request)
    }

    fn record_abci_flush(&self, received: Instant) {
        App::record_abci_flush(self, received);
    }
}
