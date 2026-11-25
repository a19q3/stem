use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, ensure};
use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use ckb_jsonrpc_types::{
    BlockResponse, HeaderView as JsonHeader, IndexerOrder, IndexerSearchKey, JsonBytes,
    OutPoint as JsonOutPoint, OutputsValidator, ResponseFormat, Transaction as JsonTx,
    TransactionView as JsonTxView, TransactionWithStatusResponse, TxStatus, Uint32, Uint64,
};
use ckb_types::{
    H256,
    core::{BlockBuilder, HeaderView, TransactionView},
    packed::{Byte32, Header, OutPoint, Transaction},
    prelude::*,
};
use clap::Args;
use jsonrpc_core::{Error, ErrorCode, IoHandler, Params, Value};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::json;
use tokio::sync::Semaphore;

use crate::{
    comet_rpc::Rpc,
    genesis::{MAX_TX_BYTES, transactions_root},
    index::MAX_PAGE,
    read_genesis,
};

const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_BATCH: usize = 16;
const CONCURRENCY: usize = 16;
const CELL_PROOF_METHOD: &str = "get_live_cell_proof";
type RpcResult<T = Value> = std::result::Result<T, Error>;

#[derive(Args)]
pub struct Serve {
    #[arg(long, default_value = "127.0.0.1:8114")]
    listen: SocketAddr,
    #[arg(long, default_value = "http://127.0.0.1:26657/")]
    comet_rpc: String,
    /// The application genesis used to pin the backend chain identity.
    #[arg(long)]
    genesis: PathBuf,
}

impl Serve {
    pub fn run(self) -> Result<()> {
        ensure!(
            self.listen.ip().is_loopback(),
            "CKB RPC listens on loopback; publish it through an RPC proxy"
        );
        let spec = read_genesis(&self.genesis)?;
        let (genesis, allocations) = spec.validate()?;
        let genesis = genesis.hash();
        let rpc = Rpc::new(&self.comet_rpc)?;
        rpc.check_genesis(&genesis)?;
        let backend = Arc::new(Backend {
            rpc,
            genesis,
            allocations,
            compact_checkpoints: spec.checkpoint_compact_transactions,
        });
        let mut dispatcher = IoHandler::new();
        for name in [
            "get_tip_block_number",
            "get_tip_header",
            "get_header",
            "get_header_by_number",
            "get_block_hash",
            "get_block",
            "get_block_by_number",
            "get_live_cell",
            CELL_PROOF_METHOD,
            "get_transaction",
            "send_transaction",
            "get_indexer_tip",
            "get_cells",
            "get_cells_capacity",
        ] {
            let backend = Arc::clone(&backend);
            dispatcher.add_sync_method(name, move |params| backend.call(name, params));
        }
        let state = Arc::new(Http {
            handler: dispatcher,
            permits: Arc::new(Semaphore::new(CONCURRENCY)),
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind(self.listen).await?;
            eprintln!(
                "level=info event=ckb_rpc_listening address={}",
                listener.local_addr()?
            );
            let router = Router::new()
                .route("/", post(handle_http))
                .with_state(state);
            axum::serve(listener, router).await?;
            Ok(())
        })
    }
}

struct Http {
    handler: IoHandler,
    permits: Arc<Semaphore>,
}

async fn handle_http(State(state): State<Arc<Http>>, req: Request) -> Response {
    let Ok(permit) = Arc::clone(&state.permits).try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let body =
        match tokio::time::timeout(Duration::from_secs(5), to_bytes(req.into_body(), MAX_BODY))
            .await
        {
            Ok(Ok(body)) => body,
            Ok(Err(_)) => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
            Err(_) => return StatusCode::REQUEST_TIMEOUT.into_response(),
        };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let text = std::str::from_utf8(&body).unwrap_or("");
        if matches!(serde_json::from_str::<Value>(text), Ok(Value::Array(calls))
            if calls.len() > MAX_BATCH || calls.iter().any(|call| call["method"] == CELL_PROOF_METHOD))
        {
            return Some(
                json!({"jsonrpc":"2.0", "error": Error::invalid_request(), "id":null}).to_string(),
            );
        }
        state.handler.handle_request_sync(text)
    });
    match tokio::time::timeout(Duration::from_secs(20), task).await {
        Ok(Ok(Some(reply))) => {
            ([(header::CONTENT_TYPE, "application/json")], reply).into_response()
        }
        Ok(Ok(None)) => StatusCode::NO_CONTENT.into_response(),
        Ok(Err(_)) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}

struct Backend {
    rpc: Rpc,
    genesis: Byte32,
    allocations: Vec<TransactionView>,
    compact_checkpoints: bool,
}

impl Backend {
    fn call(&self, method: &str, params: Params) -> RpcResult {
        let args = match params {
            Params::None => Vec::new(),
            Params::Array(args) => args,
            Params::Map(_) => {
                return Err(Error::invalid_params("CKB RPC takes positional parameters"));
            }
        };
        match method {
            "get_tip_block_number" => {
                arity(&args, 0, 0)?;
                to_value(Uint64::from(self.tip().map_err(backend_error)?.number()))
            }
            "get_tip_header" => {
                arity(&args, 0, 1)?;
                to_value(header_format(
                    self.tip().map_err(backend_error)?,
                    optional(&args, 0)?,
                )?)
            }
            "get_header" => {
                arity(&args, 1, 2)?;
                let hash: H256 = required(&args, 0)?;
                let verbosity = optional(&args, 1)?;
                let header = self
                    .header("/header", hash.as_bytes())
                    .map_err(backend_error)?
                    .map(|header| header_format(header, verbosity))
                    .transpose()?;
                to_value(header)
            }
            "get_header_by_number" | "get_block_hash" => {
                arity(&args, 1, if method == "get_block_hash" { 1 } else { 2 })?;
                let number: Uint64 = required(&args, 0)?;
                let verbosity = optional(&args, 1)?;
                let header = self
                    .header("/rpc/header_at", &number.value().to_be_bytes())
                    .map_err(backend_error)?;
                if method == "get_block_hash" {
                    return to_value(header.map(|header| H256::from(header.hash())));
                }
                to_value(
                    header
                        .map(|header| header_format(header, verbosity))
                        .transpose()?,
                )
            }
            "get_block" | "get_block_by_number" => {
                arity(&args, 1, 3)?;
                let verbosity = optional::<Uint32>(&args, 1)?.map_or(2, |n| n.value());
                if !matches!(verbosity, 0 | 2) {
                    return Err(Error::invalid_params("invalid block verbosity"));
                }
                let cycles = optional::<bool>(&args, 2)?.unwrap_or(false);
                let header = if method == "get_block" {
                    let hash: H256 = required(&args, 0)?;
                    self.header("/header", hash.as_bytes())
                } else {
                    let number: Uint64 = required(&args, 0)?;
                    self.header("/rpc/header_at", &number.value().to_be_bytes())
                }
                .map_err(backend_error)?;
                let block = header
                    .map(|header| self.block(header, verbosity, cycles))
                    .transpose()
                    .map_err(backend_error)?;
                to_value(block)
            }
            "get_live_cell" => {
                arity(&args, 2, 3)?;
                let point: OutPoint = required::<JsonOutPoint>(&args, 0)?.into();
                let with_data: bool = required(&args, 1)?;
                if optional::<bool>(&args, 2)?.unwrap_or(false) {
                    return Err(Error::invalid_params(
                        "live Cell RPC covers committed state",
                    ));
                }
                let mut key = vec![u8::from(with_data)];
                key.extend_from_slice(point.as_slice());
                self.query_json("/rpc/live_cell", &key)
            }
            CELL_PROOF_METHOD => {
                arity(&args, 2, 2)?;
                let point: OutPoint = required::<JsonOutPoint>(&args, 0)?.into();
                let height = required::<Uint64>(&args, 1)?.value();
                if !(1..i64::MAX as u64).contains(&height) {
                    return Err(Error::invalid_params("invalid checkpoint height"));
                }
                let mut key = height.to_be_bytes().to_vec();
                key.extend_from_slice(point.as_slice());
                let checkpoint = self.query_json("/rpc/live_cell_proof", &key)?;
                if checkpoint.is_null() {
                    return Ok(checkpoint);
                }
                let commit = self.rpc.commit(height).map_err(backend_error)?;
                let next_commit = self.rpc.commit(height + 1).map_err(backend_error)?;
                Ok(json!({"checkpoint": checkpoint, "commit": commit, "next_commit": next_commit}))
            }
            "get_transaction" => {
                arity(&args, 1, 3)?;
                let hash = required(&args, 0)?;
                let verbosity = optional::<Uint32>(&args, 1)?.map_or(2, |n| n.value());
                let _ = optional::<bool>(&args, 2)?;
                if verbosity > 2 {
                    return Err(Error::invalid_params("invalid transaction verbosity"));
                }
                to_value(self.transaction(&hash, verbosity).map_err(backend_error)?)
            }
            "send_transaction" => {
                arity(&args, 1, 2)?;
                let tx: Transaction = required::<JsonTx>(&args, 0)?.into();
                if optional::<OutputsValidator>(&args, 1)?
                    == Some(OutputsValidator::WellKnownScriptsOnly)
                {
                    return Err(Error::invalid_params(
                        "use passthrough for sidechain output scripts",
                    ));
                }
                if tx.as_slice().len() > MAX_TX_BYTES {
                    return Err(Error::invalid_params(
                        "packed transaction exceeds sidechain byte limit",
                    ));
                }
                let tx = tx.into_view();
                self.rpc
                    .check_genesis(&self.genesis)
                    .map_err(backend_error)?;
                let admitted = self.rpc.broadcast_sync(&tx).map_err(backend_error)?;
                if admitted.code != 0 {
                    return Err(server_error(
                        -302,
                        "TransactionFailedToVerify",
                        admitted.log,
                    ));
                }
                to_value(H256::from(tx.hash()))
            }
            "get_indexer_tip" => {
                arity(&args, 0, 0)?;
                self.query_json("/rpc/indexer_tip", &[])
            }
            "get_cells" => {
                arity(&args, 3, 4)?;
                let _: IndexerSearchKey = required(&args, 0)?;
                let _: IndexerOrder = required(&args, 1)?;
                let limit: Uint32 = required(&args, 2)?;
                let _: Option<JsonBytes> = optional(&args, 3)?;
                if !(1..=MAX_PAGE).contains(&(limit.value() as usize)) {
                    return Err(Error::invalid_params(format!(
                        "limit must be between 1 and {MAX_PAGE}"
                    )));
                }
                let mut args = args;
                args.resize(4, Value::Null);
                self.query_json(
                    "/rpc/cells",
                    &serde_json::to_vec(&args).map_err(backend_error)?,
                )
            }
            "get_cells_capacity" => {
                arity(&args, 1, 1)?;
                let _: IndexerSearchKey = required(&args, 0)?;
                self.query_json(
                    "/rpc/cells_capacity",
                    &serde_json::to_vec(&args[0]).map_err(backend_error)?,
                )
            }
            _ => Err(Error::method_not_found()),
        }
    }

    fn query_json(&self, path: &str, key: &[u8]) -> RpcResult {
        let data = self
            .rpc
            .query_binary(path, key, None)
            .map_err(backend_error)?
            .ok_or_else(Error::internal_error)?;
        serde_json::from_slice(&data).map_err(backend_error)
    }

    fn header(&self, path: &str, key: &[u8]) -> Result<Option<HeaderView>> {
        self.rpc
            .query_binary(path, key, Some("unknown header"))?
            .map(|data| Ok(Header::from_slice(&data)?.into_view()))
            .transpose()
    }

    fn tip(&self) -> Result<HeaderView> {
        self.header("/rpc/tip_header", &[])?
            .context("missing tip header")
    }

    fn block(
        &self,
        header: HeaderView,
        verbosity: u32,
        include_cycles: bool,
    ) -> Result<BlockResponse> {
        let height = header.number();
        let txs = if height == 0 {
            self.allocations.clone()
        } else {
            self.rpc.block(height)?
        };
        ensure!(
            transactions_root(&txs) == header.transactions_root(),
            "block transactions do not match the committed CKB header"
        );
        let count = txs.len();
        let block = BlockBuilder::default()
            .header(header)
            .set_transactions(txs)
            .build_unchecked();
        let response = if verbosity == 0 {
            ResponseFormat::hex(block.data().as_bytes())
        } else {
            ResponseFormat::json(block.into())
        };
        Ok(if include_cycles {
            let cycles = (height != 0)
                .then(|| self.rpc.block_cycles(height, count))
                .transpose()?
                .map(|cycles| cycles.into_iter().map(Into::into).collect());
            BlockResponse::with_cycles(response, cycles)
        } else {
            BlockResponse::regular(response)
        })
    }

    fn transaction(&self, hash: &H256, verbosity: u32) -> Result<TransactionWithStatusResponse> {
        let mut response = TransactionWithStatusResponse {
            transaction: None,
            cycles: None,
            time_added_to_pool: None,
            tx_status: TxStatus::unknown(),
            fee: None,
            min_replace_fee: None,
        };
        let path = if self.compact_checkpoints {
            "/rpc/transaction"
        } else {
            "/transaction"
        };
        let Some(raw) =
            self.rpc
                .query_binary(path, hash.as_bytes(), Some("unknown transaction"))?
        else {
            return Ok(response);
        };
        let (block_hash, data) = raw
            .split_at_checked(32)
            .context("invalid archived transaction")?;
        let header = self
            .header("/header", block_hash)?
            .context("missing transaction origin header")?;
        ensure!(
            header.hash().as_slice() == block_hash,
            "archived header hash differs"
        );
        response.tx_status.status = ckb_jsonrpc_types::Status::Committed;
        response.tx_status.block_hash = Some(header.hash().into());
        response.tx_status.block_number = Some(header.number().into());
        if !data.is_empty() || !self.compact_checkpoints {
            let tx = Transaction::from_slice(data)?.into_view();
            ensure!(
                tx.hash().as_slice() == hash.as_bytes(),
                "archived transaction hash differs"
            );
            response.transaction = match verbosity {
                0 => Some(ResponseFormat::hex(tx.data().as_bytes())),
                2 => Some(ResponseFormat::json(JsonTxView::from(tx))),
                _ => None,
            };
        }
        Ok(response)
    }
}

fn header_format(
    header: HeaderView,
    verbosity: Option<Uint32>,
) -> RpcResult<ResponseFormat<JsonHeader>> {
    match verbosity.map_or(1, |n| n.value()) {
        0 => Ok(ResponseFormat::hex(header.data().as_bytes())),
        1 => Ok(ResponseFormat::json(header.into())),
        _ => Err(Error::invalid_params("invalid header verbosity")),
    }
}

fn arity(args: &[Value], min: usize, max: usize) -> RpcResult<()> {
    if (min..=max).contains(&args.len()) {
        Ok(())
    } else {
        Err(Error::invalid_params("unexpected number of parameters"))
    }
}

fn required<T: DeserializeOwned>(args: &[Value], at: usize) -> RpcResult<T> {
    serde_json::from_value(args[at].clone())
        .map_err(|error| Error::invalid_params(error.to_string()))
}

fn optional<T: DeserializeOwned>(args: &[Value], at: usize) -> RpcResult<Option<T>> {
    if at >= args.len() {
        Ok(None)
    } else {
        required(args, at)
    }
}

fn to_value(value: impl Serialize) -> RpcResult {
    serde_json::to_value(value).map_err(backend_error)
}

fn backend_error(error: impl std::fmt::Display) -> Error {
    server_error(-32000, "Sidechain RPC query failed", error.to_string())
}

fn server_error(code: i64, message: &str, details: String) -> Error {
    Error {
        code: ErrorCode::ServerError(code),
        message: message.to_owned(),
        data: Some(Value::String(details)),
    }
}
