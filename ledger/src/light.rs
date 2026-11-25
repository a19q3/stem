//! Storage-independent verification of native Jolteon Headers and state records.

use anyhow::{Result, ensure};
use ckb_types::{packed::Byte32, prelude::*};
use stem_bridge_wire::intercell::{Identity, Message, record_key};
use stem_stake::{
    Member,
    jolteon::{Committee, CommitteeChange, FinalizedHeader},
};

use crate::{
    block::Header,
    hash,
    patricia::{Namespace, Patricia, Proof},
};

pub const EXTINGUISHED_FEES_KEY: &[u8] = b"fees/extinguished";

pub struct FinalityVerifier {
    network: Byte32,
    genesis: Byte32,
    trust: Byte32,
    committee: Committee,
    boundary: u64,
}

impl FinalityVerifier {
    /// Both the genesis execution ID and the initial committee must be trusted.
    pub fn new(network: Byte32, genesis: Byte32, members: &[Member]) -> Result<Self> {
        let roster = CommitteeChange {
            epoch: 0,
            activation_height: 1,
            members: members.to_vec(),
        }
        .encode();
        let trust = hash(
            b"Stem light client trust anchor\0",
            &network,
            &[genesis.as_slice(), &roster],
        );
        let committee = Committee::new(network.unpack(), genesis.unpack(), 0, members)?;
        Ok(Self {
            network,
            genesis,
            trust,
            committee,
            boundary: 0,
        })
    }

    pub fn epoch(&self) -> u64 {
        self.committee.epoch()
    }

    pub fn accept_handoff(&mut self, raw: &[u8]) -> Result<VerifiedHeader> {
        let (change, boundary, _) = self.committee.verify_handoff(raw)?;
        ensure!(
            boundary.height() > self.boundary,
            "handoffs must advance execution height"
        );
        let next = Committee::at_anchor(
            self.network.unpack(),
            self.genesis.unpack(),
            change.epoch,
            boundary.id(),
            &change.members,
        )?;
        let verified = self.authenticated(&boundary)?;
        self.committee = next;
        self.boundary = boundary.height();
        Ok(verified)
    }

    pub fn verify(&self, raw: &[u8], terminal: bool) -> Result<VerifiedHeader> {
        let finalized = if terminal {
            self.committee.verify_handoff(raw)?.1
        } else {
            self.committee.verify_finality(raw)?
        };
        self.authenticated(&finalized)
    }

    fn authenticated(&self, finalized: &FinalizedHeader) -> Result<VerifiedHeader> {
        ensure!(
            finalized.height() > self.boundary,
            "proof predates its committee's activation"
        );
        Ok(VerifiedHeader {
            header: Header::decode(finalized.as_bytes())?,
            network: self.network.clone(),
            trust: self.trust.clone(),
        })
    }
}

#[derive(Clone)]
pub struct VerifiedHeader {
    header: Header,
    network: Byte32,
    trust: Byte32,
}

impl VerifiedHeader {
    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn network(&self) -> &Byte32 {
        &self.network
    }

    pub fn same_trust_anchor(&self, other: &Self) -> bool {
        self.trust == other.trust
    }
}

pub fn verify_system_record(
    network: &Byte32,
    header: &Header,
    name: &[u8],
    record: Option<&[u8]>,
    proof: &Proof,
) -> bool {
    let tree = Patricia::new(network.clone(), Namespace::System);
    tree.verify(
        &header.state.system_root.unpack(),
        &tree.record_key(name),
        record.map(|value| tree.value_hash(value)).as_ref(),
        proof,
    )
}

pub fn verify_intercell(
    network: &Byte32,
    header: &Header,
    inbound: bool,
    id: &Identity,
    record: Option<&[u8]>,
    proof: &Proof,
) -> bool {
    record.is_none_or(|raw| {
        Message::decode(raw).is_some_and(|message| {
            message.id() == *id
                && if inbound {
                    message.destination
                } else {
                    message.source
                } == network.as_slice()
        })
    }) && verify_system_record(network, header, &record_key(inbound, id), record, proof)
}
