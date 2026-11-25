use std::{
    fs::{self, DirBuilder},
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::Path,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use ckb_db::internal::{DB, Options, WriteBatch, WriteOptions, prelude::*};
use ckb_jsonrpc_types::{Either, JsonBytes, Status, TransactionWithStatusResponse};
use ckb_types::{
    H256,
    packed::{Byte32, Transaction},
    prelude::*,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::bridge::{Bridge, Deposit};

pub struct Rpc {
    client: reqwest::blocking::Client,
    url: reqwest::Url,
}

pub struct Journal {
    db: DB,
    key: Byte32,
}

impl Journal {
    pub fn open(path: &Path, key: Byte32) -> Result<Self> {
        match DirBuilder::new().mode(0o700).create(path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err.into()),
        }
        let metadata = fs::symlink_metadata(path)?;
        ensure!(
            metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
            "guardian journal directory must be private"
        );
        let mut options = Options::default();
        options.create_if_missing(true);
        options.set_write_buffer_size(1024 * 1024);
        let db = DB::open(&options, path)?;
        Ok(Self { db, key })
    }

    pub fn validate(&self, node: &Rpc) -> Result<Option<u64>> {
        let Some(record) = self.db.get(self.key.as_slice())? else {
            return Ok(None);
        };
        ensure!(record.len() == 40, "invalid guardian journal record");
        let height = u64::from_le_bytes(record[..8].try_into()?);
        let current = node
            .block_hash(height)
            .context("guardian halted: previous L1 anchor unavailable")?;
        ensure!(
            current.as_bytes() == &record[8..],
            "guardian halted: previous L1 anchor was reorganized"
        );
        Ok(Some(height))
    }

    pub fn advance(&self, node: &Rpc, deposit: &Deposit) -> Result<()> {
        self.pin(node, deposit.confirmed_height, &deposit.confirmed_block)
    }

    pub fn pin(&self, node: &Rpc, height: u64, hash: &H256) -> Result<()> {
        let previous = self.validate(node)?;
        ensure!(
            node.block_hash(height)? == *hash,
            "guardian halted: current L1 anchor changed during approval"
        );
        if previous.is_some_and(|previous| previous >= height) {
            return Ok(());
        }
        let mut record = Vec::with_capacity(40);
        record.extend(height.to_le_bytes());
        record.extend(hash.as_bytes());
        let mut batch = WriteBatch::default();
        batch.put(self.key.as_slice(), record)?;
        let mut options = WriteOptions::default();
        options.set_sync(true);
        self.db.write_opt(&batch, &options)?;
        Ok(())
    }
}

impl Rpc {
    pub fn new(url: &str) -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self {
            client,
            url: url.parse()?,
        })
    }

    pub fn call<T: DeserializeOwned>(&self, method: &str, params: Value) -> Result<T> {
        let response = self
            .client
            .post(self.url.clone())
            .json(&json!({"id":1, "jsonrpc":"2.0", "method":method, "params":params}))
            .send()?
            .error_for_status()?;
        const MAX: u64 = 16 * 1024 * 1024;
        let mut buf = Vec::new();
        use std::io::Read;
        response.take(MAX + 1).read_to_end(&mut buf)?;
        ensure!(
            buf.len() as u64 <= MAX,
            "L1 RPC response exceeds size limit"
        );
        let reply: Value = serde_json::from_slice(&buf)?;
        ensure!(
            reply["error"].is_null(),
            "L1 {method} failed: {}",
            reply["error"]
        );
        let result = reply
            .get("result")
            .context("L1 RPC response has no result")?;
        Ok(serde_json::from_value(result.clone())?)
    }

    pub fn observe_deposit(
        &self,
        bridge: &Bridge,
        deposit: &H256,
        output_index: u32,
        consolidation: &H256,
    ) -> Result<Deposit> {
        ensure!(
            self.block_hash(0)? == bridge.l1_genesis,
            "L1 RPC serves a different genesis"
        );
        let (deposit_tx, deposit_height, deposit_block) = self.committed(deposit)?;
        let (consolidation_tx, consolidation_height, consolidation_block) =
            self.committed(consolidation)?;
        let confirmed_height = consolidation_height
            .checked_add(bridge.confirmations)
            .context("confirmation height overflow")?;
        let confirmed_block = self
            .block_hash(confirmed_height)
            .context("consolidation has insufficient confirmations")?;
        ensure!(
            self.block_hash(deposit_height)? == deposit_block
                && self.block_hash(consolidation_height)? == consolidation_block
                && self.block_hash(confirmed_height)? == confirmed_block,
            "L1 canonical chain changed during confirmation"
        );
        Ok(Deposit {
            deposit_tx,
            output_index,
            deposit_block,
            deposit_height,
            consolidation_tx,
            consolidation_block,
            consolidation_height,
            confirmed_block,
            confirmed_height,
        })
    }

    pub fn block_hash(&self, height: u64) -> Result<H256> {
        let hash: Option<H256> = self.call("get_block_hash", json!([format!("0x{height:x}")]))?;
        hash.with_context(|| format!("L1 block {height} unavailable"))
    }

    pub fn committed(&self, hash: &H256) -> Result<(JsonBytes, u64, H256)> {
        let response: TransactionWithStatusResponse =
            self.call("get_transaction", json!([hash, "0x0", true]))?;
        ensure!(
            response.tx_status.status == Status::Committed,
            "L1 transaction is unconfirmed"
        );
        let block = response
            .tx_status
            .block_hash
            .context("L1 status lacks block hash")?;
        let height = response
            .tx_status
            .block_number
            .context("L1 status lacks block number")?
            .value();
        let Some(Either::Right(raw)) = response.transaction.map(|r| r.inner) else {
            anyhow::bail!("L1 did not return packed transaction");
        };
        let transaction = Transaction::from_slice(raw.as_bytes())?.into_view();
        ensure!(
            transaction.hash().as_slice() == hash.as_bytes(),
            "L1 transaction hash mismatch"
        );
        Ok((raw, height, block))
    }
}
