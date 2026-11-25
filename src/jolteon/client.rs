//! Blocking client for a native Stem replica's request endpoint.

use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use prost::Message;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use stem_ledger::{Transaction, block::Header};
use stem_stake::Hash32;

use super::wire;

pub struct Client {
    address: SocketAddr,
    timeout: Duration,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize)]
pub struct AdmissionMetrics {
    pub client_requests: u64,
    pub relayed_requests: u64,
    pub checks: u64,
    pub verification_us: u64,
}

#[derive(Debug, Deserialize, serde::Serialize)]
pub struct Status {
    pub height: u64,
    pub epoch: u64,
    pub view: u64,
    pub high_view: u64,
    pub timeout_views: Vec<u64>,
    pub last_untimely_view: u64,
    pub app_hash: String,
    pub member: Option<u16>,
    pub voting_power: Option<u64>,
    pub committed_transactions: u64,
    pub catching_up: bool,
    pub observer_only: bool,
    pub history_window: Option<u64>,
    pub sent_bytes: u64,
    pub received_bytes: u64,
    pub transport_drops: u64,
    pub invalid_messages: u64,
    pub mempool_transactions: usize,
    pub mempool_bytes: usize,
    pub admission: Option<AdmissionMetrics>,
}

#[derive(Debug, Deserialize)]
pub struct Certificate {
    pub epoch: u64,
    pub handoff: bool,
    #[serde(deserialize_with = "unhex")]
    pub proof: Vec<u8>,
}

#[derive(Debug, Deserialize)]
pub struct QueryResult {
    pub height: u64,
    #[serde(deserialize_with = "unhex")]
    pub value: Vec<u8>,
}

#[derive(Deserialize)]
struct Admission {
    admitted: String,
}

#[derive(Deserialize)]
struct HeaderResult {
    height: u64,
    #[serde(deserialize_with = "unhex")]
    header: Vec<u8>,
}

#[derive(Deserialize)]
struct Handoff {
    #[serde(deserialize_with = "unhex")]
    proof: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Response<T> {
    Failure { error: String },
    Success(T),
}

impl Client {
    pub fn new(address: SocketAddr, timeout: Duration) -> Self {
        Self { address, timeout }
    }

    pub fn status(&self) -> Result<Status> {
        self.request(wire::STATUS, Vec::new())
    }

    /// A successful reply confirms admission. Fetch a finality proof for confirmation.
    pub fn submit(&self, transaction: &Transaction) -> Result<Hash32> {
        let Admission { admitted } = self.request(wire::SUBMIT, transaction.as_slice().to_vec())?;
        let id = ckb_hash::blake2b_256(transaction.as_slice());
        ensure!(
            hex::decode(admitted)? == id,
            "replica returned a different transaction digest"
        );
        Ok(id)
    }

    pub fn header(&self, height: u64) -> Result<Header> {
        let reply: HeaderResult = self.request(wire::HEADER, height.to_le_bytes().to_vec())?;
        let header = Header::decode(&reply.header)?;
        ensure!(
            reply.height == height && header.context.height == height,
            "replica returned a different Header height"
        );
        Ok(header)
    }

    pub fn finality(&self, height: u64) -> Result<Certificate> {
        self.request(wire::FINALITY, height.to_le_bytes().to_vec())
    }

    pub fn handoff(&self, epoch: u64) -> Result<Vec<u8>> {
        let Handoff { proof } = self.request(wire::HANDOFF_PROOF, epoch.to_le_bytes().to_vec())?;
        Ok(proof)
    }

    pub fn query(&self, path: &str, height: u64, data: &[u8], prove: bool) -> Result<QueryResult> {
        self.request(wire::QUERY, serde_json::to_vec(&json!({
            "path": path, "height": height, "data": format!("0x{}", hex::encode(data)), "prove": prove,
        }))?)
    }

    fn request<T: DeserializeOwned>(&self, kind: u32, body: Vec<u8>) -> Result<T> {
        let frame = wire::Frame {
            sender: wire::CLIENT,
            kind,
            body,
            epoch: 0,
        }
        .encode_to_vec();
        ensure!(frame.len() <= wire::MAX_FRAME, "oversized request");
        let mut socket = TcpStream::connect_timeout(&self.address, self.timeout)?;
        socket.set_write_timeout(Some(self.timeout))?;
        socket.set_read_timeout(Some(self.timeout))?;
        socket.write_all(&(frame.len() as u32).to_be_bytes())?;
        socket.write_all(&frame)?;
        let mut length = [0; 4];
        socket.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        ensure!(
            (1..=wire::MAX_FRAME).contains(&length),
            "invalid response length"
        );
        let mut bytes = vec![0; length];
        socket.read_exact(&mut bytes)?;
        match serde_json::from_slice(&bytes).context("invalid replica response")? {
            Response::Failure { error } => bail!("{error}"),
            Response::Success(response) => Ok(response),
        }
    }
}

fn unhex<'de, D: serde::Deserializer<'de>>(decoder: D) -> Result<Vec<u8>, D::Error> {
    let text = String::deserialize(decoder)?;
    hex::decode(text).map_err(serde::de::Error::custom)
}
