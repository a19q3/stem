//! Sparse CKB checkpoints authenticated by Stem execution proofs and committee handoffs.

pub mod evidence;

use alloc::vec::Vec;
use prost::Message;

use crate::{Error, Hash32, Result, finality::NATIVE_HEADER_BYTES};

pub use crate::jolteon::roster_id;

use crate::finality::{native_height, native_id};
use crate::jolteon::{Committee, CommitteeChange, ExecutionProof, timestamp};

const MAX_WITNESS_BYTES: usize = 72 * 1024;

/// Fixed Type Script arguments. The first field is the CKB Type ID.
pub struct Policy {
    pub network: Hash32,
    pub ckb_genesis: Hash32,
    pub genesis: Hash32,
    pub initial_roster: Hash32,
}

impl Policy {
    pub fn decode(args: &[u8]) -> Result<Self> {
        let (words, rest) = args.as_chunks::<32>();
        let [_type_id, network, ckb_genesis, genesis, initial_roster] = words else {
            return Err(Error::Encoding);
        };
        if !rest.is_empty() || words.contains(&[0; 32]) {
            return Err(Error::Identity);
        }
        Ok(Self {
            network: *network,
            ckb_genesis: *ckb_genesis,
            genesis: *genesis,
            initial_roster: *initial_roster,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tip {
    pub epoch_anchor: Hash32,
    pub roster: Hash32,
    pub header: [u8; NATIVE_HEADER_BYTES],
}

impl Tip {
    pub const SIZE: usize = 64 + NATIVE_HEADER_BYTES;

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() != Self::SIZE {
            return Err(Error::Encoding);
        }
        let tip = Self {
            epoch_anchor: bytes[..32].try_into().unwrap(),
            roster: bytes[32..64].try_into().unwrap(),
            header: bytes[64..].try_into().unwrap(),
        };
        if tip.epoch_anchor == [0; 32] || tip.roster == [0; 32] || tip.height() == 0 {
            return Err(Error::Anchor);
        }
        Ok(tip)
    }

    pub fn encode(&self) -> Vec<u8> {
        [&self.epoch_anchor[..], &self.roster, &self.header].concat()
    }

    pub fn height(&self) -> u64 {
        native_height(&self.header)
    }

    pub fn timestamp_ms(&self) -> u64 {
        timestamp(&self.header)
    }

    pub fn system_root(&self) -> Hash32 {
        self.header[112..144].try_into().unwrap()
    }
}

/// A CKB fee fund can only subsidize this checkpoint and within these bounds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FundingPolicy {
    pub checkpoint_code: Hash32,
    pub checkpoint_type_id: Hash32,
    pub network: Hash32,
    pub ckb_genesis: Hash32,
    pub fee_cap: u64,
    pub interval_ms: u64,
}

impl FundingPolicy {
    pub const SIZE: usize = 144;

    pub fn decode(args: &[u8]) -> Result<Self> {
        if args.len() != Self::SIZE {
            return Err(Error::Encoding);
        }
        let (words, _) = args[..128].as_chunks::<32>();
        let [checkpoint_code, checkpoint_type_id, network, ckb_genesis] = words else {
            return Err(Error::Encoding);
        };
        let fee_cap = u64::from_le_bytes(args[128..136].try_into().unwrap());
        let interval_ms = u64::from_le_bytes(args[136..].try_into().unwrap());
        if words.contains(&[0; 32]) || fee_cap == 0 || interval_ms == 0 {
            return Err(Error::Configuration);
        }
        Ok(Self {
            checkpoint_code: *checkpoint_code,
            checkpoint_type_id: *checkpoint_type_id,
            network: *network,
            ckb_genesis: *ckb_genesis,
            fee_cap,
            interval_ms,
        })
    }

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[..32].copy_from_slice(&self.checkpoint_code);
        bytes[32..64].copy_from_slice(&self.checkpoint_type_id);
        bytes[64..96].copy_from_slice(&self.network);
        bytes[96..128].copy_from_slice(&self.ckb_genesis);
        bytes[128..136].copy_from_slice(&self.fee_cap.to_le_bytes());
        bytes[136..].copy_from_slice(&self.interval_ms.to_le_bytes());
        bytes
    }

    pub fn verify_checkpoint_type(&self, code_hash: &Hash32, args: &[u8]) -> Result<Policy> {
        if code_hash != &self.checkpoint_code || !args.starts_with(&self.checkpoint_type_id) {
            return Err(Error::Identity);
        }
        let policy = Policy::decode(args)?;
        if policy.network != self.network || policy.ckb_genesis != self.ckb_genesis {
            return Err(Error::Identity);
        }
        Ok(policy)
    }

    /// The pinned checkpoint Type Script separately authenticates both tips.
    pub fn verify_charge(&self, previous: &Tip, next: &Tip, fee: u64) -> Result<()> {
        if fee == 0 || fee > self.fee_cap {
            return Err(Error::Configuration);
        }
        if next.height() <= previous.height() {
            return Err(Error::Sequence);
        }
        let elapsed = next
            .timestamp_ms()
            .checked_sub(previous.timestamp_ms())
            .ok_or(Error::Anchor)?;
        if next.epoch_anchor == previous.epoch_anchor && elapsed < self.interval_ms {
            return Err(Error::Anchor);
        }
        Ok(())
    }
}

#[derive(Clone, Message)]
pub struct Advance {
    #[prost(bytes, tag = "1")]
    pub roster: Vec<u8>,
    #[prost(bytes, tag = "2")]
    pub genesis_header: Vec<u8>,
    #[prost(bytes, tag = "3")]
    pub previous_frontier: Vec<u8>,
    #[prost(bytes, tag = "4")]
    pub suffix: Vec<u8>,
    #[prost(bytes, tag = "5")]
    pub proof: Vec<u8>,
    #[prost(bool, tag = "6")]
    pub terminal: bool,
}

pub fn verify(policy: &Policy, previous: Option<&Tip>, bytes: &[u8]) -> Result<Tip> {
    if bytes.len() > MAX_WITNESS_BYTES {
        return Err(Error::Encoding);
    }
    let advance = Advance::decode(bytes).map_err(|_| Error::Encoding)?;
    if advance.encode_to_vec() != bytes {
        return Err(Error::Encoding);
    }
    let roster = CommitteeChange::decode(&advance.roster)?;
    let roster_commitment = roster_id(&advance.roster);
    let (anchor, old_header) = match previous {
        Some(tip) => {
            if !advance.genesis_header.is_empty() || tip.roster != roster_commitment {
                return Err(Error::Committee);
            }
            (tip.epoch_anchor, &tip.header)
        }
        None => {
            let header = advance
                .genesis_header
                .as_slice()
                .try_into()
                .map_err(|_| Error::Encoding)?;
            if native_height(header) != 0
                || native_id(&policy.network, header) != policy.genesis
                || roster_commitment != policy.initial_roster
                || roster.epoch != 0
            {
                return Err(Error::Anchor);
            }
            (policy.genesis, header)
        }
    };
    let committee = Committee::at_anchor(
        policy.network,
        policy.genesis,
        roster.epoch,
        anchor,
        &roster.members,
    )?;
    let proof = if advance.terminal {
        ExecutionProof::TerminalHandoff(&advance.proof)
    } else {
        ExecutionProof::TwoChain(&advance.proof)
    };
    let executed = committee.authenticate_execution(proof)?;
    let header = executed.header;
    let new_roster = executed
        .successor
        .map(|successor| roster_id(&successor.encode()));
    if header.height() < roster.activation_height {
        return Err(Error::Epoch);
    }
    let (suffix, remaining) = advance.suffix.as_chunks::<32>();
    if !remaining.is_empty() {
        return Err(Error::Encoding);
    }
    header.verify_extension(old_header, &advance.previous_frontier, suffix)?;
    let (epoch_anchor, roster) =
        new_roster.map_or((anchor, roster_commitment), |roster| (header.id(), roster));
    Ok(Tip {
        epoch_anchor,
        roster,
        header: *header.as_bytes(),
    })
}

#[cfg(test)]
mod funding_tests {
    use super::*;

    #[test]
    fn charge_binds_checkpoint_identity_fee_and_time() -> Result<()> {
        let policy = FundingPolicy {
            checkpoint_code: [1; 32],
            checkpoint_type_id: [2; 32],
            network: [3; 32],
            ckb_genesis: [4; 32],
            fee_cap: 100,
            interval_ms: 1000,
        };
        let encoded = policy.encode();
        assert_eq!(FundingPolicy::decode(&encoded)?, policy);
        let mut invalid = encoded;
        invalid[128..136].fill(0);
        assert_eq!(FundingPolicy::decode(&invalid), Err(Error::Configuration));

        let mut args = [[2; 32], [3; 32], [4; 32], [5; 32], [6; 32]].concat();
        policy.verify_checkpoint_type(&[1; 32], &args)?;
        args[0] ^= 1;
        assert!(policy.verify_checkpoint_type(&[1; 32], &args).is_err());
        args[0] ^= 1;
        args[32] ^= 1;
        assert!(policy.verify_checkpoint_type(&[1; 32], &args).is_err());

        let tip = |anchor, height: u64, time: u64| {
            let mut header = [0; NATIVE_HEADER_BYTES];
            header[..8].copy_from_slice(&height.to_le_bytes());
            header[8..16].copy_from_slice(&time.to_le_bytes());
            Tip {
                epoch_anchor: [anchor; 32],
                roster: [7; 32],
                header,
            }
        };
        let previous = tip(8, 21, 5000);
        let next = tip(8, 22, 6000);
        policy.verify_charge(&previous, &next, 100)?;
        assert_eq!(
            policy.verify_charge(&previous, &next, 0),
            Err(Error::Configuration)
        );
        assert_eq!(
            policy.verify_charge(&previous, &next, 101),
            Err(Error::Configuration)
        );
        assert_eq!(
            policy.verify_charge(&previous, &previous, 100),
            Err(Error::Sequence)
        );
        assert_eq!(
            policy.verify_charge(&previous, &tip(8, 22, 5999), 100),
            Err(Error::Anchor)
        );
        policy.verify_charge(&previous, &tip(9, 22, 5000), 100)?;
        assert_eq!(
            policy.verify_charge(&previous, &tip(9, 22, 4999), 100),
            Err(Error::Anchor)
        );
        Ok(())
    }
}
