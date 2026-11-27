use std::time::Duration;

use anyhow::{Result, ensure};
use bytes::Bytes;
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
pub const GET_REPLAY: u32 = 10;
pub const REPLAY: u32 = 11;
pub const PEER_ENDPOINT: u32 = 12;
pub const ENDPOINT_CHALLENGE: u32 = 13;
pub const ENDPOINT_RESPONSE: u32 = 14;
pub const SUBMIT: u32 = 20;
pub const STATUS: u32 = 21;
pub const FINALITY: u32 = 22;
pub const HEADER: u32 = 23;
pub const HANDOFF_PROOF: u32 = 24;
pub const QUERY: u32 = 25;
pub const CLIENT: u32 = u32::MAX;
pub const SUBMIT_SESSION_REQUESTS: usize = 256;
pub const SUBMIT_IDLE: Duration = Duration::from_secs(1);
pub const MAX_FRAME: usize = 24 * 1024 * 1024;

#[derive(Clone, Message)]
pub struct Frame {
    #[prost(uint32, tag = "1")]
    pub sender: u32,
    #[prost(uint32, tag = "2")]
    pub kind: u32,
    #[prost(bytes = "bytes", tag = "3")]
    pub body: Bytes,
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
    #[prost(bytes = "bytes", repeated, tag = "5")]
    pub transactions: Vec<Bytes>,
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
    #[prost(bool, tag = "11")]
    pub observer_only: bool,
    #[prost(uint64, tag = "12")]
    pub history_window: u64,
    #[prost(bytes, tag = "13")]
    pub proposal_floor: Vec<u8>,
}

#[derive(Clone, Message)]
pub struct Finalized {
    #[prost(bytes, tag = "1")]
    pub proof: Vec<u8>,
    #[prost(uint64, tag = "2")]
    pub epoch: u64,
    #[prost(bool, tag = "3")]
    pub handoff: bool,
    #[prost(uint64, tag = "4")]
    pub view: u64,
}

pub fn canonical<T: Message + Default>(raw: &[u8]) -> Result<T> {
    check_encoding(raw, T::decode(raw)?)
}

/// Decoded byte fields can share storage with an immutable input buffer.
pub fn canonical_bytes<T: Message + Default>(raw: Bytes) -> Result<T> {
    let decoded = T::decode(raw.clone())?;
    check_encoding(&raw, decoded)
}

fn check_encoding<T: Message>(raw: &[u8], decoded: T) -> Result<T> {
    ensure!(
        decoded.encode_to_vec() == raw,
        "noncanonical consensus frame"
    );
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckb_types::packed::OutPoint;
    use stem_ledger::{CellInput, Transaction};

    #[test]
    fn canonical_frame_and_proposal_share_the_validated_transaction_bytes() -> Result<()> {
        let tx = Transaction::new(
            vec![],
            vec![],
            vec![CellInput::new(OutPoint::default(), None)],
            vec![],
            vec![],
        )?;
        let proposal = Proposal {
            view: 1,
            transactions: vec![tx.as_bytes()],
            ..Default::default()
        };
        let frame = Frame {
            kind: PROPOSAL,
            body: proposal.encode_to_vec().into(),
            ..Default::default()
        };
        let raw = Bytes::from(frame.encode_to_vec());
        let received: Frame = canonical_bytes(raw.clone())?;
        let proposal_offset = raw.len() - received.body.len();
        assert_eq!(received.body.as_ptr(), raw[proposal_offset..].as_ptr());
        let body = received.body;
        let proposal: Proposal = canonical_bytes(body.clone())?;
        let transaction = &proposal.transactions[0];
        let tx_offset = body.len() - transaction.len();
        assert_eq!(transaction.as_ptr(), body[tx_offset..].as_ptr());
        let decoded = Transaction::from_bytes(transaction.clone())?;
        assert_eq!(decoded.as_slice().as_ptr(), transaction.as_ptr());
        assert_eq!(decoded.as_slice(), tx.as_slice());

        let mut trailing = raw.to_vec();
        trailing.extend_from_slice(&[0x28, 0]); // Unknown protobuf field.
        assert!(canonical::<Frame>(&trailing).is_err());
        assert!(canonical_bytes::<Frame>(trailing.into()).is_err());
        assert!(Transaction::from_bytes(transaction.slice(..tx.as_slice().len() - 1)).is_err());
        Ok(())
    }
}
