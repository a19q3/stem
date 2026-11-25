//! CometBFT lifecycle for the native state machine.

use std::{
    path::Path,
    sync::{Mutex, RwLock},
};

use anyhow::{Context, Result, ensure};
use bytes::Bytes;
use ckb_types::prelude::Entity;
use serde::Deserialize;
use tendermint_proto::{
    google::protobuf::Timestamp,
    v0_38::{
        abci::{self, request::Value, response::Value as Out},
        crypto::public_key::Sum,
    },
};

use crate::{
    native_store::{Prepared, Store},
    socket::Handler,
};
use stem_ledger::Transaction;

mod snapshots;

pub struct App {
    snapshots: snapshots::Snapshots,
    workers: rayon::ThreadPool,
    store: RwLock<Store>,
    candidate: Mutex<Option<Candidate>>,
    pending: Mutex<Option<Prepared>>,
}

struct Candidate {
    height: u64,
    timestamp: u64,
    raw: Vec<Bytes>,
    block: Prepared,
}

impl App {
    pub fn new(
        store: Store,
        workers: usize,
        archive: &Path,
        interval: u64,
        max_bytes: u64,
    ) -> Result<Self> {
        ensure!(
            (1..=64).contains(&workers),
            "VM workers must be between 1 and 64"
        );
        ensure!(
            !store.genesis_validators().is_empty(),
            "missing genesis committee"
        );
        Ok(Self {
            snapshots: snapshots::Snapshots::open(
                archive,
                store.snapshot_reader(),
                interval,
                max_bytes,
            )?,
            workers: rayon::ThreadPoolBuilder::new()
                .num_threads(workers)
                .thread_name(|index| format!("stem-vm-{index}"))
                .build()?,
            store: RwLock::new(store),
            candidate: Mutex::new(None),
            pending: Mutex::new(None),
        })
    }

    fn info(&self) -> abci::ResponseInfo {
        let store = self.store.read().expect("state lock");
        abci::ResponseInfo {
            data: "Stem native ledger".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            app_version: 1,
            last_block_height: store
                .tip()
                .context
                .height
                .try_into()
                .expect("CometBFT height"),
            last_block_app_hash: store.tip().app_hash(store.network()).as_bytes(),
        }
    }

    fn init(&self, request: abci::RequestInitChain) -> Result<abci::ResponseInitChain> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct AppState {
            stem_genesis: String,
        }

        let store = self.store.read().expect("state lock");
        ensure!(
            request.initial_height == 1
                && store.tip().context.height == 0
                && request.chain_id == store.chain_id()
                && timestamp(request.time)? == store.tip().context.timestamp_ms,
            "CometBFT genesis does not match Stem genesis"
        );
        let app_state: AppState = serde_json::from_slice(&request.app_state_bytes)?;
        ensure!(
            hex::decode(&app_state.stem_genesis)?.as_slice() == store.genesis_id().as_slice(),
            "CometBFT app_state does not commit to this Stem genesis"
        );
        let mut validators = request
            .validators
            .into_iter()
            .map(|update| {
                let Some(Sum::Ed25519(key)) = update.pub_key.and_then(|key| key.sum) else {
                    anyhow::bail!("expected CometBFT Ed25519 key");
                };
                Ok((key.as_slice().try_into()?, update.power))
            })
            .collect::<Result<Vec<([u8; 32], i64)>>>()?;
        validators.sort_unstable();
        ensure!(
            validators == store.genesis_validators(),
            "CometBFT genesis committee differs from Stem genesis"
        );
        Ok(abci::ResponseInitChain {
            app_hash: store.tip().app_hash(store.network()).as_bytes(),
            ..Default::default()
        })
    }

    fn propose(
        &self,
        request: abci::RequestPrepareProposal,
    ) -> Result<abci::ResponsePrepareProposal> {
        let store = self.store.read().expect("state lock");
        let height = height(&store, request.height)?;
        let timestamp = timestamp(request.time)?;
        let txs = request
            .txs
            .iter()
            .filter_map(|raw| Transaction::from_slice(raw).ok())
            .collect();
        let max_tx_bytes = u64::try_from(request.max_tx_bytes)?;
        let selected = self
            .workers
            .install(|| store.select(txs, timestamp, max_tx_bytes))?;
        let raw: Vec<_> = selected
            .iter()
            .map(|tx| Bytes::copy_from_slice(tx.as_slice()))
            .collect();
        let block = self
            .workers
            .install(|| store.prepare(&selected, timestamp, true))?;
        *self.candidate.lock().expect("candidate lock") = Some(Candidate {
            height,
            timestamp,
            raw: raw.clone(),
            block,
        });
        Ok(abci::ResponsePrepareProposal { txs: raw })
    }

    fn process(&self, request: abci::RequestProcessProposal) -> Result<()> {
        let store = self.store.read().expect("state lock");
        let height = height(&store, request.height)?;
        let timestamp = timestamp(request.time)?;
        let mut candidate = self.candidate.lock().expect("candidate lock");
        if candidate
            .as_ref()
            .is_some_and(|block| block.matches(height, timestamp, &request.txs))
        {
            return Ok(());
        }
        let block = self
            .workers
            .install(|| store.prepare(&decode(&request.txs)?, timestamp, true))?;
        *candidate = Some(Candidate {
            height,
            timestamp,
            raw: request.txs,
            block,
        });
        Ok(())
    }

    fn finalize(&self, request: abci::RequestFinalizeBlock) -> Result<abci::ResponseFinalizeBlock> {
        let store = self.store.read().expect("state lock");
        let height = height(&store, request.height)?;
        let timestamp = timestamp(request.time)?;
        let candidate = self.candidate.lock().expect("candidate lock").take();
        let block = match candidate {
            Some(candidate) if candidate.matches(height, timestamp, &request.txs) => {
                candidate.block
            }
            _ => self
                .workers
                .install(|| store.prepare(&decode(&request.txs)?, timestamp, true))?,
        };
        let tx_results = block
            .receipts()
            .iter()
            .map(|receipt| {
                let cycles = i64::try_from(receipt.cycles)?;
                Ok(abci::ExecTxResult {
                    data: receipt.encode(),
                    gas_wanted: cycles,
                    gas_used: cycles,
                    ..Default::default()
                })
            })
            .collect::<Result<_>>()?;
        let validator_updates = block
            .validator_updates()
            .iter()
            .map(|member| {
                Ok(abci::ValidatorUpdate {
                    pub_key: Some(tendermint_proto::v0_38::crypto::PublicKey {
                        sum: Some(Sum::Ed25519(member.key.to_vec())),
                    }),
                    power: i64::try_from(member.power)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let app_hash = block.header().app_hash(store.network()).as_bytes();
        let mut pending = self.pending.lock().expect("commit lock");
        ensure!(pending.is_none(), "previous FinalizeBlock awaits Commit");
        *pending = Some(block);
        Ok(abci::ResponseFinalizeBlock {
            tx_results,
            validator_updates,
            app_hash,
            ..Default::default()
        })
    }

    fn commit(&self) -> Result<abci::ResponseCommit> {
        let mut store = self.store.write().expect("state lock");
        let block = self
            .pending
            .lock()
            .expect("commit lock")
            .take()
            .context("Commit without FinalizeBlock")?;
        store.commit(block)?;
        self.snapshots.committed(store.tip().context.height);
        Ok(abci::ResponseCommit {
            retain_height: store.consensus_retain_height().try_into()?,
        })
    }

    fn query(&self, request: abci::RequestQuery) -> Result<abci::ResponseQuery> {
        use ckb_types::packed::OutPoint;
        use stem_ledger::{proof::Tree, record};
        let store = self.store.read().expect("state lock");
        let tip = store.tip().context.height;
        let requested = if request.height == 0 {
            tip
        } else {
            request.height.try_into()?
        };
        let value = match request.path.as_str() {
            "/stem/header" if !request.prove => store
                .header(requested)?
                .context("Header outside local retention window")?
                .encode(),
            "/stem/stake-retirement" if request.prove => {
                ensure!(
                    requested == tip,
                    "Stake retirement queries require the current height"
                );
                let validator = request.data.as_ref().try_into()?;
                let (height, proof) = store.prove_stake_retirement(validator)?;
                serde_json::to_vec(&serde_json::json!({
                    "effective_height": height,
                    "nodes": proof.nodes.iter().map(hex::encode).collect::<Vec<_>>(),
                }))?
                .into()
            }
            "/stem/stake-snapshot" if request.prove && request.data.is_empty() => {
                ensure!(
                    requested == tip,
                    "Stake Snapshot queries require the current height"
                );
                let (snapshot, proof) = store.prove_stake_snapshot()?;
                serde_json::to_vec(&serde_json::json!({
                    "snapshot": snapshot.map(hex::encode),
                    "nodes": proof.nodes.iter().map(hex::encode).collect::<Vec<_>>(),
                }))?
                .into()
            }
            "/stem/extinguished-fees" if request.prove && request.data.is_empty() => {
                ensure!(
                    requested == tip,
                    "fee proof queries require the current height"
                );
                let (amount, proof) = store.prove_extinguished_fees()?;
                serde_json::to_vec(&serde_json::json!({
                    "extinguished_shannons": amount.map(ckb_jsonrpc_types::Uint64::from),
                    "proof": format!("0x{}", hex::encode(proof.encode()?)),
                }))?
                .into()
            }
            "/stem/bridge-backing" if request.prove && request.data.is_empty() => {
                ensure!(
                    requested == tip,
                    "bridge backing proof requires the current height"
                );
                let (backing, path) = store.prove_bridge_backing()?;
                serde_json::to_vec(&serde_json::json!({
                    "record": format!("0x{}", hex::encode(backing.encode())),
                    "proof": format!("0x{}", hex::encode(path.encode()?)),
                    "liquid_shannons": ckb_jsonrpc_types::Uint64::from(backing.liquid()),
                    "circulating_shannons": ckb_jsonrpc_types::Uint64::from(backing.circulating()),
                    "pending_shannons": ckb_jsonrpc_types::Uint64::from(backing.pending()),
                    "unallocated_shannons": ckb_jsonrpc_types::Uint64::from(backing.unallocated()),
                }))?
                .into()
            }
            "/stem/cell" => {
                ensure!(
                    requested == tip,
                    "Live Cell queries require the current height"
                );
                let point = OutPoint::from_slice(&request.data)?;
                let (cell, proof) = if request.prove {
                    let (cell, proof) = store.prove_cell(&point)?;
                    (cell, Some(format!("0x{}", hex::encode(proof.encode()?))))
                } else {
                    (store.live_cell(&point)?, None)
                };
                let cell = cell
                    .as_ref()
                    .map(|cell| hex::encode(record::encode_cell(&point, cell)));
                serde_json::to_vec(&serde_json::json!({"cell": cell, "proof": proof}))?.into()
            }
            "/stem/execution" => {
                let index = u32::from_le_bytes(request.data.as_ref().try_into()?);
                let (transactions, receipts) = store
                    .execution(requested)?
                    .context("execution outside local retention window")?;
                let tx = transactions
                    .get(index as usize)
                    .context("transaction index out of range")?;
                let receipt = receipts
                    .get(index as usize)
                    .context("receipt index out of range")?;
                let proof = if request.prove {
                    let tx =
                        Tree::transactions(store.network().clone(), &transactions)?.prove(index)?;
                    let receipt =
                        Tree::receipts(store.network().clone(), &receipts)?.prove(index)?;
                    Some(format!(
                        "0x{}",
                        hex::encode(tx.encode_execution_pair(&receipt)?)
                    ))
                } else {
                    None
                };
                serde_json::to_vec(&serde_json::json!({
                    "transaction": hex::encode(tx.as_slice()), "receipt": hex::encode(receipt.encode()),
                    "proof": proof,
                }))?.into()
            }
            _ => anyhow::bail!("unsupported ABCI query"),
        };
        Ok(abci::ResponseQuery {
            value,
            height: requested.try_into()?,
            ..Default::default()
        })
    }

    fn check(&self, request: abci::RequestCheckTx) -> Result<abci::ResponseCheckTx> {
        let tx = Transaction::from_slice(&request.tx)?;
        let store = self.store.read().expect("state lock");
        let receipt = store.check(&tx)?;
        Ok(abci::ResponseCheckTx {
            gas_wanted: i64::try_from(receipt.cycles)?,
            ..Default::default()
        })
    }
}

impl Handler for App {
    fn dispatch(&self, request: Value) -> Out {
        match request {
            Value::Echo(r) => Out::Echo(abci::ResponseEcho { message: r.message }),
            Value::Flush(_) => Out::Flush(abci::ResponseFlush {}),
            Value::Info(_) => Out::Info(self.info()),
            Value::InitChain(r) => reply(self.init(r), Out::InitChain),
            Value::CheckTx(r) => {
                Out::CheckTx(self.check(r).unwrap_or_else(|error| abci::ResponseCheckTx {
                    code: 1,
                    log: error.to_string(),
                    ..Default::default()
                }))
            }
            Value::PrepareProposal(r) => reply(self.propose(r), Out::PrepareProposal),
            Value::ProcessProposal(r) => {
                use abci::response_process_proposal::ProposalStatus;
                let status = match self.process(r) {
                    Ok(()) => ProposalStatus::Accept,
                    Err(error) => {
                        eprintln!("level=warn event=native_proposal_rejected error={error:?}");
                        ProposalStatus::Reject
                    }
                };
                Out::ProcessProposal(abci::ResponseProcessProposal {
                    status: status as i32,
                })
            }
            Value::FinalizeBlock(r) => reply(self.finalize(r), Out::FinalizeBlock),
            Value::Commit(_) => reply(self.commit(), Out::Commit),
            Value::Query(r) => {
                Out::Query(self.query(r).unwrap_or_else(|error| abci::ResponseQuery {
                    code: 1,
                    log: format!("{error:#}"),
                    ..Default::default()
                }))
            }
            Value::ExtendVote(_) => Out::ExtendVote(abci::ResponseExtendVote::default()),
            Value::VerifyVoteExtension(r) => {
                use abci::response_verify_vote_extension::VerifyStatus;
                let status = if r.vote_extension.is_empty() {
                    VerifyStatus::Accept
                } else {
                    VerifyStatus::Reject
                };
                Out::VerifyVoteExtension(abci::ResponseVerifyVoteExtension {
                    status: status as i32,
                })
            }
            Value::ListSnapshots(_) => {
                let height = self.store.read().expect("state lock").tip().context.height;
                Out::ListSnapshots(self.snapshots.list(height))
            }
            Value::LoadSnapshotChunk(request) => {
                let height = self.store.read().expect("state lock").tip().context.height;
                Out::LoadSnapshotChunk(self.snapshots.chunk(request, height).unwrap_or_default())
            }
            Value::OfferSnapshot(request) => {
                let height = self.store.read().expect("state lock").tip().context.height;
                Out::OfferSnapshot(
                    self.snapshots
                        .offer(request, height)
                        .unwrap_or_else(|error| {
                            eprintln!("level=warn event=snapshot_offer_rejected error={error:#}");
                            abci::ResponseOfferSnapshot {
                                result: abci::response_offer_snapshot::Result::Reject as i32,
                            }
                        }),
                )
            }
            Value::ApplySnapshotChunk(request) => {
                let mut state = self.store.write().expect("state lock");
                Out::ApplySnapshotChunk(self.snapshots.apply(request, &mut state).unwrap_or_else(
                    |error| {
                        eprintln!("level=warn event=snapshot_restore_rejected error={error:#}");
                        abci::ResponseApplySnapshotChunk {
                            result: abci::response_apply_snapshot_chunk::Result::RejectSnapshot
                                as i32,
                            ..Default::default()
                        }
                    },
                ))
            }
        }
    }
}

impl Candidate {
    fn matches(&self, height: u64, timestamp: u64, raw: &[Bytes]) -> bool {
        self.height == height && self.timestamp == timestamp && self.raw == raw
    }
}

fn reply<T>(result: Result<T>, value: impl FnOnce(T) -> Out) -> Out {
    match result {
        Ok(res) => value(res),
        Err(error) => Out::Exception(abci::ResponseException {
            error: format!("{error:#}"),
        }),
    }
}

fn decode(raw: &[Bytes]) -> Result<Vec<Transaction>> {
    raw.iter().map(|raw| Transaction::from_slice(raw)).collect()
}

fn height(store: &Store, height: i64) -> Result<u64> {
    let height = u64::try_from(height)?;
    ensure!(
        store.tip().context.height.checked_add(1) == Some(height),
        "unexpected consensus height"
    );
    Ok(height)
}

fn timestamp(time: Option<Timestamp>) -> Result<u64> {
    let Timestamp { seconds, nanos } = time.context("missing CometBFT timestamp")?;
    ensure!(
        (0..1_000_000_000).contains(&nanos),
        "invalid timestamp nanoseconds"
    );
    u64::try_from(seconds)?
        .checked_mul(1000)
        .and_then(|ms| ms.checked_add(nanos as u64 / 1_000_000))
        .context("consensus timestamp overflow")
}
