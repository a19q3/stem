use anyhow::{Result, ensure};
use prost::Message;

pub const PROPOSAL: u32 = 1;
pub const VOTE: u32 = 2;
pub const QUORUM: u32 = 3;
pub const TIMEOUT: u32 = 4;
pub const TIMEOUT_CERTIFICATE: u32 = 5;
pub const GET: u32 = 6;
pub const RELAY_TX: u32 = 7;
pub const HANDOFF: u32 = 8;
pub const GET_HANDOFF: u32 = 9;
pub const SUBMIT: u32 = 20;
pub const STATUS: u32 = 21;
pub const FINALITY: u32 = 22;
pub const HEADER: u32 = 23;
pub const HANDOFF_PROOF: u32 = 24;
pub const QUERY: u32 = 25;
pub const CLIENT: u32 = u32::MAX;
pub const MAX_FRAME: usize = 24 * 1024 * 1024;

#[derive(Clone, Message)]
pub struct Frame {
    #[prost(uint32, tag = "1")]
    pub sender: u32,
    #[prost(uint32, tag = "2")]
    pub kind: u32,
    #[prost(bytes, tag = "3")]
    pub body: Vec<u8>,
    #[prost(uint64, tag = "4")]
    pub epoch: u64,
}

#[derive(Clone, Message)]
pub struct Proposal {
    #[prost(uint64, tag = "1")]
    pub view: u64,
    #[prost(bytes, tag = "2")]
    pub header: Vec<u8>,
    #[prost(bytes, tag = "3")]
    pub parent: Vec<u8>,
    #[prost(bytes, tag = "4")]
    pub timeout: Vec<u8>,
    #[prost(bytes, repeated, tag = "5")]
    pub transactions: Vec<Vec<u8>>,
    #[prost(bytes, tag = "6")]
    pub signature: Vec<u8>,
    #[prost(bool, tag = "7")]
    pub seal: bool,
}

#[derive(Clone, Message)]
pub struct Timeout {
    #[prost(uint64, tag = "1")]
    pub view: u64,
    #[prost(bytes, tag = "2")]
    pub high: Vec<u8>,
    #[prost(bytes, tag = "3")]
    pub vote: Vec<u8>,
}

#[derive(Clone, Message)]
pub struct Journal {
    #[prost(bytes, tag = "1")]
    pub identity: Vec<u8>,
    #[prost(bytes, tag = "2")]
    pub high: Vec<u8>,
    #[prost(bytes, tag = "3")]
    pub vote: Vec<u8>,
    #[prost(bytes, tag = "4")]
    pub timeout: Vec<u8>,
    #[prost(bytes, tag = "5")]
    pub certificate: Vec<u8>,
    #[prost(bytes, tag = "6")]
    pub finalized: Vec<u8>,
    #[prost(uint64, tag = "7")]
    pub lock: u64,
    #[prost(uint64, tag = "8")]
    pub committed_transactions: u64,
    #[prost(bytes, tag = "9")]
    pub committee: Vec<u8>,
    #[prost(bytes, tag = "10")]
    pub anchor: Vec<u8>,
}

#[derive(Clone, Message)]
pub struct Finalized {
    #[prost(bytes, tag = "1")]
    pub proof: Vec<u8>,
    #[prost(uint64, tag = "2")]
    pub epoch: u64,
    #[prost(bool, tag = "3")]
    pub handoff: bool,
}

pub fn canonical<T: Message + Default>(raw: &[u8]) -> Result<T> {
    let decoded = T::decode(raw)?;
    ensure!(
        decoded.encode_to_vec() == raw,
        "noncanonical consensus frame"
    );
    Ok(decoded)
}
