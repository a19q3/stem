//! CKB Checkpoint Cell data, append-only history checks, and authorization.

use alloc::vec::Vec;

use crate::{
    Attestation, Authority, BlockId, Error, Hash32, Member, Result, digest, history::Frontier,
};

pub const MAX_MEMBERS: usize = 16;
pub const MAX_SUFFIX: usize = 128;
pub const MAX_UPDATE_BYTES: usize = 2 + 32 * MAX_SUFFIX + 2 * (4 + MAX_MEMBERS * (40 + 66));
pub const ZERO_OUT_POINT: [u8; 36] = [0; 36];
const FIXED_BYTES: usize = 8 + 7 * 32 + 2 * 68;

pub fn stake_snapshot_commitment(network: &Hash32, snapshot: &[u8]) -> Hash32 {
    digest(b"Stem checkpoint Stake Snapshot\0", &[network, snapshot])
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Domain {
    pub network: Hash32,
    pub ckb_genesis: Hash32,
    pub checkpoint_type: Hash32,
}

impl BlockId {
    pub(crate) fn decode(bytes: &mut &[u8]) -> Result<Self> {
        let block = Self {
            hash: take(bytes)?,
            parts: u32::from_le_bytes(take(bytes)?),
            part_set_hash: take(bytes)?,
        };
        if block.hash == [0; 32] || block.parts == 0 || block.part_set_hash == [0; 32] {
            return Err(Error::Encoding);
        }
        Ok(block)
    }

    pub(crate) fn encode(&self, bytes: &mut Vec<u8>) {
        bytes.extend_from_slice(&self.hash);
        bytes.extend_from_slice(&self.parts.to_le_bytes());
        bytes.extend_from_slice(&self.part_set_hash);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Checkpoint {
    pub sequence: u64,
    pub previous: Hash32,
    pub header: Hash32,
    pub block: BlockId,
    pub successor: BlockId,
    pub committee: Hash32,
    pub next_committee: Hash32,
    pub stake_snapshot: Hash32,
    pub handoffs: Hash32,
    pub generation: Hash32,
    pub history: Frontier,
}

impl Checkpoint {
    pub const MAX_SIZE: usize = FIXED_BYTES + 8 + 32 * 64;

    pub fn decode(network: Hash32, mut bytes: &[u8]) -> Result<Self> {
        let record = Self {
            sequence: u64::from_le_bytes(take(&mut bytes)?),
            previous: take(&mut bytes)?,
            header: take(&mut bytes)?,
            block: BlockId::decode(&mut bytes)?,
            successor: BlockId::decode(&mut bytes)?,
            committee: take(&mut bytes)?,
            next_committee: take(&mut bytes)?,
            stake_snapshot: take(&mut bytes)?,
            handoffs: take(&mut bytes)?,
            generation: take(&mut bytes)?,
            history: Frontier::decode(network, bytes)?,
        };
        if record.height() == 0
            || record.height() >= i64::MAX as u64
            || record.header == [0; 32]
            || record.committee == [0; 32]
            || record.next_committee == [0; 32]
            || record.handoffs == [0; 32]
            || record.generation == [0; 32]
            || record.block.hash == record.successor.hash
        {
            return Err(Error::Encoding);
        }
        Ok(record)
    }

    pub fn height(&self) -> u64 {
        self.history.count()
    }

    pub fn app_hash(&self, network: &Hash32) -> Hash32 {
        digest(
            b"Stem AppHash\0",
            &[network, &self.height().to_le_bytes(), &self.header],
        )
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(FIXED_BYTES + 8 + 32 * 64);
        bytes.extend_from_slice(&self.sequence.to_le_bytes());
        bytes.extend_from_slice(&self.previous);
        bytes.extend_from_slice(&self.header);
        self.block.encode(&mut bytes);
        self.successor.encode(&mut bytes);
        bytes.extend_from_slice(&self.committee);
        bytes.extend_from_slice(&self.next_committee);
        bytes.extend_from_slice(&self.stake_snapshot);
        bytes.extend_from_slice(&self.handoffs);
        bytes.extend_from_slice(&self.generation);
        bytes.extend_from_slice(&self.history.encode());
        bytes
    }

    pub fn commitment(&self, domain: &Domain) -> Hash32 {
        digest(
            b"Stem CKB Checkpoint Cell\0",
            &[
                &domain.network,
                &domain.ckb_genesis,
                &domain.checkpoint_type,
                &self.encode(),
            ],
        )
    }

    pub fn signing_message(&self, domain: &Domain, predecessor: &[u8; 36]) -> Hash32 {
        digest(
            b"Stem CKB Checkpoint publication\0",
            &[predecessor, &self.commitment(domain)],
        )
    }

    pub fn verify_parent(
        &self,
        domain: &Domain,
        old: Option<&Self>,
        predecessor: &[u8; 36],
        suffix: &[Hash32],
    ) -> Result<()> {
        let Some(old) = old else {
            if self.sequence == 0
                && self.previous == [0; 32]
                && *predecessor == ZERO_OUT_POINT
                && suffix.is_empty()
            {
                return Ok(());
            }
            return Err(Error::Sequence);
        };
        if *predecessor == ZERO_OUT_POINT
            || self.sequence != old.sequence.checked_add(1).ok_or(Error::Overflow)?
            || self.previous != old.commitment(domain)
            || self.height() <= old.height()
        {
            return Err(Error::Sequence);
        }
        // The new prefix must extend both the old peaks and the old checkpoint
        // Header. The latter was at the tip and was absent from the old peaks.
        let mut prefix = old.history.clone();
        prefix.append(old.height(), &old.header)?;
        if !prefix.verify_extension(self.height(), &self.history.root(), suffix) {
            return Err(Error::Anchor);
        }
        Ok(())
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Certificate {
    pub members: Vec<Member>,
    pub votes: Vec<Attestation>,
}

impl Certificate {
    pub fn generation(members: &[Member]) -> Result<Hash32> {
        if !(1..=MAX_MEMBERS).contains(&members.len()) {
            return Err(Error::Authority);
        }
        let mut raw = Vec::with_capacity(40 * members.len());
        for member in members {
            raw.extend_from_slice(&member.key);
            raw.extend_from_slice(&member.power.to_le_bytes());
        }
        Ok(digest(b"Stem CKB Checkpoint authority\0", &[&raw]))
    }

    pub fn encode(&self, bytes: &mut Vec<u8>) -> Result<()> {
        Self::generation(&self.members)?;
        if !(1..=self.members.len()).contains(&self.votes.len()) {
            return Err(Error::Authority);
        }
        bytes.extend_from_slice(&(self.members.len() as u16).to_le_bytes());
        for member in &self.members {
            bytes.extend_from_slice(&member.key);
            bytes.extend_from_slice(&member.power.to_le_bytes());
        }
        bytes.extend_from_slice(&(self.votes.len() as u16).to_le_bytes());
        for vote in &self.votes {
            bytes.extend_from_slice(&vote.guardian.to_le_bytes());
            bytes.extend_from_slice(&vote.signature);
        }
        Ok(())
    }

    fn decode(bytes: &mut &[u8]) -> Result<Self> {
        let member_count = count(bytes, MAX_MEMBERS)?;
        let members = (0..member_count)
            .map(|_| {
                Ok(Member {
                    key: take(bytes)?,
                    power: u64::from_le_bytes(take(bytes)?),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let vote_count = count(bytes, member_count)?;
        let votes = (0..vote_count)
            .map(|_| {
                Ok(Attestation {
                    guardian: u16::from_le_bytes(take(bytes)?),
                    signature: take(bytes)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { members, votes })
    }

    fn verify(&self, expected: Hash32, message: &Hash32) -> Result<()> {
        if Self::generation(&self.members)? != expected {
            return Err(Error::Authority);
        }
        Authority::new(expected, &self.members)?.verify(message, &self.votes)
    }
}

#[derive(Debug, Eq, PartialEq)]
pub struct Update {
    pub suffix: Vec<Hash32>,
    pub authority: Certificate,
    pub rotation: Option<Certificate>,
}

impl Update {
    pub fn decode(mut bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_UPDATE_BYTES {
            return Err(Error::Encoding);
        }
        let length = count(&mut bytes, MAX_SUFFIX)?;
        let suffix = (0..length)
            .map(|_| take(&mut bytes))
            .collect::<Result<Vec<_>>>()?;
        let authority = Certificate::decode(&mut bytes)?;
        let rotation = if bytes.is_empty() {
            None
        } else {
            Some(Certificate::decode(&mut bytes)?)
        };
        if !bytes.is_empty() {
            return Err(Error::Encoding);
        }
        Ok(Self {
            suffix,
            authority,
            rotation,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.suffix.len() > MAX_SUFFIX {
            return Err(Error::Encoding);
        }
        let mut bytes = (self.suffix.len() as u16).to_le_bytes().to_vec();
        for node in &self.suffix {
            bytes.extend_from_slice(node);
        }
        self.authority.encode(&mut bytes)?;
        if let Some(rotation) = &self.rotation {
            rotation.encode(&mut bytes)?;
        }
        Ok(bytes)
    }

    pub fn verify(
        &self,
        domain: &Domain,
        initial_generation: Hash32,
        old: Option<&Checkpoint>,
        next: &Checkpoint,
        predecessor: &[u8; 36],
    ) -> Result<()> {
        next.verify_parent(domain, old, predecessor, &self.suffix)?;
        let current = old.map_or(initial_generation, |record| record.generation);
        let message = next.signing_message(domain, predecessor);
        self.authority.verify(current, &message)?;
        match &self.rotation {
            None if current == next.generation => Ok(()),
            Some(proof) if old.is_some() && current != next.generation => {
                proof.verify(next.generation, &message)
            }
            _ => Err(Error::Authority),
        }
    }
}

fn take<const N: usize>(bytes: &mut &[u8]) -> Result<[u8; N]> {
    let (item, remaining) = bytes.split_at_checked(N).ok_or(Error::Encoding)?;
    *bytes = remaining;
    Ok(item.try_into().expect("fixed-width field"))
}

fn count(bytes: &mut &[u8], limit: usize) -> Result<usize> {
    let count = u16::from_le_bytes(take(bytes)?) as usize;
    if count > limit {
        return Err(Error::Encoding);
    }
    Ok(count)
}
