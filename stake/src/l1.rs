//! State transitions enforced by the CKB Stake Registry contract.

use alloc::vec::Vec;

use ed25519_dalek::Signature;

use crate::{
    Attestation, Authority, DoubleVote, Error, Hash32, Liability, Member, Registry, Result,
    StakeData, ValidatorKey, delegation::PrincipalChange, digest, finality::CheckpointId,
    patricia::decode_path, public_key, retirement,
};

const EMPTY: Hash32 = [0; 32];
pub const RELATIVE_BLOCKS: u64 = 1 << 63;
pub const MAX_DELAY: u64 = (1 << 56) - 1;
pub const BOND_DELEGATION: u8 = 9;
pub const WITHDRAW_DELEGATION: u8 = 10;
pub const SLASH_DELEGATION: u8 = 11;
pub const REDUCE_DELEGATION: u8 = 12;
pub const CONFIRM_REDUCTION: u8 = 13;
pub const WITHDRAW_REDUCTION: u8 = 14;
pub const SLASH_REDUCTION: u8 = 15;
pub const CONFIRM_DELEGATION: u8 = 16;

/// A delegation uses a native Type ID bound to its own CKB creation input and output index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DelegationId {
    pub registry: Hash32,
    pub validator: ValidatorKey,
    pub owner: Hash32,
    pub nonce: Hash32,
}

impl DelegationId {
    pub const SIZE: usize = 4 * 32;

    pub fn decode(args: &[u8]) -> Result<Self> {
        if args.len() != Self::SIZE {
            return Err(Error::Encoding);
        }
        let id = Self {
            registry: args[..32].try_into().unwrap(),
            validator: args[32..64].try_into().unwrap(),
            owner: args[64..96].try_into().unwrap(),
            nonce: args[96..].try_into().unwrap(),
        };
        if [id.registry, id.validator, id.owner, id.nonce].contains(&EMPTY) {
            return Err(Error::Identity);
        }
        Ok(id)
    }

    pub fn encode(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        for (field, value) in
            bytes
                .chunks_exact_mut(32)
                .zip([self.registry, self.validator, self.owner, self.nonce])
        {
            field.copy_from_slice(&value);
        }
        bytes
    }

    pub fn commitment(self) -> Hash32 {
        digest(b"Stem CKB delegation identity\0", &[&self.encode()])
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DelegationBond {
    pub active: u64,
    pub pending: u64,
    pub requested_sequence: u64,
    pub cutoff_height: u64,
    pub creation_sequence: u64,
    pub first_active_height: u64,
}

impl DelegationBond {
    pub const SIZE: usize = 48;

    pub fn decode(data: &[u8]) -> Result<Self> {
        let bytes: &[u8; Self::SIZE] = data.try_into().map_err(|_| Error::Encoding)?;
        let bond = Self {
            active: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            pending: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            requested_sequence: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
            cutoff_height: u64::from_le_bytes(bytes[24..32].try_into().unwrap()),
            creation_sequence: u64::from_le_bytes(bytes[32..40].try_into().unwrap()),
            first_active_height: u64::from_le_bytes(bytes[40..48].try_into().unwrap()),
        };
        if bond.total()? == 0
            || (bond.pending != 0) != (bond.requested_sequence != 0)
            || (bond.pending == 0 && bond.cutoff_height != 0)
            || (bond.creation_sequence == 0) == (bond.first_active_height == 0)
            || (bond.pending != 0 && bond.first_active_height == 0)
            || (bond.cutoff_height != 0 && bond.cutoff_height < bond.first_active_height)
        {
            return Err(Error::Encoding);
        }
        Ok(bond)
    }

    pub fn encode(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        for (field, value) in bytes.chunks_exact_mut(8).zip([
            self.active,
            self.pending,
            self.requested_sequence,
            self.cutoff_height,
            self.creation_sequence,
            self.first_active_height,
        ]) {
            field.copy_from_slice(&value.to_le_bytes());
        }
        bytes
    }

    pub fn total(self) -> Result<u64> {
        self.active.checked_add(self.pending).ok_or(Error::Overflow)
    }
}

pub const APPLIED_REGISTRY_SEQUENCE: &[u8] = b"stake/applied-registry-sequence";

pub fn registry_application_key(registry: &Hash32, sequence: u64) -> Vec<u8> {
    [
        b"stake/registry-application/".as_slice(),
        registry,
        &sequence.to_le_bytes(),
    ]
    .concat()
}

#[derive(Clone, Copy)]
pub struct RegistryApplication {
    pub previous_sequence: u64,
    pub effective_height: u64,
    pub l1_anchor: crate::Anchor,
}

impl RegistryApplication {
    pub const SIZE: usize = 56;

    pub fn encode(self) -> [u8; Self::SIZE] {
        let mut bytes = [0; Self::SIZE];
        bytes[..8].copy_from_slice(&self.previous_sequence.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.effective_height.to_le_bytes());
        bytes[16..24].copy_from_slice(&self.l1_anchor.height.to_le_bytes());
        bytes[24..].copy_from_slice(&self.l1_anchor.hash);
        bytes
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let bytes: &[u8; Self::SIZE] = bytes.try_into().map_err(|_| Error::Encoding)?;
        Ok(Self {
            previous_sequence: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            effective_height: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            l1_anchor: crate::Anchor {
                height: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
                hash: bytes[24..].try_into().unwrap(),
            },
        })
    }

    pub fn verify(
        self,
        policy: &Policy<'_>,
        checkpoint: CheckpointState,
        applied_sequence: u64,
        requested_sequence: u64,
        path: &[u8],
    ) -> Result<()> {
        use crate::patricia::{Commitment, Namespace};
        if requested_sequence <= self.previous_sequence
            || requested_sequence > applied_sequence
            || self.effective_height == 0
            || self.effective_height > checkpoint.height
            || self.l1_anchor.hash == EMPTY
        {
            return Err(Error::Anchor);
        }
        let commitment = Commitment::new(policy.network_id, Namespace::System);
        let key = registry_application_key(&policy.registry_id, applied_sequence);
        if !commitment.verify(
            &checkpoint.system_root,
            &commitment.record_key(&key),
            Some(&commitment.value_hash(&self.encode())),
            decode_path(path)?.into_iter(),
        ) {
            return Err(Error::Anchor);
        }
        Ok(())
    }
}

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
            && record.frozen() == (self.penalty != EMPTY)
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
        let (old_record, new_record) =
            verify_subject(self.old_registry, self.new_registry, &self.validator)?;
        let balances = |record: Option<StakeData>| {
            record.map_or((0, 0), |stake| {
                (stake.delegated_amount, stake.unbonding_amount)
            })
        };
        if old_record == new_record
            || balances(old_record) != balances(new_record)
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
                    || stake.frozen()
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
                    || before.delegation_root != after.delegation_root
                    || before.commission_bps != after.commission_bps
                    || before.fault_height != after.fault_height
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
                    || before.frozen()
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
                    || (before.frozen() && before.bonded_amount != 0)
                    || (self.since & !MAX_DELAY) != RELATIVE_BLOCKS
                    || (self.since & MAX_DELAY) < self.policy.unbonding_blocks
                    || new_record.is_some()
                {
                    return Err(Error::Sequence);
                }
                Ok(())
            }
            Action::Freeze(evidence) => self.freeze(|liability| {
                let hash =
                    evidence.verify(&self.policy.network_id, self.policy.chain_id, liability)?;
                Ok((
                    hash,
                    u64::try_from(evidence.first.height).map_err(|_| Error::Evidence)?,
                ))
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
                if !before.frozen()
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

    fn freeze(&self, verify: impl FnOnce(&Liability) -> Result<(Hash32, u64)>) -> Result<()> {
        let (before, old_cell) = self.old.ok_or(Error::Sequence)?;
        if before.bonded_amount == 0 || before.frozen() || old_cell.penalty != EMPTY {
            return Err(Error::Evidence);
        }
        let (penalty, fault_height) = verify(&Liability {
            validator: self.validator,
            first_active_height: old_cell.first_active_height,
            first_inactive_height: (old_cell.first_inactive_height != 0)
                .then_some(old_cell.first_inactive_height),
        })?;
        self.verify_cell_update(
            StakeData {
                fault_height,
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

pub enum DelegationAction {
    Bond {
        amount: u64,
        owner_authorized: bool,
    },
    Withdraw {
        amount: u64,
        start: u64,
        retired_block: u64,
        since: u64,
    },
    Slash {
        amount: u64,
        start: u64,
        fee: u64,
        historical_evidence: bool,
    },
    Reduce {
        amount: u64,
        owner_authorized: bool,
    },
    Confirm,
    WithdrawReduction {
        amount: u64,
        start: u64,
        cutoff: u64,
        since: u64,
    },
    SlashReduction {
        amount: u64,
        start: u64,
        cutoff: u64,
        fee: u64,
        historical_evidence: bool,
    },
}

pub fn verify_delegation(
    policy: &Policy<'_>,
    old: &Registry,
    new: &Registry,
    validator: &ValidatorKey,
    action: DelegationAction,
    principal: PrincipalChange<'_>,
) -> Result<()> {
    verify_sequence(policy, old, new)?;
    if old.guardian_generation != new.guardian_generation
        || old.retired_root != new.retired_root
        || principal.id.registry != policy.registry_id
        || &principal.id.validator != validator
    {
        return Err(Error::Sequence);
    }
    let (Some(before), Some(after)) = verify_subject(old, new, validator)? else {
        return Err(Error::Sequence);
    };
    let mut unbonding_amount = before.unbonding_amount;
    let delegated_amount = match action {
        DelegationAction::Bond {
            amount,
            owner_authorized,
        } => {
            if !owner_authorized
                || before.first_active_height == 0
                || before.first_inactive_height != 0
                || before.exit_epoch.is_some()
                || before.frozen()
                || before.bonded_amount < policy.minimum_bond
                || amount == 0
            {
                return Err(Error::Sequence);
            }
            before
                .delegated_amount
                .checked_add(amount)
                .ok_or(Error::Overflow)?
        }
        DelegationAction::Withdraw {
            amount,
            start,
            retired_block,
            since,
        } => {
            let minimum = retired_block
                .checked_add(policy.unbonding_blocks)
                .ok_or(Error::Overflow)?;
            if before.first_inactive_height == 0
                || start == 0
                || before.fault_height >= start
                || retired_block == 0
                || since & !MAX_DELAY != 0
                || since < minimum
                || amount == 0
            {
                return Err(Error::Sequence);
            }
            before
                .delegated_amount
                .checked_sub(amount)
                .ok_or(Error::Sequence)?
        }
        DelegationAction::Slash {
            amount,
            start,
            fee,
            historical_evidence,
        } => {
            if before.first_inactive_height == 0
                || amount == 0
                || fee < amount
                || start == 0
                || !(historical_evidence || before.fault_height >= start)
            {
                return Err(Error::Evidence);
            }
            before
                .delegated_amount
                .checked_sub(amount)
                .ok_or(Error::Sequence)?
        }
        DelegationAction::Reduce {
            amount,
            owner_authorized,
        } => {
            if !owner_authorized
                || amount == 0
                || before.first_active_height == 0
                || before.first_inactive_height != 0
                || before.frozen()
            {
                return Err(Error::Sequence);
            }
            unbonding_amount = unbonding_amount
                .checked_add(amount)
                .ok_or(Error::Overflow)?;
            before
                .delegated_amount
                .checked_sub(amount)
                .ok_or(Error::Sequence)?
        }
        DelegationAction::Confirm => before.delegated_amount,
        DelegationAction::WithdrawReduction {
            amount,
            start,
            cutoff,
            since,
        } => {
            if amount == 0
                || cutoff == 0
                || start == 0
                || cutoff < start
                || (before.fault_height >= start && before.fault_height < cutoff)
                || since & !MAX_DELAY != RELATIVE_BLOCKS
                || since & MAX_DELAY < policy.unbonding_blocks
            {
                return Err(Error::Sequence);
            }
            unbonding_amount = unbonding_amount
                .checked_sub(amount)
                .ok_or(Error::Sequence)?;
            before.delegated_amount
        }
        DelegationAction::SlashReduction {
            amount,
            start,
            cutoff,
            fee,
            historical_evidence,
        } => {
            if amount == 0
                || cutoff == 0
                || fee < amount
                || start == 0
                || cutoff < start
                || !(historical_evidence
                    || (before.fault_height >= start && before.fault_height < cutoff))
            {
                return Err(Error::Evidence);
            }
            unbonding_amount = unbonding_amount
                .checked_sub(amount)
                .ok_or(Error::Sequence)?;
            before.delegated_amount
        }
    };
    let expected_delegated = before
        .delegated_amount
        .checked_sub(principal.before)
        .and_then(|total| total.checked_add(principal.after))
        .ok_or(Error::Sequence)?;
    if expected_delegated != delegated_amount {
        return Err(Error::Sequence);
    }
    let delegation_root = principal.apply(&policy.network_id, &before.delegation_root)?;
    if after
        != (StakeData {
            delegated_amount,
            unbonding_amount,
            delegation_root,
            ..before
        })
    {
        return Err(Error::Sequence);
    }
    Ok(())
}

fn verify_subject(
    old: &Registry,
    new: &Registry,
    validator: &ValidatorKey,
) -> Result<(Option<StakeData>, Option<StakeData>)> {
    let before = old.record(validator);
    let after = new.record(validator);
    if !old
        .records
        .iter()
        .filter(|stake| &stake.validator_pubkey != validator)
        .eq(new
            .records
            .iter()
            .filter(|stake| &stake.validator_pubkey != validator))
    {
        return Err(Error::Sequence);
    }
    Ok((before, after))
}

pub fn retirement_cell(
    policy: &Policy<'_>,
    record: &StakeData,
    cell: StakeCell,
    inactive: i64,
) -> Result<StakeCell> {
    let earliest = if record.frozen() {
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
