use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_types::{packed::Byte32, prelude::*};
use fern_ledger::{
    Transaction,
    block::Receipt,
    protocol::{BlockBudget, Limits, Operation, SnapshotContext, snapshot_body},
    replay::Key as ReplayKey,
};
use fern_stake::{
    AcceptedSnapshot, Anchor, Attestation, Authority, Member, Registry, RegistryAnchor,
    StakeConfig, StakeDomain, StakeSnapshot, ValidatorKey, l1::guardian_generation,
};

use super::Store;

pub(super) const ACCEPTED: &[u8] = b"stake/accepted";
pub(super) const ANNOUNCED: &[u8] = b"stake/announced";
pub(super) const SNAPSHOT: &[u8] = b"stake/snapshot";
pub(super) const CONFIG: &[u8] = b"stake/config";
const AUTHORITY: &[u8] = b"stake/authority";

/// Network parameters and the initial L1 Registry observation authenticated by guardians.
#[derive(Clone)]
pub struct StakeGenesis {
    pub config: StakeConfig,
    pub signing_window: u64,
    pub limits: Limits,
    pub registry: Registry,
    pub statement: StakeSnapshot,
    pub guardians: Vec<Member>,
    pub signatures: Vec<Attestation>,
}

pub(super) struct Staking {
    config: StakeConfig,
    window: u64,
    limits: Limits,
    domain: StakeDomain,
    authority: Authority,
}

pub(super) fn retirement_key(
    network: &Byte32,
    registry: [u8; 32],
    validator: ValidatorKey,
) -> Byte32 {
    ReplayKey::Retirement {
        registry: registry.pack(),
        validator: validator.pack(),
    }
    .commitment(network)
}

impl Staking {
    pub(super) fn retirement_key(&self, network: &Byte32, validator: ValidatorKey) -> Byte32 {
        retirement_key(network, self.domain.registry_id, validator)
    }

    pub(super) fn window(&self) -> u64 {
        self.window
    }

    pub(super) fn initialize(
        genesis: &StakeGenesis,
        network: [u8; 32],
    ) -> Result<(Self, StakeState, Vec<u8>, Vec<u8>)> {
        ensure!(
            genesis.statement.domain.fern_network_id == network,
            "genesis Stake Registry is for a different network"
        );
        ensure!(
            (3..genesis.config.blocks_per_epoch).contains(&genesis.signing_window),
            "invalid Stake Snapshot signing window"
        );
        let authority = Authority::new(genesis.statement.guardian_generation, &genesis.guardians)?;
        let domain = genesis.statement.domain;
        let (accepted, members) = genesis.statement.verify(
            &domain,
            None,
            &genesis.registry,
            &authority,
            &genesis.signatures,
            &genesis.config,
        )?;
        let state = StakeState {
            accepted,
            eligible: members.clone(),
            announced: members,
            submitted: None,
        };
        let mut config = Vec::new();
        for item in [
            genesis.config.minimum_bond,
            genesis.config.shannons_per_power,
            u64::try_from(genesis.config.max_validators)?,
            genesis.config.max_total_power,
            genesis.config.blocks_per_epoch,
            genesis.signing_window,
            genesis.limits.bytes,
            u64::from(genesis.limits.operations),
            u64::from(genesis.limits.signatures),
        ] {
            config.extend_from_slice(&item.to_le_bytes());
        }
        config.extend_from_slice(&encode_members(&genesis.guardians));
        let mut snapshot = snapshot_body(&genesis.statement, &genesis.registry, &[])?.to_vec();
        snapshot.extend_from_slice(&u16::try_from(genesis.signatures.len())?.to_le_bytes());
        for attestation in &genesis.signatures {
            snapshot.extend_from_slice(&attestation.guardian.to_le_bytes());
            snapshot.extend_from_slice(&attestation.signature);
        }
        Ok((
            Self {
                config: genesis.config,
                window: genesis.signing_window,
                limits: genesis.limits,
                domain,
                authority,
            },
            state,
            config,
            snapshot,
        ))
    }

    pub(super) fn load(&self, store: &Store) -> Result<StagedStake> {
        let zero = Limits {
            operations: 0,
            bytes: 0,
            signatures: 0,
        };
        let accepted = store
            .system_record(ACCEPTED)?
            .context("missing accepted Stake Snapshot")?;
        let announced = store
            .system_record(ANNOUNCED)?
            .context("missing announced committee")?;
        let authority = match store.system_record(AUTHORITY)? {
            Some(encoded) => {
                let members = decode_members(&encoded)?;
                Authority::new(guardian_generation(&members), &members)?
            }
            None => self.authority.clone(),
        };
        Ok(StagedStake {
            state: StakeState::decode(&accepted, &announced)?,
            budget: BlockBudget::new(zero, self.limits, zero),
            authority,
            rotation: None,
            retirements: BTreeMap::new(),
        })
    }

    pub(super) fn verify(
        &self,
        store: &Store,
        height: u64,
        tx: &Transaction,
        staged: &mut StagedStake,
    ) -> Result<Receipt> {
        let Some(Operation::StakeSnapshot(statement)) = tx.operation() else {
            anyhow::bail!("unsupported protocol operation");
        };
        let anchor_height = self
            .config
            .effective_height(statement.epoch)?
            .checked_sub(self.window + 1)
            .context("invalid signing anchor height")?;
        ensure!(
            anchor_height < height,
            "Stake Snapshot signing anchor is in the future"
        );
        let header = store
            .header(anchor_height)?
            .context("unknown Stake Snapshot signing Header")?;
        let anchor = header.id(store.network());
        let context = SnapshotContext {
            height,
            window: self.window,
            signing_anchor_height: anchor_height,
            signing_anchor: &anchor,
            domain: &self.domain,
            previous: staged.state.accepted,
            authority: &staged.authority,
            config: &self.config,
        };
        let mut budget = staged.budget.clone();
        let verified = context.verify(tx, &mut budget)?;
        for member in &verified.eligible {
            let id = self.retirement_key(store.network(), member.key);
            ensure!(
                !staged.retirements.contains_key(&id)
                    && store.system_record(id.as_slice())?.is_none(),
                "Stake Snapshot would reactivate a retired validator"
            );
        }
        let effective = self.config.effective_height(statement.epoch)?;
        let mut retirements = staged.retirements.clone();
        for key in verified.retired {
            let id = self.retirement_key(store.network(), key);
            if store.system_record(id.as_slice())?.is_none() {
                retirements.entry(id).or_insert(effective);
            }
        }
        ensure!(
            retirements.len() <= fern_stake::MAX_RECORDS,
            "block retirement record limit exceeded"
        );
        if let Some(committee) = verified.rotation {
            staged.authority = Authority::new(guardian_generation(&committee), &committee)?;
            staged.rotation = Some(committee);
        }
        staged.budget = budget;
        staged.retirements = retirements;
        staged.state.accepted = verified.accepted;
        staged.state.eligible = verified.eligible;
        staged.state.submitted = Some(tx.clone());
        let mut result = statement.epoch.to_le_bytes().to_vec();
        result.extend_from_slice(statement.commitment.as_slice());
        Ok(Receipt {
            fee: 0,
            cycles: 0,
            protocol_result: result.into(),
        })
    }

    pub(super) fn finish(
        &self,
        staged: StagedStake,
        height: u64,
    ) -> Result<(Vec<(Vec<u8>, Vec<u8>)>, Vec<Member>)> {
        let StakeState {
            accepted,
            eligible,
            announced,
            submitted,
        } = staged.state;
        let mut writes = staged
            .retirements
            .into_iter()
            .map(|(key, height)| (key.as_slice().to_vec(), height.to_le_bytes().to_vec()))
            .collect::<Vec<_>>();
        if let Some(rotation) = staged.rotation {
            writes.push((AUTHORITY.to_vec(), encode_members(&rotation)));
        }
        if let Some(tx) = submitted {
            writes.push((SNAPSHOT.to_vec(), tx.as_slice().to_vec()));
            writes.push((ACCEPTED.to_vec(), encode_accepted(accepted, &eligible)));
        }
        if accepted.epoch == 0 || self.config.update_height(accepted.epoch)? != height {
            return Ok((writes, vec![]));
        }

        let prior = announced
            .iter()
            .map(|member| (member.key, member.power))
            .collect::<std::collections::BTreeMap<_, _>>();
        let next = eligible
            .iter()
            .map(|member| (member.key, member.power))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut updates = Vec::new();
        for (key, power) in &next {
            if prior.get(key) != Some(power) {
                updates.push(Member {
                    key: *key,
                    power: *power,
                });
            }
        }
        for key in prior.keys() {
            if !next.contains_key(key) {
                updates.push(Member {
                    key: *key,
                    power: 0,
                });
            }
        }
        updates.sort_unstable_by_key(|member| member.key);
        writes.push((ANNOUNCED.to_vec(), encode_members(&eligible)));
        Ok((writes, updates))
    }
}

pub(super) struct StagedStake {
    state: StakeState,
    budget: BlockBudget,
    authority: Authority,
    rotation: Option<Vec<Member>>,
    retirements: BTreeMap<Byte32, u64>,
}

pub(super) struct StakeState {
    accepted: AcceptedSnapshot,
    eligible: Vec<Member>,
    pub(super) announced: Vec<Member>,
    submitted: Option<Transaction>,
}

impl StakeState {
    pub(super) fn genesis_records(&self) -> [(&'static [u8], Vec<u8>); 2] {
        [
            (ACCEPTED, encode_accepted(self.accepted, &self.eligible)),
            (ANNOUNCED, encode_members(&self.announced)),
        ]
    }

    fn decode(accepted: &[u8], announced: &[u8]) -> Result<Self> {
        let (header, eligible) = accepted
            .split_at_checked(88)
            .context("truncated accepted Stake Snapshot")?;
        Ok(Self {
            accepted: AcceptedSnapshot {
                epoch: u64::from_le_bytes(header[..8].try_into()?),
                anchor: RegistryAnchor {
                    block: Anchor {
                        height: u64::from_le_bytes(header[8..16].try_into()?),
                        hash: header[16..48].try_into()?,
                    },
                    sequence: u64::from_le_bytes(header[48..56].try_into()?),
                    commitment: header[56..88].try_into()?,
                },
            },
            eligible: decode_members(eligible)?,
            announced: decode_members(announced)?,
            submitted: None,
        })
    }
}

fn encode_accepted(accepted: AcceptedSnapshot, members: &[Member]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&accepted.epoch.to_le_bytes());
    bytes.extend_from_slice(&accepted.anchor.block.height.to_le_bytes());
    bytes.extend_from_slice(&accepted.anchor.block.hash);
    bytes.extend_from_slice(&accepted.anchor.sequence.to_le_bytes());
    bytes.extend_from_slice(&accepted.anchor.commitment);
    bytes.extend_from_slice(&encode_members(members));
    bytes
}

fn encode_members(members: &[Member]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(members.len() * 40);
    for member in members {
        bytes.extend_from_slice(&member.key);
        bytes.extend_from_slice(&member.power.to_le_bytes());
    }
    bytes
}

fn decode_members(bytes: &[u8]) -> Result<Vec<Member>> {
    ensure!(
        bytes.len().is_multiple_of(40) && bytes.len() / 40 <= fern_stake::MAX_RECORDS,
        "invalid committee record"
    );
    bytes
        .chunks_exact(40)
        .map(|entry| {
            Ok(Member {
                key: entry[..32].try_into()?,
                power: u64::from_le_bytes(entry[32..].try_into()?),
            })
        })
        .collect()
}
