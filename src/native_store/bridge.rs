use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use ckb_types::{
    bytes::Bytes,
    core::{Capacity, ScriptHashType},
    packed::{Byte32, CellOutput, OutPoint, Script},
    prelude::*,
};
use stem_ledger::{
    Transaction,
    protocol::{
        BlockBudget, BridgeBacking, BridgeLedger, FeeClaim, FeeFund, FeeFundDestination,
        FeeFundPolicy, FeePayouts, L1Anchor, L1AnchorUpdate, Lane, Limits, PendingWithdrawal,
        RewardClaim, SettlementChanges, VaultRevenue, VaultRevenueEntry, VerifiedMint,
        l1_reference::{L1ReferenceVerifier, VerifiedL1References},
        reward_balance_key, reward_claim_key,
    },
    replay,
};
use stem_stake::{
    Authority, Member, Registry,
    finality::{CheckpointId, VaultPolicy},
    jolteon::{CommitteeChange, checkpoint::FundingPolicy},
    rewards::epoch_key,
};

use super::Store;

pub(super) mod intercell;

const CONFIG: &[u8] = b"bridge/config";
const ANCHOR: &[u8] = b"bridge/anchor";
const BACKING: &[u8] = b"bridge/backing";
const DEPOSITS: &[u8] = b"bridge/replay-count";
const LANE: &[u8] = b"bridge/lane/";
const DEPOSIT: &[u8] = b"bridge/deposit/";
const FUND_POLICY: &[u8] = b"fees/fund-policy";
const FUND_PAYOUTS: &[u8] = b"fees/fund-payouts";
const FUND_PENDING: &[u8] = b"fees/fund-pending/";

pub struct BridgeGenesis {
    pub l1_genesis: Byte32,
    pub confirmations: u64,
    pub anchor: L1Anchor,
    pub guardian_generation: Byte32,
    pub guardians: Vec<Member>,
    pub wrapped_code_hash: Byte32,
    pub burn_code_hash: Byte32,
    pub lanes: Vec<LaneGenesis>,
    pub recipient_locks: Vec<RecipientLock>,
    pub reserved_recipient_locks: Vec<Script>,
    pub mint_limits: Limits,
    pub settlement_limits: Limits,
    pub anchor_limits: Limits,
    pub deposits_per_block: u32,
    pub fee_funds: Option<[FeeFundDestination; 4]>,
    pub intercell_mailbox_code: Option<Byte32>,
    pub checkpoint_fee_fund_code: Option<Byte32>,
}

pub struct LaneGenesis {
    pub vault_type: Script,
    pub vault: OutPoint,
    pub l1_type: Option<Script>,
}

impl LaneGenesis {
    pub fn id(&self, genesis: &Byte32) -> Byte32 {
        let asset = self.l1_type.as_ref().map(Script::calc_script_hash);
        lane_id(genesis, &self.vault_type.calc_script_hash(), asset.as_ref())
    }
}

pub struct RecipientLock {
    pub code_hash: Byte32,
    pub hash_type: ScriptHashType,
    pub args_len: u32,
}

pub(super) struct Bridge {
    l1_genesis: Byte32,
    reference_verifier: L1ReferenceVerifier,
    burn: Byte32,
    lanes: BTreeMap<Byte32, Asset>,
    recipient_locks: Vec<(Byte32, u8, u32)>,
    reserved_locks: Vec<Script>,
    mint_limits: Limits,
    settlement_limits: Limits,
    anchor_limits: Limits,
    deposits_per_block: u32,
    fee_funds: Option<FeeFundPolicy>,
    intercell_mailbox: Option<(Byte32, CheckpointId)>,
}

struct Asset {
    l1: Option<Script>,
    stem: Option<Script>,
}

pub(super) type Records = BTreeMap<Vec<u8>, Option<Vec<u8>>>;

pub(super) struct StagedBridge {
    pub records: Records,
    budget: BlockBudget,
    deposits_remaining: u32,
    claimed_funds: u8,
}

#[derive(Default)]
pub(super) struct BridgeChanges {
    pub records: Records,
    pub omit_outputs: BTreeSet<usize>,
    pub protocol_result: Bytes,
    budget: Option<BlockBudget>,
    deposits_remaining: Option<u32>,
    claimed_funds: u8,
}

impl StagedBridge {
    pub fn apply(&mut self, change: BridgeChanges) {
        self.records.extend(change.records);
        if let Some(budget) = change.budget {
            self.budget = budget;
        }
        if let Some(remaining) = change.deposits_remaining {
            self.deposits_remaining = remaining;
        }
        self.claimed_funds |= change.claimed_funds;
    }

    pub fn finish(mut self, store: &Store, fees: u64, total_fees: Option<u64>) -> Result<Records> {
        if fees > 0 || self.records.contains_key(BACKING) {
            let view = View {
                store,
                staged: &self.records,
            };
            let backing = view.backing()?.extinguish_fee(fees)?;
            let total = match total_fees {
                Some(value) => value,
                None => store.fee_surplus()?,
            };
            let expected = if store.fee_fund_policy().is_some() {
                total
                    .checked_sub(view.fee_payouts()?.total_queued()?)
                    .and_then(|remaining| remaining.checked_sub(backing.accrued_stake_rewards()))
                    .context("fund claims exceed total fees")?
            } else {
                total
            };
            ensure!(
                backing.unallocated() == expected,
                "bridge backing differs from fee accounting"
            );
            if fees != 0 {
                let parent = View {
                    store,
                    staged: &Records::new(),
                };
                let opening = if self.records.contains_key(BACKING) {
                    parent.backing()?.liquid()
                } else {
                    backing.liquid()
                };
                // A first mint can fund a later transaction in the same block.
                let (positions, liquid) = if opening == 0 {
                    (&view, backing.liquid())
                } else {
                    (&parent, opening)
                };
                let revenue = VaultRevenue::decode(
                    &parent
                        .read(VaultRevenue::KEY)?
                        .context("missing vault revenue record")?,
                )?;
                let weights = revenue
                    .entries()
                    .iter()
                    .map(|entry| Ok(positions.lane_state(&entry.lane)?.0.capacity))
                    .collect::<Result<Vec<_>>>()?;
                ensure!(
                    weights
                        .iter()
                        .try_fold(0u64, |sum, weight| sum.checked_add(*weight))
                        == Some(liquid),
                    "vault reserves differ from aggregate backing"
                );
                let revenue = revenue.allocate(fees, &weights)?;
                ensure!(
                    revenue.total()? == total,
                    "per-vault revenue differs from extinguished fees"
                );
                self.records
                    .insert(VaultRevenue::KEY.to_vec(), Some(revenue.encode().to_vec()));
            }
            self.records
                .insert(BACKING.to_vec(), Some(backing.encode().to_vec()));
        }
        Ok(self.records)
    }
}

impl Bridge {
    pub fn initialize(spec: &BridgeGenesis, network: &Byte32) -> Result<(Self, Records)> {
        ensure!(
            spec.confirmations > 0
                && spec.l1_genesis != Byte32::default()
                && spec.anchor.hash != Byte32::default(),
            "missing L1 bridge identity"
        );
        ensure!(
            spec.burn_code_hash != Byte32::default()
                && spec.wrapped_code_hash != Byte32::default()
                && spec.burn_code_hash != spec.wrapped_code_hash,
            "invalid Stem bridge Script hashes"
        );
        ensure!(
            !spec.lanes.is_empty() && !spec.recipient_locks.is_empty(),
            "missing bridge lanes or L1 recipient templates"
        );
        let authority = Authority::new(spec.guardian_generation.unpack(), &spec.guardians)?;
        let mut records = Records::new();
        records.insert(ANCHOR.to_vec(), Some(spec.anchor.encode_adopted(0)));
        records.insert(
            BACKING.to_vec(),
            Some(BridgeBacking::default().encode().to_vec()),
        );
        records.insert(DEPOSITS.to_vec(), Some(0u64.to_le_bytes().to_vec()));

        let mut lanes = BTreeMap::new();
        let mut assets = BTreeSet::new();
        let mut vaults = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let mut lane_ids = Vec::with_capacity(spec.lanes.len());
        for lane in &spec.lanes {
            let vault_type_hash = lane.vault_type.calc_script_hash();
            ensure!(
                !lane.vault.is_null()
                    && vaults.insert(lane.vault.clone())
                    && identities.insert(vault_type_hash.clone()),
                "missing or duplicate vault"
            );
            let l1_hash = lane.l1_type.as_ref().map(Script::calc_script_hash);
            if let Some(l1) = &lane.l1_type {
                ScriptHashType::try_from(l1.hash_type())?;
            }
            ensure!(assets.insert(l1_hash.clone()), "duplicate L1 asset");
            let name = lane_id(&spec.l1_genesis, &vault_type_hash, l1_hash.as_ref());
            lane_ids.push(name.clone());
            let wrapped = l1_hash.map(|id| {
                Script::new_builder()
                    .hash_type(ScriptHashType::Data)
                    .code_hash(spec.wrapped_code_hash.clone())
                    .args(id.as_bytes())
                    .build()
            });
            lanes.insert(
                name.clone(),
                Asset {
                    l1: lane.l1_type.clone(),
                    stem: wrapped,
                },
            );
            let lane = Lane {
                vault: lane.vault.clone(),
                next_nonce: 0,
                capacity: 0,
                units: 0,
                reserved_capacity: 0,
                reserved_units: 0,
            };
            records.insert(key(LANE, &name, None), Some(lane.encode(0).to_vec()));
        }

        let revenue = VaultRevenue::new(
            lanes
                .keys()
                .map(|lane| VaultRevenueEntry {
                    lane: lane.clone(),
                    attributed: 0,
                })
                .collect(),
        )?;
        records.insert(VaultRevenue::KEY.to_vec(), Some(revenue.encode().to_vec()));

        let recipient_locks: Vec<_> = spec.recipient_locks.iter()
            .map(|lock| (lock.code_hash.clone(), lock.hash_type as u8, lock.args_len))
            .collect();
        let intercell_mailbox = if let Some(code) = &spec.intercell_mailbox_code {
            ensure!(
                *code != Byte32::default() && network != &spec.l1_genesis,
                "invalid Intercell network policy"
            );
            let vault = spec
                .lanes
                .first()
                .context("Intercell requires a shared CKB checkpoint")?;
            let checkpoint = VaultPolicy::decode(&vault.vault_type.args().raw_data())?.checkpoint;
            Some((code.clone(), checkpoint))
        } else {
            None
        };
        if let Some(code) = &spec.checkpoint_fee_fund_code {
            ensure!(
                *code != Byte32::default()
                    && spec.fee_funds.is_some()
                    && !spec
                        .recipient_locks
                        .iter()
                        .any(|recipient| recipient.code_hash == *code),
                "restricted checkpoint fee fund cannot be a general withdrawal recipient"
            );
        }
        let config = stem_ledger::protocol::genesis::BridgeConfig {
            l1_genesis: &spec.l1_genesis,
            guardian_generation: &spec.guardian_generation,
            wrapped_code: &spec.wrapped_code_hash,
            burn_code: &spec.burn_code_hash,
            budgets: [spec.mint_limits, spec.settlement_limits, spec.anchor_limits],
            confirmations: spec.confirmations,
            deposits_per_block: spec.deposits_per_block,
            guardians: &spec.guardians,
            lanes: &lane_ids,
            recipients: &recipient_locks,
            reserved: &spec.reserved_recipient_locks,
            mailbox: spec.intercell_mailbox_code.as_ref(),
            checkpoint_fund: spec.checkpoint_fee_fund_code.as_ref(),
        }.encode()?;
        records.insert(CONFIG.to_vec(), Some(config));

        let mut bridge = Self {
            l1_genesis: spec.l1_genesis.clone(),
            reference_verifier: L1ReferenceVerifier::new(
                network.clone(),
                spec.l1_genesis.clone(),
                authority,
            ),
            burn: spec.burn_code_hash.clone(),
            lanes,
            recipient_locks,
            reserved_locks: spec.reserved_recipient_locks.clone(),
            mint_limits: spec.mint_limits,
            settlement_limits: spec.settlement_limits,
            anchor_limits: spec.anchor_limits,
            deposits_per_block: spec.deposits_per_block,
            fee_funds: None,
            intercell_mailbox,
        };
        if let Some(funds) = &spec.fee_funds {
            let (lane, _) = bridge
                .lanes
                .iter()
                .find(|(_, asset)| asset.l1.is_none())
                .context("fee funds require a native CKB vault")?;
            for (role, fund) in FeeFund::ALL.into_iter().zip(funds) {
                if let (FeeFund::ProofOfWork, Some(code)) = (role, &spec.checkpoint_fee_fund_code) {
                    bridge.check_recipient_shape(&fund.recipient)?;
                    let vault = spec
                        .lanes
                        .iter()
                        .find(|lane| lane.l1_type.is_none())
                        .context("checkpoint fee fund requires a native CKB vault")?;
                    let checkpoint =
                        VaultPolicy::decode(&vault.vault_type.args().raw_data())?.checkpoint;
                    let purpose = FundingPolicy::decode(&fund.recipient.args().raw_data())?;
                    ensure!(
                        fund.recipient.code_hash() == *code
                            && ScriptHashType::try_from(fund.recipient.hash_type())?
                                == ScriptHashType::Data2
                            && purpose.checkpoint_code == checkpoint.code_hash
                            && purpose.checkpoint_type_id == checkpoint.type_id
                            && purpose.ckb_genesis.as_slice() == spec.l1_genesis.as_slice()
                            && purpose.network.as_slice() == network.as_slice(),
                        "PoW Security Fund must target the native vault checkpoint on this network"
                    );
                } else {
                    bridge.check_recipient(&fund.recipient)?;
                }
            }
            let policy = FeeFundPolicy {
                lane: lane.clone(),
                funds: funds.clone(),
            };
            policy.validate()?;
            records.insert(FUND_POLICY.to_vec(), Some(policy.encode().to_vec()));
            records.insert(
                FUND_PAYOUTS.to_vec(),
                Some(FeePayouts::default().encode().to_vec()),
            );
            bridge.fee_funds = Some(policy);
        }
        Ok((bridge, records))
    }

    pub fn stage(&self) -> StagedBridge {
        let zero = Limits {
            operations: 0,
            bytes: 0,
            signatures: 0,
        };
        StagedBridge {
            records: Records::new(),
            budget: BlockBudget::new(
                self.mint_limits,
                zero,
                self.settlement_limits,
                self.anchor_limits,
            ),
            deposits_remaining: self.deposits_per_block,
            claimed_funds: 0,
        }
    }

    pub fn mint(
        &self,
        store: &Store,
        state: &StagedBridge,
        height: u64,
        tx: &Transaction,
    ) -> Result<(VerifiedMint, Option<Script>, BridgeChanges)> {
        let view = View {
            store,
            staged: &state.records,
        };
        let mut budget = state.budget.clone();
        let mint = VerifiedMint::verify(
            tx,
            store.network(),
            &self.l1_genesis,
            &view.anchor()?,
            self.reference_verifier.authority(),
            &mut budget,
        )?;
        let observation = mint.observation();
        let (mut lane, next_request) = view.lane_state(&observation.vault.lane)?;
        ensure!(
            lane.vault == observation.vault.input,
            "mint names a spent or foreign vault"
        );
        let asset = self
            .lanes
            .get(&observation.vault.lane)
            .context("unknown mint lane")?;
        ensure!(
            asset.stem.is_some() == (observation.units > 0),
            "mint asset differs from vault lane"
        );
        let mut units = 0u128;
        for cell in tx.outputs() {
            ensure!(
                !self.is_burn(&cell.lock()),
                "mint cannot create a Burn Cell"
            );
            for script in cell.type_scripts() {
                if self
                    .lanes
                    .values()
                    .any(|other| other.stem.as_ref() == Some(&script))
                {
                    ensure!(
                        asset.stem.as_ref() == Some(&script),
                        "mint contains a foreign wrapped asset"
                    );
                    let amount = decode_units(&cell.data())?;
                    units = units.checked_add(amount).context("mint units overflow")?;
                }
            }
        }
        ensure!(
            units == observation.units,
            "mint differs from authenticated xUDT amount"
        );
        lane.vault = observation.vault.output.clone();
        lane.capacity = lane
            .capacity
            .checked_add(observation.capacity)
            .context("mint backing Capacity overflow")?;
        lane.units = lane
            .units
            .checked_add(observation.units)
            .context("mint backing units overflow")?;
        let mut changes = BridgeChanges {
            budget: Some(budget),
            ..Default::default()
        };
        let backing = view.backing()?.credit_mint(observation.capacity)?;
        changes
            .records
            .insert(BACKING.to_vec(), Some(backing.encode().to_vec()));
        changes.records.insert(
            key(LANE, &observation.vault.lane, None),
            Some(lane.encode(next_request).to_vec()),
        );
        changes.records.insert(
            ANCHOR.to_vec(),
            Some(observation.anchor.encode_adopted(height)),
        );

        let deposits = &mint.statement().deposits;
        let count = u32::try_from(deposits.len())?;
        changes.deposits_remaining = Some(
            state
                .deposits_remaining
                .checked_sub(count)
                .context("deposit replay insertion budget exceeded")?,
        );
        for deposit in deposits {
            let id = replay::Key::Deposit {
                l1_genesis: self.l1_genesis.clone(),
                deposit: deposit.clone(),
            }
            .commitment(store.network());
            let key = key(DEPOSIT, &id, None);
            ensure!(view.read(&key)?.is_none(), "deposit already credited");
            changes
                .records
                .insert(key, Some(height.to_le_bytes().to_vec()));
        }
        let total = u64::from_le_bytes(
            view.read(DEPOSITS)?
                .context("missing replay counter")?
                .as_slice()
                .try_into()?,
        )
        .checked_add(u64::from(count))
        .context("deposit replay count overflow")?;
        replay::logical_bytes(total)?;
        changes
            .records
            .insert(DEPOSITS.to_vec(), Some(total.to_le_bytes().to_vec()));
        changes.protocol_result = observation.anchor.hash.as_bytes();
        Ok((mint, asset.stem.clone(), changes))
    }

    pub fn burn(
        &self,
        store: &Store,
        state: &StagedBridge,
        tx: &Transaction,
    ) -> Result<BridgeChanges> {
        let tx_id = tx.id(store.network());
        let mut changes = BridgeChanges::default();
        let mut receipts = Vec::new();
        let mut withdrawal_capacity = 0u64;
        for (index, cell) in tx.outputs().enumerate() {
            let lock = cell.lock();
            if !self.is_burn(&lock) {
                continue;
            }
            let args = lock.args().raw_data();
            let (id, recipient) = args
                .split_at_checked(32)
                .context("Burn Lock does not identify a lane and recipient")?;
            let lane_id = Byte32::from_slice(id)?;
            let asset = self
                .lanes
                .get(&lane_id)
                .context("Burn Lock names an unknown lane")?;
            let recipient = Script::from_slice(recipient)?;
            self.check_recipient(&recipient)?;
            let mut units = 0u128;
            for script in cell.type_scripts() {
                if self
                    .lanes
                    .values()
                    .any(|asset| asset.stem.as_ref() == Some(&script))
                {
                    ensure!(
                        asset.stem.as_ref() == Some(&script),
                        "Burn Cell uses a foreign wrapped asset"
                    );
                    units = decode_units(&cell.data())?;
                }
            }
            ensure!(
                asset.stem.is_some() == (units > 0),
                "Burn Cell is missing required wrapped units"
            );
            let output = CellOutput::new_builder()
                .capacity(cell.capacity())
                .lock(recipient.clone())
                .type_(asset.l1.clone().pack())
                .build();
            let l1_data_bytes = if asset.l1.is_some() { 16 } else { 0 };
            ensure!(
                !output.is_lack_of_capacity(Capacity::bytes(l1_data_bytes)?)?,
                "Burn Cell is below L1 occupied Capacity"
            );

            let view = View {
                store,
                staged: &state.records,
            };
            let (_, receipt) = view.queue_withdrawal(
                &mut changes.records,
                Withdrawal {
                    lane: &lane_id,
                    tx_id: &tx_id,
                    index: u32::try_from(index)?,
                    capacity: cell.capacity(),
                    units,
                    recipient: &recipient,
                },
            )?;
            changes.omit_outputs.insert(index);
            withdrawal_capacity = withdrawal_capacity
                .checked_add(cell.capacity())
                .context("withdrawal capacity overflow")?;
            append_ticket(&mut receipts, &receipt)?;
        }
        if withdrawal_capacity > 0 {
            let backing = View {
                store,
                staged: &state.records,
            }
            .backing()?
            .reserve_withdrawal(withdrawal_capacity)?;
            changes
                .records
                .insert(BACKING.to_vec(), Some(backing.encode().to_vec()));
        }
        changes.protocol_result = receipts.into();
        Ok(changes)
    }

    pub fn claim_fee(
        &self,
        store: &Store,
        state: &StagedBridge,
        tx: &Transaction,
        claim: FeeClaim,
    ) -> Result<BridgeChanges> {
        ensure!(
            claim.fund != FeeFund::Validators || store.staking.is_none(),
            "the stake-backed validator fund requires individual reward proofs"
        );
        let policy = self.fee_funds.as_ref().context("fee funds are disabled")?;
        let view = fee_view(store, state, claim.fund)?;
        self.queue_fee_payout(
            &view,
            tx,
            claim,
            &policy.funds[claim.fund as usize].recipient,
            None,
        )
    }

    pub fn claim_reward(
        &self,
        store: &Store,
        state: &StagedBridge,
        tx: &Transaction,
        claim: RewardClaim,
    ) -> Result<BridgeChanges> {
        ensure!(store.staking.is_some(), "CKB staking is disabled");
        let policy = self.fee_funds.as_ref().context("fee funds are disabled")?;
        let fund = FeeFund::Validators;
        let view = fee_view(store, state, fund)?;
        self.check_recipient(&claim.recipient)?;
        let owner = claim.recipient.calc_script_hash();
        let balance_key = reward_balance_key(&owner.unpack());
        let previous_balance = view
            .read(&balance_key)?
            .as_deref()
            .map(decode_reward_balance)
            .transpose()?
            .unwrap_or(0);
        let network = store.network().unpack();
        let mut unclaimed = Vec::new();
        let mut total = 0u64;
        for epoch in &claim.epochs {
            let raw = store
                .system_record(&epoch_key(epoch.epoch))?
                .context("stake reward epoch is not closed")?;
            let record = stem_ledger::stake_proof::decode_epoch_rewards(&raw)?;
            let registry = Registry::decode(&epoch.registry)?;
            let committee = CommitteeChange::decode(&epoch.committee)?;
            let terms = record.bind(&network, &registry, &committee)?;
            for position in &epoch.positions {
                let (kind, id) = position.id();
                let key = reward_claim_key(epoch.epoch, kind, &id);
                ensure!(
                    view.read(&key)?.is_none(),
                    "stake reward position already claimed"
                );
                let quote = terms.quote(position.subject())?;
                ensure!(
                    quote.owner == owner.as_slice(),
                    "reward recipient does not own the position"
                );
                ensure!(quote.amount != 0, "stake reward position has no earnings");
                total = total
                    .checked_add(quote.amount)
                    .context("reward claim amount overflow")?;
                unclaimed.push((key, quote.amount));
            }
        }
        let balance = previous_balance
            .checked_add(total)
            .context("recipient stake reward balance overflow")?;
        let backing = view.backing()?.accrue_stake_rewards(total)?;
        let queued = view.fee_payouts()?.fund(fund).queued;
        let allocation = FeeFund::allocation(
            store.fee_surplus()?,
            policy.funds[fund as usize].basis_points,
        )?;
        ensure!(
            queued
                .checked_add(backing.accrued_stake_rewards())
                .is_some_and(|earned| earned <= allocation),
            "accrued rewards exceed fund allocation"
        );
        let mut changes = if claim.payout == 0 {
            let mut changes = BridgeChanges {
                claimed_funds: 1 << fund as u8,
                ..Default::default()
            };
            changes
                .records
                .insert(BACKING.to_vec(), Some(backing.encode().to_vec()));
            changes
                .records
                .insert(balance_key, Some(balance.to_le_bytes().to_vec()));
            changes
        } else {
            ensure!(
                claim.payout == balance,
                "reward payout differs from the authenticated balance"
            );
            let debit = FeeClaim {
                fund,
                amount: balance,
                claimed_before: queued,
            };
            let mut changes =
                self.queue_fee_payout(&view, tx, debit, &claim.recipient, Some(backing))?;
            if previous_balance != 0 {
                changes.records.insert(balance_key, None);
            }
            changes
        };
        let tx_id = tx.id(store.network());
        for (key, amount) in unclaimed {
            let mut record = tx_id.as_slice().to_vec();
            record.extend_from_slice(&amount.to_le_bytes());
            changes.records.insert(key, Some(record));
        }
        Ok(changes)
    }

    fn queue_fee_payout(
        &self,
        view: &View<'_>,
        tx: &Transaction,
        claim: FeeClaim,
        recipient: &Script,
        accrued: Option<BridgeBacking>,
    ) -> Result<BridgeChanges> {
        let policy = self.fee_funds.as_ref().context("fee funds are disabled")?;
        let destination = &policy.funds[claim.fund as usize];
        let output = CellOutput::new_builder()
            .capacity(claim.amount)
            .lock(recipient.clone())
            .build();
        ensure!(
            !output.is_lack_of_capacity(Capacity::zero())?,
            "fee claim is below L1 occupied Capacity"
        );
        let mut payouts = view.fee_payouts()?;
        let allocation = FeeFund::allocation(view.store.fee_surplus()?, destination.basis_points)?;
        payouts.queue(claim, allocation)?;
        let backing = match accrued {
            Some(backing) => backing.reserve_stake_payout(claim.amount)?,
            None => view.backing()?.reserve_fee_payout(claim.amount)?,
        };
        let mut changes = BridgeChanges {
            claimed_funds: 1 << claim.fund as u8,
            ..Default::default()
        };
        let (nonce, receipt) = view.queue_withdrawal(
            &mut changes.records,
            Withdrawal {
                lane: &policy.lane,
                tx_id: &tx.id(view.store.network()),
                index: 0,
                capacity: claim.amount,
                units: 0,
                recipient,
            },
        )?;
        changes
            .records
            .insert(BACKING.to_vec(), Some(backing.encode().to_vec()));
        changes
            .records
            .insert(FUND_PAYOUTS.to_vec(), Some(payouts.encode().to_vec()));
        changes.records.insert(
            key(FUND_PENDING, &policy.lane, Some(nonce)),
            Some(vec![claim.fund as u8]),
        );
        let mut ticket = Vec::new();
        append_ticket(&mut ticket, &receipt)?;
        changes.protocol_result = ticket.into();
        Ok(changes)
    }

    pub fn advance_anchor(
        &self,
        store: &Store,
        state: &StagedBridge,
        tx: &Transaction,
        update: L1AnchorUpdate,
        height: u64,
    ) -> Result<BridgeChanges> {
        let mut budget = state.budget.clone();
        budget.charge(tx)?;
        let view = View {
            store,
            staged: &state.records,
        };
        update.verify(
            store.network(),
            &self.l1_genesis,
            &view.anchor()?,
            self.reference_verifier.authority(),
            &tx.certificate().context("missing L1 anchor certificate")?,
        )?;
        let record = update.anchor.encode_adopted(height);
        Ok(BridgeChanges {
            budget: Some(budget),
            records: Records::from([(ANCHOR.to_vec(), Some(record))]),
            protocol_result: update.anchor.hash.as_bytes(),
            ..Default::default()
        })
    }

    pub fn settle(
        &self,
        store: &Store,
        state: &StagedBridge,
        tx: &Transaction,
        height: u64,
    ) -> Result<BridgeChanges> {
        let view = View {
            store,
            staged: &state.records,
        };
        let mut budget = state.budget.clone();
        let settlement = SettlementChanges::verify(
            tx,
            store.network(),
            &self.l1_genesis,
            &view.anchor()?,
            self.reference_verifier.authority(),
            &view,
            &mut budget,
        )?;
        let mut changes = BridgeChanges {
            budget: Some(budget),
            ..Default::default()
        };
        for (id, lane) in &settlement.lanes {
            let (_, next_request) = view.lane_state(id)?;
            changes.records.insert(
                key(LANE, id, None),
                Some(lane.encode(next_request).to_vec()),
            );
        }
        if let Some((lane, nonce)) = settlement.paid {
            let paid = view.pending(&lane, nonce)?;
            let backing = view.backing()?.settle_withdrawal(paid.capacity)?;
            changes
                .records
                .insert(BACKING.to_vec(), Some(backing.encode().to_vec()));
            changes
                .records
                .insert(pending_key(&lane, nonce).to_vec(), None);
            let fund_key = key(FUND_PENDING, &lane, Some(nonce));
            if self.fee_funds.is_some()
                && let Some(value) = view.read(&fund_key)?
            {
                let [id]: [u8; 1] = value.as_slice().try_into()?;
                let mut payouts = view.fee_payouts()?;
                payouts.settle(FeeFund::from_id(id)?, paid.capacity)?;
                changes
                    .records
                    .insert(FUND_PAYOUTS.to_vec(), Some(payouts.encode().to_vec()));
                changes.records.insert(fund_key, None);
            }
        }
        changes.records.insert(
            ANCHOR.to_vec(),
            Some(settlement.anchor.encode_adopted(height)),
        );
        changes.protocol_result = settlement.anchor.hash.as_bytes();
        Ok(changes)
    }

    fn is_burn(&self, lock: &Script) -> bool {
        lock.code_hash() == self.burn
    }

    fn check_recipient_shape(&self, recipient: &Script) -> Result<()> {
        ensure!(
            recipient.as_slice().len() <= stem_bridge_wire::MAX_RECIPIENT_SIZE,
            "L1 recipient Lock exceeds the withdrawal proof size limit"
        );
        ensure!(
            !self.reserved_locks.contains(recipient),
            "reserved L1 recipient"
        );
        Ok(())
    }

    fn check_recipient(&self, recipient: &Script) -> Result<()> {
        self.check_recipient_shape(recipient)?;
        let hash_type = recipient.hash_type().as_slice()[0];
        ensure!(
            self.recipient_locks.iter().any(|(code, kind, len)| {
                *code == recipient.code_hash()
                    && *kind == hash_type
                    && (*len as usize) == recipient.args().len()
            }),
            "unsupported L1 recipient Lock"
        );
        Ok(())
    }

    pub fn verify_l1_references(
        &self,
        store: &Store,
        tx: &Transaction,
        height: u64,
    ) -> Result<VerifiedL1References> {
        let (anchor, adopted_at) = L1Anchor::decode_adopted(
            &store
                .system_record(ANCHOR)?
                .context("missing L1 backing anchor")?,
        )?;
        let age = height
            .checked_sub(adopted_at)
            .context("L1 anchor is newer than the transaction")?;
        self.reference_verifier
            .verify(tx, &anchor, age <= store.limits.header_dep_age)
    }
}

impl Store {
    pub fn prove_bridge_anchor(&self) -> Result<((L1Anchor, u64), super::Proof)> {
        let (record, proof) = self.prove_system(ANCHOR)?;
        Ok((
            L1Anchor::decode_adopted(&record.context("bridge is not configured")?)?,
            proof,
        ))
    }

    pub fn verify_bridge_anchor(
        network: &Byte32,
        header: &super::Header,
        anchor: &L1Anchor,
        adopted_at: u64,
        proof: &super::Proof,
    ) -> bool {
        adopted_at <= header.context.height
            && Self::verify_system_record(
                network,
                header,
                ANCHOR,
                Some(&anchor.encode_adopted(adopted_at)),
                proof,
            )
    }

    pub fn prove_stake_reward_balance(&self, owner: &[u8; 32]) -> Result<(u64, super::Proof)> {
        let (record, proof) = self.prove_system(&reward_balance_key(owner))?;
        let balance = record
            .as_deref()
            .map(decode_reward_balance)
            .transpose()?
            .unwrap_or(0);
        Ok((balance, proof))
    }

    pub fn verify_stake_reward_balance(
        network: &Byte32,
        header: &super::Header,
        owner: &[u8; 32],
        balance: u64,
        proof: &super::Proof,
    ) -> bool {
        let value = (balance != 0).then(|| balance.to_le_bytes());
        Self::verify_system_record(
            network,
            header,
            &reward_balance_key(owner),
            value.as_ref().map(|bytes| bytes.as_slice()),
            proof,
        )
    }

    pub fn fee_fund_policy(&self) -> Option<&FeeFundPolicy> {
        self.bridge.as_ref()?.fee_funds.as_ref()
    }

    pub fn prove_fee_fund_policy(&self) -> Result<super::Proof> {
        Ok(self.prove_system(FUND_POLICY)?.1)
    }

    pub fn verify_fee_fund_policy(
        network: &Byte32,
        header: &super::Header,
        policy: &FeeFundPolicy,
        proof: &super::Proof,
    ) -> bool {
        Self::verify_system_record(network, header, FUND_POLICY, Some(&policy.encode()), proof)
    }

    pub fn prove_fee_payouts(&self) -> Result<(FeePayouts, super::Proof)> {
        let (record, proof) = self.prove_system(FUND_PAYOUTS)?;
        Ok((
            FeePayouts::decode(&record.context("fee funds are disabled")?)?,
            proof,
        ))
    }

    pub fn verify_fee_payouts(
        network: &Byte32,
        header: &super::Header,
        payouts: &FeePayouts,
        proof: &super::Proof,
    ) -> bool {
        Self::verify_system_record(
            network,
            header,
            FUND_PAYOUTS,
            Some(&payouts.encode()),
            proof,
        )
    }

    pub fn prove_bridge_backing(&self) -> Result<(BridgeBacking, super::Proof)> {
        let (record, proof) = self.prove_system(BACKING)?;
        let record = record.context("bridge backing is not configured")?;
        Ok((BridgeBacking::decode(&record)?, proof))
    }

    pub fn verify_bridge_backing(
        network: &Byte32,
        header: &super::Header,
        backing: &BridgeBacking,
        proof: &super::Proof,
    ) -> bool {
        Self::verify_system_record(network, header, BACKING, Some(&backing.encode()), proof)
    }

    pub fn prove_vault_revenue(&self) -> Result<(VaultRevenue, super::Proof)> {
        let (record, proof) = self.prove_system(VaultRevenue::KEY)?;
        let record = record.context("vault revenue is not configured")?;
        Ok((VaultRevenue::decode(&record)?, proof))
    }

    pub fn verify_vault_revenue(
        network: &Byte32,
        header: &super::Header,
        revenue: &VaultRevenue,
        proof: &super::Proof,
    ) -> bool {
        Self::verify_system_record(
            network,
            header,
            VaultRevenue::KEY,
            Some(&revenue.encode()),
            proof,
        )
    }

    pub fn bridge_lane(&self, lane: &Byte32) -> Result<Lane> {
        Ok(Lane::decode(
            &self
                .system_record(&key(LANE, lane, None))?
                .context("unknown bridge lane")?,
        )?
        .0)
    }

    pub fn prove_bridge_lane(&self, id: &Byte32) -> Result<((Lane, u64), super::Proof)> {
        let (record, proof) = self.prove_system(&key(LANE, id, None))?;
        let record = record.context("unknown bridge lane")?;
        Ok((Lane::decode(&record)?, proof))
    }

    pub fn verify_bridge_lane(
        network: &Byte32,
        header: &super::Header,
        id: &Byte32,
        record: &(Lane, u64),
        proof: &super::Proof,
    ) -> bool {
        let (lane, next_request) = record;
        Self::verify_system_record(
            network,
            header,
            &key(LANE, id, None),
            Some(&lane.encode(*next_request)),
            proof,
        )
    }

    pub fn prove_bridge_withdrawal(
        &self,
        lane: &Byte32,
        nonce: u64,
    ) -> Result<(Option<Vec<u8>>, super::Proof)> {
        self.prove_system(&pending_key(lane, nonce))
    }

    pub fn verify_bridge_withdrawal(
        network: &Byte32,
        header: &super::Header,
        lane: &Byte32,
        nonce: u64,
        record: &[u8],
        proof: &super::Proof,
    ) -> bool {
        stem_bridge_wire::Withdrawal::decode(&network.unpack(), record)
            .is_some_and(|claim| claim.lane == lane.as_slice() && claim.nonce == nonce)
            && Self::verify_system_record(
                network,
                header,
                &pending_key(lane, nonce),
                Some(record),
                proof,
            )
    }

    pub fn bridge_receipt(&self, lane: &Byte32, nonce: u64) -> Result<Option<Vec<u8>>> {
        self.system_record(&pending_key(lane, nonce))
    }
}

struct View<'a> {
    store: &'a Store,
    staged: &'a Records,
}

fn fee_view<'a>(store: &'a Store, state: &'a StagedBridge, fund: FeeFund) -> Result<View<'a>> {
    ensure!(
        state.claimed_funds & (1 << fund as u8) == 0,
        "fee fund already claimed in this block"
    );
    Ok(View {
        store,
        staged: &state.records,
    })
}

fn decode_reward_balance(raw: &[u8]) -> Result<u64> {
    let balance = u64::from_le_bytes(raw.try_into()?);
    ensure!(balance != 0, "empty stake reward balance record");
    Ok(balance)
}

struct Withdrawal<'a> {
    lane: &'a Byte32,
    tx_id: &'a Byte32,
    index: u32,
    capacity: u64,
    units: u128,
    recipient: &'a Script,
}

impl View<'_> {
    fn queue_withdrawal(
        &self,
        changes: &mut Records,
        withdrawal: Withdrawal<'_>,
    ) -> Result<(u64, Vec<u8>)> {
        let name = key(LANE, withdrawal.lane, None);
        let (mut lane, nonce) = match changes.get(&name).and_then(Option::as_ref) {
            Some(encoded) => Lane::decode(encoded)?,
            None => self.lane_state(withdrawal.lane)?,
        };
        lane.reserved_capacity = lane
            .reserved_capacity
            .checked_add(withdrawal.capacity)
            .context("pending withdrawal Capacity overflow")?;
        lane.reserved_units = lane
            .reserved_units
            .checked_add(withdrawal.units)
            .context("pending withdrawal units overflow")?;
        ensure!(
            lane.reserved_capacity <= lane.capacity && lane.reserved_units <= lane.units,
            "withdrawal exceeds available vault reserve"
        );
        let next = nonce.checked_add(1).context("withdrawal nonce exhausted")?;
        let receipt = stem_ledger::protocol::withdrawal_receipt(
            withdrawal.lane, nonce, withdrawal.tx_id, withdrawal.index, withdrawal.capacity,
            withdrawal.units, withdrawal.recipient,
        );
        let id = stem_bridge_wire::withdrawal_receipt_id(&self.store.network().unpack(), &receipt);
        let mut pending = id.to_vec();
        pending.extend_from_slice(&receipt);
        let pending_key = pending_key(withdrawal.lane, nonce).to_vec();
        ensure!(
            !changes.contains_key(&pending_key) && self.read(&pending_key)?.is_none(),
            "withdrawal nonce already pending"
        );
        changes.insert(pending_key, Some(pending));
        changes.insert(name, Some(lane.encode(next).to_vec()));
        Ok((nonce, receipt))
    }

    fn fee_payouts(&self) -> Result<FeePayouts> {
        FeePayouts::decode(
            &self
                .read(FUND_PAYOUTS)?
                .context("missing fee fund payout record")?,
        )
    }

    fn read(&self, key: &[u8]) -> Result<Option<Vec<u8>>> {
        match self.staged.get(key) {
            Some(value) => Ok(value.clone()),
            None => self.store.system_record(key),
        }
    }

    fn anchor(&self) -> Result<L1Anchor> {
        Ok(L1Anchor::decode_adopted(&self.read(ANCHOR)?.context("missing L1 backing anchor")?)?.0)
    }

    fn backing(&self) -> Result<BridgeBacking> {
        BridgeBacking::decode(&self.read(BACKING)?.context("missing bridge backing")?)
    }

    fn lane_state(&self, lane: &Byte32) -> Result<(Lane, u64)> {
        Lane::decode(
            &self
                .read(&key(LANE, lane, None))?
                .context("missing vault lane")?,
        )
    }
}

impl BridgeLedger for View<'_> {
    fn lane(&self, id: &Byte32) -> Result<Lane> {
        Ok(self.lane_state(id)?.0)
    }

    fn pending(&self, id: &Byte32, nonce: u64) -> Result<PendingWithdrawal> {
        let encoded = self
            .read(&pending_key(id, nonce))?
            .context("missing pending withdrawal")?;
        let withdrawal =
            stem_bridge_wire::Withdrawal::decode(&self.store.network().unpack(), &encoded)
                .context("invalid pending withdrawal")?;
        ensure!(
            withdrawal.lane == id.as_slice() && withdrawal.nonce == nonce,
            "wrong pending withdrawal identity"
        );
        Ok(PendingWithdrawal {
            receipt: withdrawal.id.pack(),
            capacity: withdrawal.capacity,
            units: withdrawal.units,
        })
    }
}

pub fn lane_id(genesis: &Byte32, vault_type: &Byte32, asset: Option<&Byte32>) -> Byte32 {
    stem_bridge_wire::lane_id(
        &genesis.unpack(),
        &vault_type.unpack(),
        asset.map(Unpack::unpack).as_ref(),
    )
    .pack()
}

fn pending_key(lane: &Byte32, nonce: u64) -> [u8; stem_bridge_wire::WITHDRAWAL_KEY_SIZE] {
    stem_bridge_wire::pending_key(&lane.unpack(), nonce)
}

fn append_ticket(tickets: &mut Vec<u8>, receipt: &[u8]) -> Result<()> {
    tickets.extend_from_slice(&u32::try_from(receipt.len())?.to_le_bytes());
    tickets.extend_from_slice(receipt);
    Ok(())
}

fn key(prefix: &[u8], id: &Byte32, nonce: Option<u64>) -> Vec<u8> {
    let mut key = prefix.to_vec();
    key.extend_from_slice(id.as_slice());
    if let Some(nonce) = nonce {
        key.extend_from_slice(&nonce.to_le_bytes());
    }
    key
}

fn decode_units(data: &[u8]) -> Result<u128> {
    let units = u128::from_le_bytes(
        data.try_into()
            .context("wrapped asset data must be 16 bytes")?,
    );
    ensure!(units != 0, "wrapped asset amount must be positive");
    Ok(units)
}
