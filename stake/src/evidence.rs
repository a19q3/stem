use alloc::vec::Vec;

use ed25519_dalek::Signature;
use prost::Message;
use tendermint_proto::{
    google::protobuf::Timestamp,
    v0_38::types::{CanonicalBlockId, CanonicalPartSetHeader, CanonicalVote},
};

use crate::{Error, Hash32, Result, ValidatorKey, digest, public_key};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub enum VoteKind {
    Prevote = 1,
    Precommit = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BlockId {
    pub hash: Hash32,
    pub parts: u32,
    pub part_set_hash: Hash32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignedVote {
    pub kind: VoteKind,
    pub height: i64,
    pub round: i32,
    pub block_id: Option<BlockId>,
    pub seconds: i64,
    pub nanos: i32,
    pub signature: [u8; 64],
}

impl SignedVote {
    pub const SIZE: usize = 158;

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::SIZE {
            return Err(Error::Encoding);
        }
        let kind = match bytes[0] {
            1 => VoteKind::Prevote,
            2 => VoteKind::Precommit,
            _ => return Err(Error::Encoding),
        };
        let block_id = match bytes[25] {
            0 if bytes[26..94].iter().all(|byte| *byte == 0) => None,
            1 => Some(BlockId {
                parts: u32::from_le_bytes(bytes[26..30].try_into().unwrap()),
                hash: bytes[30..62].try_into().unwrap(),
                part_set_hash: bytes[62..94].try_into().unwrap(),
            }),
            _ => return Err(Error::Encoding),
        };
        Ok(Self {
            kind,
            height: i64::from_le_bytes(bytes[1..9].try_into().unwrap()),
            round: i32::from_le_bytes(bytes[9..13].try_into().unwrap()),
            seconds: i64::from_le_bytes(bytes[13..21].try_into().unwrap()),
            nanos: i32::from_le_bytes(bytes[21..25].try_into().unwrap()),
            block_id,
            signature: bytes[94..158].try_into().unwrap(),
        })
    }

    pub fn sign_bytes(&self, chain_id: &str) -> Result<Vec<u8>> {
        if chain_id.is_empty()
            || chain_id.len() > 50
            || self.height <= 0
            || self.round < 0
            || !(-62_135_596_800..=253_402_300_799).contains(&self.seconds)
            || !(0..1_000_000_000).contains(&self.nanos)
        {
            return Err(Error::Evidence);
        }
        if self.block_id.is_some_and(|block| {
            block.parts == 0 || block.hash == [0; 32] || block.part_set_hash == [0; 32]
        }) {
            return Err(Error::Evidence);
        }
        let vote = CanonicalVote {
            r#type: self.kind as i32,
            height: self.height,
            round: self.round.into(),
            block_id: self.block_id.map(|block| CanonicalBlockId {
                hash: block.hash.to_vec(),
                part_set_header: Some(CanonicalPartSetHeader {
                    total: block.parts,
                    hash: block.part_set_hash.to_vec(),
                }),
            }),
            timestamp: Some(Timestamp {
                seconds: self.seconds,
                nanos: self.nanos,
            }),
            chain_id: chain_id.into(),
        };
        Ok(vote.encode_length_delimited_to_vec())
    }
}

/// Stake liability must come from authenticated committee history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Liability {
    pub validator: ValidatorKey,
    pub first_active_height: i64,
    pub first_inactive_height: Option<i64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DoubleVote {
    pub first: SignedVote,
    pub second: SignedVote,
}

impl DoubleVote {
    /// Returns the replay-protection key for one canonical vote position.
    /// An L1 Registry transition must persist the key with the penalty.
    pub fn verify(
        &self,
        network_id: &Hash32,
        chain_id: &str,
        liability: &Liability,
    ) -> Result<Hash32> {
        let first = &self.first;
        let second = &self.second;
        if *network_id == [0; 32]
            || first.height != second.height
            || first.round != second.round
            || first.kind != second.kind
            || first.block_id == second.block_id
            || liability.first_active_height <= 0
            || first.height < liability.first_active_height
            || liability
                .first_inactive_height
                .is_some_and(|height| first.height >= height)
        {
            return Err(Error::Evidence);
        }
        let key = public_key(&liability.validator)?;
        for vote in [first, second] {
            let sign_bytes = vote.sign_bytes(chain_id)?;
            key.verify_strict(&sign_bytes, &Signature::from_bytes(&vote.signature))
                .map_err(|_| Error::Signature)?;
        }
        Ok(digest(
            b"Stem CometBFT double vote\0",
            &[
                network_id,
                &(chain_id.len() as u32).to_le_bytes(),
                chain_id.as_bytes(),
                &liability.validator,
                &first.height.to_le_bytes(),
                &first.round.to_le_bytes(),
                &[first.kind as u8],
            ],
        ))
    }
}
