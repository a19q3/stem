//! State transitions enforced by the CKB Stake Registry contract.

use alloc::vec::Vec;

use ed25519_dalek::Signature;

use crate::{
    Attestation, Authority, DoubleVote, Error, Hash32, Liability, Member, Registry, Result,
    StakeData, ValidatorKey, digest, finality::CheckpointId, patricia::decode_path, public_key,
    retirement,
};

const EMPTY: Hash32 = [0; 32];
pub const RELATIVE_BLOCKS: u64 = 1 << 63;
pub const MAX_DELAY: u64 = (1 << 56) - 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StakeCell {
    pub owner: Hash32,
    pub first_active_height: i64,
    pub first_inactive_height: i64,
    pub penalty: Hash32,
}

impl StakeCell {
    pub const SIZE: usize = 32 + 8 + 8 + 32;

    pub fn encode(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[..32].copy_from_slice(&self.owner);
        bytes[32..40].copy_from_slice(&self.first_active_height.to_le_bytes());
        bytes[40..48].copy_from_slice(&self.first_inactive_height.to_le_bytes());
        bytes[48..].copy_from_slice(&self.penalty);
        bytes
    }

    pub fn decode(data: &[u8]) -> Result<Self> {
        if data.len() != Self::SIZE {
            return Err(Error::Encoding);
        }
        let result = Self {
            owner: data[..32].try_into().unwrap(),
            first_active_height: i64::from_le_bytes(data[32..40].try_into().unwrap()),
            first_inactive_height: i64::from_le_bytes(data[40..48].try_into().unwrap()),
            penalty: data[48..].try_into().unwrap(),
        };
        if result.owner == EMPTY
            || result.first_active_height < 0
            || result.first_inactive_height < 0
            || (result.first_inactive_height != 0
                && result.first_active_height >= result.first_inactive_height)
        {
            return Err(Error::Encoding);
        }
        Ok(result)
    }

    pub fn verify_record(record: &StakeData, data: &[u8]) -> Result<Self> {
        let (state, metadata) = data.split_at_checked(24).ok_or(Error::Encoding)?;
        let fields = [
            record.bonded_amount,
            record.activation_epoch,
            record.exit_epoch.unwrap_or(0),
        ];
        if !fields
            .iter()
            .zip(state.chunks_exact(8))
            .all(|(value, bytes)| value.to_le_bytes() == bytes)
        {
            return Err(Error::Encoding);
        }
        let cell = Self::decode(metadata)?;
        if !cell.matches_record(record) {
            return Err(Error::Identity);
        }
        Ok(cell)
    }

    fn matches_record(self, record: &StakeData) -> bool {
        self.first_active_height == record.first_active_height
            && self.first_inactive_height == record.first_inactive_height
            && record.frozen == (self.penalty != EMPTY)
            && self.commitment() == record.stake_cell_hash
    }

    pub fn commitment(self) -> Hash32 {
        digest(b"Stem CKB Stake Cell\0", &[&self.encode()])
    }
}

pub struct Policy<'a> {
    pub registry_id: Hash32,
    pub network_id: Hash32,
    pub ckb_genesis: Hash32,
    pub chain_id: &'a str,
    pub minimum_bond: u64,
    pub blocks_per_epoch: u64,
    pub unbonding_blocks: u64,
    pub checkpoint: CheckpointId,
}

impl Policy<'_> {
    pub const MIN_ARGS: usize = 185;
    pub const MAX_ARGS: usize = 234;

    pub fn decode(registry_id: Hash32, args: &[u8]) -> Result<Policy<'_>> {
        if !(Self::MIN_ARGS..=Self::MAX_ARGS).contains(&args.len()) {
            return Err(Error::Encoding);
        }
        let policy = Policy {
            registry_id,
            network_id: args[32..64].try_into().unwrap(),
            ckb_genesis: args[64..96].try_into().unwrap(),
            minimum_bond: u64::from_le_bytes(args[96..104].try_into().unwrap()),
            blocks_per_epoch: u64::from_le_bytes(args[104..112].try_into().unwrap()),
            unbonding_blocks: u64::from_le_bytes(args[112..120].try_into().unwrap()),
            checkpoint: CheckpointId {
                code_hash: args[120..152].try_into().unwrap(),
                type_id: args[152..184].try_into().unwrap(),
            },
            chain_id: core::str::from_utf8(&args[184..]).map_err(|_| Error::Encoding)?,
        };
        policy.validate()?;
        Ok(policy)
    }

    pub fn validate(&self) -> Result<()> {
        self.checkpoint.validate()?;
        if [self.registry_id, self.network_id, self.ckb_genesis].contains(&EMPTY)
            || self.chain_id.is_empty()
            || self.chain_id.len() > 50
            || !self
                .chain_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || self.minimum_bond == 0
            || self.blocks_per_epoch < 3
            || !(1..=MAX_DELAY).contains(&self.unbonding_blocks)
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }

    fn epoch_height(&self, epoch: u64) -> Result<i64> {
        epoch
            .checked_mul(self.blocks_per_epoch)
            .and_then(|value| value.checked_add(1))
            .and_then(|height| i64::try_from(height).ok())
            .ok_or(Error::Overflow)
    }
}

pub fn guardian_generation(members: &[Member]) -> Hash32 {
    let mut bytes = Vec::with_capacity(40 * members.len());
    for member in members {
        bytes.extend_from_slice(&member.key);
        bytes.extend_from_slice(&member.power.to_le_bytes());
    }
    digest(b"Stem CKB Stake authority\0", &[&bytes])
}

pub struct GuardianProof<'a> {
    pub committee: &'a [Member],
    pub votes: &'a [Attestation],
}

impl GuardianProof<'_> {
    pub fn generation(&self) -> Hash32 {
        guardian_generation(self.committee)
    }

    pub fn verify(&self, expected_generation: Hash32, message: &Hash32) -> Result<()> {
        if self.generation() != expected_generation {
            return Err(Error::Authority);
        }
        Authority::new(expected_generation, self.committee)?.verify(message, self.votes)
    }
}

pub struct ChangeContext<'a> {
    pub policy: &'a Policy<'a>,
    pub old_registry: &'a Registry,
    pub new_registry: &'a Registry,
    pub validator: ValidatorKey,
    pub old: Option<(StakeData, StakeCell)>,
    pub new: Option<(StakeData, StakeCell)>,
    pub owner_authorized: bool,
    pub since: u64,
    pub fee: u64,
}

#[derive(Clone, Copy)]
pub struct CheckpointState {
    pub height: u64,
    pub system_root: Hash32,
}

pub enum Action<'a> {
    Register([u8; 64], &'a [u8]),
    Increase,
    RequestExit,
    Activate(GuardianProof<'a>),
    Retire {
        checkpoint: CheckpointState,
        path: &'a [u8],
    },
    Withdraw(&'a [u8]),
    Freeze(DoubleVote),
    FreezeJolteon {
        policy: &'a crate::jolteon::checkpoint::Policy,
        tip: &'a crate::jolteon::checkpoint::Tip,
        evidence: &'a [u8],
    },
    FinalizeSlash,
}

impl ChangeContext<'_> {
    pub fn verify(&self, action: Action<'_>) -> Result<()> {
        verify_sequence(self.policy, self.old_registry, self.new_registry)?;
        if self.old_registry.guardian_generation != self.new_registry.guardian_generation {
            return Err(Error::Authority);
        }
        let prior_root = self.old_registry.retired_root;
        let next_root = match &action {
            Action::Withdraw(path) => {
                retirement::insert(&self.policy.network_id, &self.validator, &prior_root, path)?
            }
            _ => prior_root,
        };
        if next_root != self.new_registry.retired_root {
            return Err(Error::Sequence);
        }
        let stake_data = |registry: &Registry| {
            registry
                .records
                .binary_search_by_key(&self.validator, |stake| stake.validator_pubkey)
                .ok()
                .map(|index| registry.records[index])
        };
        let (old_record, new_record) =
            (stake_data(self.old_registry), stake_data(self.new_registry));
        if old_record == new_record
            || !self
                .old_registry
                .records
                .iter()
                .filter(|stake| stake.validator_pubkey != self.validator)
                .eq(self
                    .new_registry
                    .records
                    .iter()
                    .filter(|stake| stake.validator_pubkey != self.validator))
            || !matches_cell(old_record, self.old)
            || !matches_cell(new_record, self.new)
        {
            return Err(Error::Sequence);
        }
        match action {
            Action::Register(sig, path) => {
                let (stake, cell) = self.new.ok_or(Error::Sequence)?;
                if old_record.is_some()
                    || !self.owner_authorized
                    || stake.bonded_amount < self.policy.minimum_bond
                    || stake.exit_epoch.is_some()
                    || stake.frozen
                    || cell.first_active_height != 0
                    || cell.first_inactive_height != 0
                    || cell.penalty != EMPTY
                {
                    return Err(Error::Sequence);
                }
                self.policy.epoch_height(stake.activation_epoch)?;
                retirement::insert(&self.policy.network_id, &self.validator, &prior_root, path)?;
                public_key(&self.validator)?
                    .verify_strict(
                        &transition_digest(
                            b"Stem CKB Stake registration\0",
                            self.policy,
                            self.old_registry,
                            self.new_registry,
                        ),
                        &Signature::from_bytes(&sig),
                    )
                    .map_err(|_| Error::Signature)
            }
            Action::Increase | Action::RequestExit => {
                let (before, old_cell) = self.old.ok_or(Error::Sequence)?;
                let (after, new_cell) = self.new.ok_or(Error::Sequence)?;
                let allowed = match action {
                    Action::Increase => {
                        after.bonded_amount > before.bonded_amount
                            && after.exit_epoch == before.exit_epoch
                            && old_cell.penalty == EMPTY
                    }
                    Action::RequestExit => {
                        after.bonded_amount == before.bonded_amount && after.exit_epoch.is_some()
                    }
                    _ => unreachable!(),
                };
                if !allowed
                    || !self.owner_authorized
                    || before.exit_epoch.is_some()
                    || old_cell != new_cell
                    || before.activation_epoch != after.activation_epoch
                    || before.frozen != after.frozen
                {
                    return Err(Error::Sequence);
                }
                if let Some(epoch) = after.exit_epoch {
                    self.policy.epoch_height(epoch)?;
                }
                Ok(())
            }
            Action::Activate(proof) => {
                let (before, old_cell) = self.old.ok_or(Error::Sequence)?;
                let (_, new_cell) = self.new.ok_or(Error::Sequence)?;
                if before.bonded_amount == 0
                    || before.frozen
                    || old_cell.first_active_height != 0
                    || old_cell.first_inactive_height != 0
                    || new_cell.first_active_height
                        < self.policy.epoch_height(before.activation_epoch)?
                    || new_cell
                        != (StakeCell {
                            first_active_height: new_cell.first_active_height,
                            ..old_cell
                        })
                {
                    return Err(Error::Epoch);
                }
                self.verify_cell_update(before, new_cell)?;
                self.certify(b"Stem CKB Stake activation\0", &proof)
            }
            Action::Retire { checkpoint, path } => {
                let (before, old_cell) = self.old.ok_or(Error::Sequence)?;
                let (_, new_cell) = self.new.ok_or(Error::Sequence)?;
                let expected = retirement_cell(
                    self.policy,
                    &before,
                    old_cell,
                    new_cell.first_inactive_height,
                )?;
                self.verify_cell_update(before, expected)?;
                let effective =
                    u64::try_from(expected.first_inactive_height).map_err(|_| Error::Epoch)?;
                retirement::verify_marker(
                    &self.policy.network_id,
                    &self.policy.registry_id,
                    &self.validator,
                    effective,
                    checkpoint.height,
                    &checkpoint.system_root,
                    decode_path(path)?.into_iter(),
                )
            }
            Action::Withdraw(_) => {
                let (before, cell) = self.old.ok_or(Error::Sequence)?;
                if self.new.is_some()
                    || !self.owner_authorized
                    || cell.first_inactive_height == 0
                    || (before.frozen && before.bonded_amount != 0)
                    || (self.since & !MAX_DELAY) != RELATIVE_BLOCKS
                    || (self.since & MAX_DELAY) < self.policy.unbonding_blocks
                    || new_record.is_some()
                {
                    return Err(Error::Sequence);
                }
                Ok(())
            }
            Action::Freeze(evidence) => self.freeze(|liability| {
                evidence.verify(&self.policy.network_id, self.policy.chain_id, liability)
            }),
            Action::FreezeJolteon {
                policy,
                tip,
                evidence,
            } => {
                if policy.network != self.policy.network_id
                    || policy.ckb_genesis != self.policy.ckb_genesis
                {
                    return Err(Error::Identity);
                }
                self.freeze(|liability| {
                    crate::jolteon::evidence::verify(policy, tip, liability, evidence)
                })
            }
            Action::FinalizeSlash => {
                let (before, cell) = self.old.ok_or(Error::Sequence)?;
                if !before.frozen
                    || before.bonded_amount == 0
                    || cell.penalty == EMPTY
                    || cell.first_inactive_height == 0
                    || self.fee < before.bonded_amount
                {
                    return Err(Error::Evidence);
                }
                self.verify_cell_update(
                    StakeData {
                        bonded_amount: 0,
                        ..before
                    },
                    cell,
                )
            }
        }
    }

    fn freeze(&self, verify: impl FnOnce(&Liability) -> Result<Hash32>) -> Result<()> {
        let (before, old_cell) = self.old.ok_or(Error::Sequence)?;
        if before.bonded_amount == 0 || before.frozen || old_cell.penalty != EMPTY {
            return Err(Error::Evidence);
        }
        let penalty = verify(&Liability {
            validator: self.validator,
            first_active_height: old_cell.first_active_height,
            first_inactive_height: (old_cell.first_inactive_height != 0)
                .then_some(old_cell.first_inactive_height),
        })?;
        self.verify_cell_update(
            StakeData {
                frozen: true,
                ..before
            },
            StakeCell {
                penalty,
                ..old_cell
            },
        )
    }

    fn verify_cell_update(&self, before: StakeData, cell: StakeCell) -> Result<()> {
        if self.new
            != Some((
                StakeData {
                    first_active_height: cell.first_active_height,
                    first_inactive_height: cell.first_inactive_height,
                    stake_cell_hash: cell.commitment(),
                    ..before
                },
                cell,
            ))
        {
            return Err(Error::Sequence);
        }
        Ok(())
    }

    fn certify(&self, domain: &[u8], proof: &GuardianProof<'_>) -> Result<()> {
        let message = transition_digest(domain, self.policy, self.old_registry, self.new_registry);
        proof.verify(self.old_registry.guardian_generation, &message)
    }
}

pub fn retirement_cell(
    policy: &Policy<'_>,
    record: &StakeData,
    cell: StakeCell,
    inactive: i64,
) -> Result<StakeCell> {
    let earliest = if record.frozen {
        cell.first_active_height
            .checked_add(1)
            .ok_or(Error::Overflow)?
    } else {
        policy.epoch_height(record.exit_epoch.ok_or(Error::Epoch)?)?
    };
    if cell.first_inactive_height != 0
        || inactive < earliest
        || inactive <= cell.first_active_height
    {
        return Err(Error::Epoch);
    }
    Ok(StakeCell {
        first_inactive_height: inactive,
        ..cell
    })
}

fn matches_cell(record: Option<StakeData>, cell: Option<(StakeData, StakeCell)>) -> bool {
    match (record, cell) {
        (Some(record), Some((stake, body))) => record == stake && body.matches_record(&record),
        (None, None) => true,
        _ => false,
    }
}

fn verify_sequence(policy: &Policy<'_>, old: &Registry, new: &Registry) -> Result<()> {
    policy.validate()?;
    if old.network_id != policy.network_id
        || new.network_id != policy.network_id
        || old.sequence.checked_add(1) != Some(new.sequence)
    {
        return Err(Error::Sequence);
    }
    Ok(())
}

pub fn verify_rotation(
    policy: &Policy<'_>,
    old: &Registry,
    new: &Registry,
    departing: &GuardianProof<'_>,
    arriving: &GuardianProof<'_>,
) -> Result<()> {
    verify_sequence(policy, old, new)?;
    if old.records != new.records
        || old.retired_root != new.retired_root
        || old.guardian_generation == new.guardian_generation
    {
        return Err(Error::Authority);
    }
    let message = transition_digest(b"Stem CKB Stake authority rotation\0", policy, old, new);
    departing.verify(old.guardian_generation, &message)?;
    arriving.verify(new.guardian_generation, &message)
}

pub fn transition_digest(
    domain: &[u8],
    policy: &Policy<'_>,
    old: &Registry,
    new: &Registry,
) -> Hash32 {
    digest(
        domain,
        &[
            &policy.network_id,
            &policy.ckb_genesis,
            &policy.registry_id,
            &old.commitment(),
            &new.commitment(),
        ],
    )
}
