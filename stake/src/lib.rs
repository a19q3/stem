//! Canonical Stake Registry, guardian certificates, and CometBFT vote evidence.
#![no_std]

extern crate alloc;

use alloc::vec::Vec;
use core::fmt;

use ckb_hash::new_blake2b;
use ed25519_dalek::{Signature, VerifyingKey};

pub mod checkpoint;
mod evidence;
pub mod history;
pub mod l1;
pub mod retirement;
pub use evidence::{BlockId, DoubleVote, Liability, SignedVote, VoteKind};

pub type Hash32 = [u8; 32];
pub type ValidatorKey = [u8; 32];

pub const MAX_TOTAL_VOTING_POWER: u64 = (i64::MAX / 8) as u64;
pub const MAX_RECORDS: usize = 256;
const RECORD_BYTES: usize = 32 + 8 + 8 + 1 + 8 + 1 + 8 + 8 + 32;
const REGISTRY_HEADER: usize = 32 + 8 + 32 + 32 + 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Encoding,
    Identity,
    InvalidKey,
    Authority,
    Signature,
    Quorum,
    Configuration,
    Overflow,
    Anchor,
    Sequence,
    Epoch,
    Committee,
    Evidence,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

impl core::error::Error for Error {}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StakeData {
    pub validator_pubkey: ValidatorKey,
    pub bonded_amount: u64,
    pub activation_epoch: u64,
    pub exit_epoch: Option<u64>,
    pub frozen: bool,
    pub first_active_height: i64,
    pub first_inactive_height: i64,
    pub stake_cell_hash: Hash32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Registry {
    network_id: Hash32,
    sequence: u64,
    guardian_generation: Hash32,
    retired_root: Hash32,
    records: Vec<StakeData>,
}

impl Registry {
    pub fn new(
        network_id: Hash32,
        sequence: u64,
        guardian_generation: Hash32,
        retired_root: Hash32,
        records: Vec<StakeData>,
    ) -> Result<Self> {
        if network_id == [0; 32] || guardian_generation == [0; 32] || records.len() > MAX_RECORDS {
            return Err(Error::Encoding);
        }
        let mut last = None;
        let mut bonded_sum = 0u64;
        for entry in &records {
            if last.is_some_and(|previous| previous >= entry.validator_pubkey)
                || entry.first_active_height < 0
                || entry.first_inactive_height < 0
                || (entry.first_inactive_height != 0
                    && entry.first_inactive_height <= entry.first_active_height)
                || (entry.frozen && entry.first_active_height == 0)
                || entry.stake_cell_hash == [0; 32]
                || entry
                    .exit_epoch
                    .is_some_and(|exit| exit <= entry.activation_epoch)
            {
                return Err(Error::Encoding);
            }
            bonded_sum = bonded_sum
                .checked_add(entry.bonded_amount)
                .ok_or(Error::Overflow)?;
            last = Some(entry.validator_pubkey);
        }
        Ok(Self {
            network_id,
            sequence,
            guardian_generation,
            retired_root,
            records,
        })
    }

    pub fn decode(data: &[u8]) -> Result<Self> {
        let header = data.get(..REGISTRY_HEADER).ok_or(Error::Encoding)?;
        let network = header[..32].try_into().unwrap();
        let sequence = u64::from_le_bytes(header[32..40].try_into().unwrap());
        let guardian_generation = header[40..72].try_into().unwrap();
        let retired_root = header[72..104].try_into().unwrap();
        let count = u16::from_le_bytes(header[104..106].try_into().unwrap()) as usize;
        let body = &data[REGISTRY_HEADER..];
        if count > MAX_RECORDS || body.len() != count * RECORD_BYTES {
            return Err(Error::Encoding);
        }
        let mut records = Vec::with_capacity(count);
        for record in body.chunks_exact(RECORD_BYTES) {
            let exit = u64::from_le_bytes(record[49..57].try_into().unwrap());
            let exit_epoch = match (record[48], exit) {
                (0, 0) => None,
                (1, exit) => Some(exit),
                _ => return Err(Error::Encoding),
            };
            records.push(StakeData {
                validator_pubkey: record[..32].try_into().unwrap(),
                bonded_amount: u64::from_le_bytes(record[32..40].try_into().unwrap()),
                activation_epoch: u64::from_le_bytes(record[40..48].try_into().unwrap()),
                exit_epoch,
                frozen: match record[57] {
                    0 => false,
                    1 => true,
                    _ => return Err(Error::Encoding),
                },
                first_active_height: i64::from_le_bytes(record[58..66].try_into().unwrap()),
                first_inactive_height: i64::from_le_bytes(record[66..74].try_into().unwrap()),
                stake_cell_hash: record[74..106].try_into().unwrap(),
            });
        }
        Self::new(
            network,
            sequence,
            guardian_generation,
            retired_root,
            records,
        )
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(REGISTRY_HEADER + self.records.len() * RECORD_BYTES);
        data.extend_from_slice(&self.network_id);
        data.extend_from_slice(&self.sequence.to_le_bytes());
        data.extend_from_slice(&self.guardian_generation);
        data.extend_from_slice(&self.retired_root);
        data.extend_from_slice(&(self.records.len() as u16).to_le_bytes());
        for entry in &self.records {
            data.extend_from_slice(&entry.validator_pubkey);
            data.extend_from_slice(&entry.bonded_amount.to_le_bytes());
            data.extend_from_slice(&entry.activation_epoch.to_le_bytes());
            data.push(u8::from(entry.exit_epoch.is_some()));
            data.extend_from_slice(&entry.exit_epoch.unwrap_or(0).to_le_bytes());
            data.push(u8::from(entry.frozen));
            data.extend_from_slice(&entry.first_active_height.to_le_bytes());
            data.extend_from_slice(&entry.first_inactive_height.to_le_bytes());
            data.extend_from_slice(&entry.stake_cell_hash);
        }
        data
    }

    pub fn network_id(&self) -> Hash32 {
        self.network_id
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn guardian_generation(&self) -> Hash32 {
        self.guardian_generation
    }

    pub fn retired_root(&self) -> Hash32 {
        self.retired_root
    }

    pub fn records(&self) -> &[StakeData] {
        &self.records
    }

    pub fn retired_keys(&self, epoch: u64) -> impl Iterator<Item = ValidatorKey> + '_ {
        self.records.iter().filter_map(move |stake| {
            (stake.frozen
                || stake.first_inactive_height != 0
                || stake.exit_epoch.is_some_and(|exit| exit <= epoch))
            .then_some(stake.validator_pubkey)
        })
    }

    pub fn commitment(&self) -> Hash32 {
        digest(b"Fern Stake Registry\0", &[&self.encode()])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Anchor {
    pub height: u64,
    pub hash: Hash32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StakeDomain {
    pub fern_network_id: Hash32,
    pub ckb_genesis: Hash32,
    pub registry_id: Hash32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RegistryAnchor {
    pub block: Anchor,
    pub sequence: u64,
    pub commitment: Hash32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AcceptedSnapshot {
    pub epoch: u64,
    pub anchor: RegistryAnchor,
}

/// Signers attest the full Registry at the named CKB block and its ancestry
/// from the previous accepted anchor. The verifier has no L1 RPC dependency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StakeSnapshot {
    pub domain: StakeDomain,
    pub registry: RegistryAnchor,
    pub parent: Option<RegistryAnchor>,
    pub target_epoch: u64,
    pub guardian_generation: Hash32,
}

impl StakeSnapshot {
    pub fn encode(&self) -> Vec<u8> {
        let mut message = Vec::new();
        message.extend_from_slice(&self.domain.fern_network_id);
        message.extend_from_slice(&self.domain.ckb_genesis);
        message.extend_from_slice(&self.domain.registry_id);
        message.extend_from_slice(&self.guardian_generation);
        message.extend_from_slice(&self.target_epoch.to_le_bytes());
        encode_anchor(&mut message, &self.registry);
        message.push(u8::from(self.parent.is_some()));
        if let Some(parent) = self.parent {
            encode_anchor(&mut message, &parent);
        }
        message
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let (fixed, parent) = bytes.split_at_checked(217).ok_or(Error::Encoding)?;
        let parent = match (fixed[216], parent.len()) {
            (0, 0) => None,
            (1, 80) => Some(decode_anchor(parent)),
            _ => return Err(Error::Encoding),
        };
        Ok(Self {
            domain: StakeDomain {
                fern_network_id: fixed[..32].try_into().unwrap(),
                ckb_genesis: fixed[32..64].try_into().unwrap(),
                registry_id: fixed[64..96].try_into().unwrap(),
            },
            guardian_generation: fixed[96..128].try_into().unwrap(),
            target_epoch: u64::from_le_bytes(fixed[128..136].try_into().unwrap()),
            registry: decode_anchor(&fixed[136..216]),
            parent,
        })
    }

    pub fn signing_digest(&self) -> Hash32 {
        digest(b"Fern guardian Stake Snapshot\0", &[&self.encode()])
    }

    pub fn verify(
        &self,
        domain: &StakeDomain,
        previous: Option<AcceptedSnapshot>,
        registry: &Registry,
        authority: &Authority,
        signatures: &[Attestation],
        config: &StakeConfig,
    ) -> Result<(AcceptedSnapshot, Vec<Member>)> {
        let validated = self.validate_statement(domain, previous, registry, authority, config)?;
        authority.verify(&self.signing_digest(), signatures)?;
        Ok(validated)
    }

    /// Validate the statement after its authentication by the enclosing protocol.
    pub fn validate_statement(
        &self,
        domain: &StakeDomain,
        previous: Option<AcceptedSnapshot>,
        registry: &Registry,
        authority: &Authority,
        config: &StakeConfig,
    ) -> Result<(AcceptedSnapshot, Vec<Member>)> {
        if self.domain != *domain
            || domain.ckb_genesis == [0; 32]
            || domain.registry_id == [0; 32]
            || domain.fern_network_id != registry.network_id
        {
            return Err(Error::Identity);
        }
        if self.guardian_generation != authority.generation
            || self.guardian_generation != registry.guardian_generation
        {
            return Err(Error::Authority);
        }
        if self.registry.sequence != registry.sequence
            || self.registry.commitment != registry.commitment()
        {
            return Err(Error::Sequence);
        }
        if self.registry.block.hash == [0; 32] {
            return Err(Error::Anchor);
        }
        if let Some(previous) = previous {
            if self.target_epoch <= previous.epoch {
                return Err(Error::Epoch);
            }
            if self.parent != Some(previous.anchor)
                || self.registry.block.height <= previous.anchor.block.height
                || self.registry.block.hash == previous.anchor.block.hash
            {
                return Err(Error::Anchor);
            }
            if self.registry.sequence < previous.anchor.sequence
                || (self.registry.sequence == previous.anchor.sequence
                    && self.registry.commitment != previous.anchor.commitment)
            {
                return Err(Error::Sequence);
            }
        } else if self.target_epoch != 0 || self.parent.is_some() {
            return Err(Error::Epoch);
        }
        let members = config.select(registry, self.target_epoch)?;
        Ok((
            AcceptedSnapshot {
                epoch: self.target_epoch,
                anchor: self.registry,
            },
            members,
        ))
    }
}

fn decode_anchor(bytes: &[u8]) -> RegistryAnchor {
    RegistryAnchor {
        block: Anchor {
            height: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            hash: bytes[8..40].try_into().unwrap(),
        },
        sequence: u64::from_le_bytes(bytes[40..48].try_into().unwrap()),
        commitment: bytes[48..80].try_into().unwrap(),
    }
}

fn encode_anchor(out: &mut Vec<u8>, anchor: &RegistryAnchor) {
    out.extend_from_slice(&anchor.block.height.to_le_bytes());
    out.extend_from_slice(&anchor.block.hash);
    out.extend_from_slice(&anchor.sequence.to_le_bytes());
    out.extend_from_slice(&anchor.commitment);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Member {
    pub key: ValidatorKey,
    pub power: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Attestation {
    pub guardian: u16,
    pub signature: [u8; 64],
}

#[derive(Clone)]
pub struct Authority {
    generation: Hash32,
    guardians: Vec<(VerifyingKey, u64)>,
    total: u64,
}

impl Authority {
    pub fn new(generation: Hash32, members: &[Member]) -> Result<Self> {
        if members.is_empty() || members.len() > MAX_RECORDS || generation == [0; 32] {
            return Err(Error::Authority);
        }
        let mut total = 0u64;
        let mut guardians = Vec::with_capacity(members.len());
        let mut last = None;
        for member in members {
            if member.power == 0 || last.is_some_and(|previous| previous >= member.key) {
                return Err(Error::Authority);
            }
            total = total.checked_add(member.power).ok_or(Error::Overflow)?;
            guardians.push((public_key(&member.key)?, member.power));
            last = Some(member.key);
        }
        if total > MAX_TOTAL_VOTING_POWER {
            return Err(Error::Authority);
        }
        Ok(Self {
            generation,
            guardians,
            total,
        })
    }

    pub fn generation(&self) -> Hash32 {
        self.generation
    }

    pub fn verify(&self, message: &[u8], votes: &[Attestation]) -> Result<()> {
        let mut last = None;
        let mut signed = 0u64;
        for vote in votes {
            if last.is_some_and(|previous| previous >= vote.guardian) {
                return Err(Error::Authority);
            }
            let (key, power) = self
                .guardians
                .get(vote.guardian as usize)
                .ok_or(Error::Authority)?;
            key.verify_strict(message, &Signature::from_bytes(&vote.signature))
                .map_err(|_| Error::Signature)?;
            signed = signed.checked_add(*power).ok_or(Error::Overflow)?;
            last = Some(vote.guardian);
        }
        if 3 * u128::from(signed) <= 2 * u128::from(self.total) {
            return Err(Error::Quorum);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StakeConfig {
    pub minimum_bond: u64,
    pub shannons_per_power: u64,
    pub max_validators: usize,
    pub max_total_power: u64,
    pub blocks_per_epoch: u64,
}

impl StakeConfig {
    fn validate(&self) -> Result<()> {
        if self.shannons_per_power == 0
            || self.minimum_bond < self.shannons_per_power
            || !(1..=MAX_RECORDS).contains(&self.max_validators)
            || !(1..=MAX_TOTAL_VOTING_POWER).contains(&self.max_total_power)
            || self.blocks_per_epoch < 3
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }

    pub fn select(&self, registry: &Registry, epoch: u64) -> Result<Vec<Member>> {
        self.validate()?;
        let height = i64::try_from(self.effective_height(epoch)?).map_err(|_| Error::Overflow)?;
        let mut eligible: Vec<_> = registry
            .records
            .iter()
            .filter(|stake| {
                !stake.frozen
                    && stake.first_active_height > 0
                    && stake.first_active_height <= height
                    && stake.first_inactive_height == 0
                    && stake.bonded_amount >= self.minimum_bond
                    && stake.activation_epoch <= epoch
                    && stake.exit_epoch.is_none_or(|exit| epoch < exit)
            })
            .collect();
        eligible.sort_unstable_by(|a, b| {
            b.bonded_amount
                .cmp(&a.bonded_amount)
                .then_with(|| a.validator_pubkey.cmp(&b.validator_pubkey))
        });
        eligible.truncate(self.max_validators);
        if eligible.is_empty() {
            return Err(Error::Committee);
        }

        let mut total = 0u64;
        let mut validators = Vec::with_capacity(eligible.len());
        for stake in eligible {
            public_key(&stake.validator_pubkey)?;
            let power = stake.bonded_amount / self.shannons_per_power;
            total = total.checked_add(power).ok_or(Error::Overflow)?;
            validators.push(Member {
                key: stake.validator_pubkey,
                power,
            });
        }
        if total > self.max_total_power {
            return Err(Error::Committee);
        }
        validators.sort_unstable_by_key(|validator| validator.key);
        Ok(validators)
    }

    /// An epoch starts at CometBFT height 1 + epoch * blocks_per_epoch.
    pub fn effective_height(&self, epoch: u64) -> Result<u64> {
        self.validate()?;
        epoch
            .checked_mul(self.blocks_per_epoch)
            .and_then(|height| height.checked_add(1))
            .filter(|height| *height <= i64::MAX as u64)
            .ok_or(Error::Overflow)
    }

    /// Commit ValidatorUpdates at H so CometBFT applies them at H+2.
    pub fn update_height(&self, epoch: u64) -> Result<u64> {
        self.effective_height(epoch)?
            .checked_sub(2)
            .ok_or(Error::Epoch)
    }
}

fn public_key(bytes: &ValidatorKey) -> Result<VerifyingKey> {
    let key = VerifyingKey::from_bytes(bytes).map_err(|_| Error::InvalidKey)?;
    if key.is_weak() {
        return Err(Error::InvalidKey);
    }
    Ok(key)
}

fn digest(domain: &[u8], values: &[&[u8]]) -> Hash32 {
    let mut hash = new_blake2b();
    hash.update(domain);
    for value in values {
        hash.update(value);
    }
    let mut result = [0; 32];
    hash.finalize(&mut result);
    result
}
