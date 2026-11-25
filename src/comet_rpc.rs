use std::{
    io::Read,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ckb_types::{
    core::TransactionView,
    packed::{Byte32, Transaction},
    prelude::*,
};
use reqwest::blocking::{Client, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::genesis::{MAX_BLOCK_BYTES, MAX_BLOCK_TXS, MAX_GENESIS_BYTES, MAX_TX_BYTES};

pub struct Rpc {
    client: Client,
    url: reqwest::Url,
}

#[derive(Deserialize)]
struct AbciQuery {
    response: AbciResponse,
}

#[derive(Deserialize)]
struct AbciResponse {
    code: u32,
    #[serde(default)]
    log: String,
    value: Option<String>,
}

#[derive(Deserialize)]
pub struct Broadcast {
    pub code: u32,
    pub log: String,
    pub hash: String,
}

#[derive(Deserialize)]
struct BlockReply {
    block: Block,
}

#[derive(Deserialize)]
struct Block {
    header: BlockHeader,
    data: BlockData,
}

#[derive(Deserialize)]
struct BlockHeader {
    height: String,
}

#[derive(Deserialize)]
struct BlockData {
    txs: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct BlockResults {
    height: String,
    txs_results: Option<Vec<TransactionResult>>,
}

#[derive(Deserialize)]
struct TransactionResult {
    code: u32,
    gas_used: String,
}

impl Rpc {
    pub fn new(url: &str) -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()?,
            url: url.parse()?,
        })
    }

    pub fn check_genesis(&self, genesis: &Byte32) -> Result<()> {
        let tip = self
            .query("/tip", "0x", None)?
            .context("missing sidechain tip")?;
        let tip: Value = serde_json::from_slice(&tip)?;
        ensure!(
            tip["genesis_hash"] == format!("{genesis:#x}"),
            "sidechain RPC serves a different genesis"
        );
        Ok(())
    }

    pub fn broadcast_sync(&self, transaction: &TransactionView) -> Result<Broadcast> {
        let data = transaction.data();
        let reply: Broadcast = serde_json::from_value(self.post(
            "broadcast_tx_sync",
            json!({"tx": STANDARD.encode(data.as_slice())}),
            65_536,
        )?)?;
        ensure!(
            reply.code != 0
                || reply
                    .hash
                    .eq_ignore_ascii_case(&hex::encode(Sha256::digest(data.as_slice()))),
            "CometBFT returned a different transaction ID"
        );
        Ok(reply)
    }

    pub fn block(&self, height: u64) -> Result<Vec<TransactionView>> {
        let block: BlockReply = serde_json::from_value(self.post(
            "block",
            json!({"height": height.to_string()}),
            MAX_BLOCK_BYTES * 2,
        )?)?;
        ensure!(
            block.block.header.height.parse::<u64>()? == height,
            "incorrect CometBFT block height"
        );
        let transactions = block.block.data.txs.unwrap_or_default();
        ensure!(
            transactions.len() <= MAX_BLOCK_TXS,
            "CometBFT block exceeds the transaction limit"
        );
        let mut total = 0;
        transactions
            .into_iter()
            .map(|bytes| {
                let packed = STANDARD.decode(bytes)?;
                total += packed.len();
                ensure!(
                    total <= MAX_BLOCK_BYTES && packed.len() <= MAX_TX_BYTES,
                    "CometBFT block exceeds the transaction byte limit"
                );
                Ok(Transaction::from_slice(&packed)?.into_view())
            })
            .collect()
    }

    pub fn commit(&self, height: u64) -> Result<Value> {
        let result = self.post("commit", json!({"height": height.to_string()}), 256 * 1024)?;
        ensure!(
            result["signed_header"]["header"]["height"]
                .as_str()
                .context("missing CometBFT commit height")?
                .parse::<u64>()?
                == height,
            "incorrect CometBFT commit height"
        );
        Ok(result)
    }

    pub fn block_cycles(&self, height: u64, expected: usize) -> Result<Vec<u64>> {
        let results: BlockResults = serde_json::from_value(self.post(
            "block_results",
            json!({"height": height.to_string()}),
            MAX_BLOCK_BYTES * 2,
        )?)?;
        ensure!(
            results.height.parse::<u64>()? == height,
            "incorrect CometBFT result height"
        );
        let results = results.txs_results.unwrap_or_default();
        ensure!(
            results.len() == expected,
            "CometBFT block results omit transactions"
        );
        results
            .into_iter()
            .map(|result| {
                ensure!(
                    result.code == 0,
                    "finalized block contains an unsuccessful transaction"
                );
                Ok(result.gas_used.parse()?)
            })
            .collect()
    }

    pub fn submit_mint(&self, genesis: &Byte32, transaction: &TransactionView) -> Result<Value> {
        self.check_genesis(genesis)?;
        if let Some(block) = self.mint_block(transaction)? {
            return Ok(json!({"block_hash": format!("{block:#x}"), "already_finalized": true}));
        }
        let body = self.broadcast_sync(transaction);
        // A lost RPC response does not tell us whether the transaction entered consensus.
        let admission = match body {
            Ok(reply) if reply.code == 0 => None,
            Ok(reply) => {
                if reply.log != "transaction already exists" {
                    anyhow::bail!("mint admission failed: {}", reply.log);
                }
                Some(reply.log)
            }
            Err(error) => Some(error.to_string()),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(block) = self.mint_block(transaction)? {
                return Ok(
                    json!({"block_hash": format!("{block:#x}"), "already_finalized": false}),
                );
            }
            thread::sleep(Duration::from_millis(150));
        }
        anyhow::bail!(
            "mint {} was not observed in a finalized block: {}",
            transaction.hash(),
            admission.as_deref().unwrap_or("admitted")
        )
    }

    fn mint_block(&self, transaction: &TransactionView) -> Result<Option<Byte32>> {
        Ok(self.committed_mint(transaction)?.map(|(block, _)| block))
    }

    pub fn committed_mint(
        &self,
        transaction: &TransactionView,
    ) -> Result<Option<(Byte32, TransactionView)>> {
        let hash = transaction.hash();
        let Some(found) = self.query(
            "/transaction",
            &format!("{hash:#x}"),
            Some("unknown transaction"),
        )?
        else {
            return Ok(None);
        };
        let (header, raw) = found
            .split_at_checked(32)
            .context("truncated archived transaction")?;
        let archived = Transaction::from_slice(raw)?.into_view();
        ensure!(
            archived.data().raw().as_slice() == transaction.data().raw().as_slice(),
            "RPC returned a different CKB transaction"
        );
        Ok(Some((Byte32::from_slice(header)?, archived)))
    }

    pub fn guardians(&self, genesis: &Byte32) -> Result<crate::bridge::Guardians> {
        self.check_genesis(genesis)?;
        let data = self
            .query("/bridge_guardians", "0x", None)?
            .context("missing guardian state")?;
        Ok(serde_json::from_slice(&data)?)
    }

    pub fn query(&self, path: &str, key: &str, missing: Option<&str>) -> Result<Option<Vec<u8>>> {
        let key = key.strip_prefix("0x").context("invalid RPC key encoding")?;
        self.query_hex(path, key, missing)
    }

    pub fn query_binary(
        &self,
        path: &str,
        key: &[u8],
        missing: Option<&str>,
    ) -> Result<Option<Vec<u8>>> {
        self.query_hex(path, &hex::encode(key), missing)
    }

    fn query_hex(&self, path: &str, key: &str, missing: Option<&str>) -> Result<Option<Vec<u8>>> {
        let limit = match path {
            // A genesis allocation may contain a system program above the ordinary tx limit.
            "/transaction"
            | "/rpc/transaction"
            | "/rpc/live_cell"
            | "/rpc/cells"
            | "/rpc/live_cell_proof" => MAX_GENESIS_BYTES * 2,
            _ => MAX_TX_BYTES * 2,
        };
        let query: AbciQuery = serde_json::from_value(self.post(
            "abci_query",
            json!({"path": path, "data": key}),
            limit,
        )?)?;
        if query.response.code != 0 && missing == Some(query.response.log.as_str()) {
            return Ok(None);
        }
        ensure!(
            query.response.code == 0,
            "sidechain query failed: {}",
            query.response.log
        );
        let encoded = query
            .response
            .value
            .context("sidechain query has no value")?;
        Ok(Some(STANDARD.decode(encoded)?))
    }

    fn post(&self, method: &str, params: Value, limit: usize) -> Result<Value> {
        let response = self
            .client
            .post(self.url.clone())
            .json(&json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}))
            .send()?;
        result(response, limit)
    }
}

fn result(response: Response, limit: usize) -> Result<Value> {
    let status = response.status();
    let mut bytes = Vec::new();
    response.take(limit as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= limit, "oversized sidechain RPC response");
    let mut parsed: Value = serde_json::from_slice(&bytes)?;
    ensure!(
        parsed["error"].is_null(),
        "sidechain RPC failed: {}",
        parsed["error"]
    );
    ensure!(status.is_success(), "sidechain RPC returned HTTP {status}");
    let result = parsed["result"].take();
    ensure!(!result.is_null(), "sidechain RPC returned no result");
    Ok(result)
}
