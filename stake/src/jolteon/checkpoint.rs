//! Sparse CKB checkpoints authenticated by Stem execution proofs and committee handoffs.

use alloc::vec::Vec;
use prost::Message;

use crate::{Error, Hash32, Result, digest, finality::NATIVE_HEADER_BYTES};

use super::{Committee, CommitteeChange, native_height, native_id};

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

    pub fn system_root(&self) -> Hash32 {
        self.header[112..144].try_into().unwrap()
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

pub fn roster_id(record: &[u8]) -> Hash32 {
    digest(b"Stem Jolteon checkpoint roster\0", &[record])
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
    let (header, new_roster) = if advance.terminal {
        let (successor, header, _) = committee.verify_handoff(&advance.proof)?;
        (header, Some(roster_id(&successor.encode())))
    } else {
        (committee.verify_finality(&advance.proof)?, None)
    };
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
