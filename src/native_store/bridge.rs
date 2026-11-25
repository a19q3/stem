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
        FeeFundPolicy, FeePayouts, L1Anchor, Lane, Limits, PendingWithdrawal, SettlementChanges,
        VerifiedMint,
    },
    replay,
};
use stem_stake::{Authority, Member};

use super::Store;

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
    pub deposits_per_block: u32,
    pub fee_funds: Option<[FeeFundDestination; 4]>,
}

pub struct LaneGenesis {
    pub vault_type_hash: Byte32,
    pub vault: OutPoint,
    pub l1_type: Option<Script>,
}

pub struct RecipientLock {
    pub code_hash: Byte32,
    pub hash_type: ScriptHashType,
    pub args_len: u32,
}

pub(super) struct Bridge {
    l1_genesis: Byte32,
    authority: Authority,
    burn: Byte32,
    lanes: BTreeMap<Byte32, Asset>,
    recipient_locks: Vec<(Byte32, u8, u32)>,
    reserved_locks: Vec<Script>,
    mint_limits: Limits,
    settlement_limits: Limits,
    deposits_per_block: u32,
    fee_funds: Option<FeeFundPolicy>,
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
                    .context("fund claims exceed total fees")?
            } else {
                total
            };
            ensure!(
                backing.unallocated() == expected,
                "bridge backing differs from fee accounting"
            );
            self.records
                .insert(BACKING.to_vec(), Some(backing.encode().to_vec()));
        }
        Ok(self.records)
    }
}

impl Bridge {
    pub fn initialize(spec: &BridgeGenesis) -> Result<(Self, Records)> {
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
        records.insert(ANCHOR.to_vec(), Some(encode_anchor(&spec.anchor)));
        records.insert(
            BACKING.to_vec(),
            Some(BridgeBacking::default().encode().to_vec()),
        );
        records.insert(DEPOSITS.to_vec(), Some(0u64.to_le_bytes().to_vec()));

        let mut lanes = BTreeMap::new();
        let mut assets = BTreeSet::new();
        let mut vaults = BTreeSet::new();
        let mut identities = BTreeSet::new();
        let mut config = Vec::new();
        for id in [
            &spec.l1_genesis,
            &spec.guardian_generation,
            &spec.wrapped_code_hash,
            &spec.burn_code_hash,
        ] {
            config.extend_from_slice(id.as_slice());
        }
        for limit in [spec.mint_limits, spec.settlement_limits] {
            config.extend_from_slice(&limit.operations.to_le_bytes());
            config.extend_from_slice(&limit.bytes.to_le_bytes());
            config.extend_from_slice(&limit.signatures.to_le_bytes());
        }
        config.extend_from_slice(&spec.confirmations.to_le_bytes());
        config.extend_from_slice(&spec.deposits_per_block.to_le_bytes());
        config.extend_from_slice(&u32::try_from(spec.guardians.len())?.to_le_bytes());
        for guardian in &spec.guardians {
            config.extend_from_slice(&guardian.key);
            config.extend_from_slice(&guardian.power.to_le_bytes());
        }

        config.extend_from_slice(&u32::try_from(spec.lanes.len())?.to_le_bytes());
        for lane in &spec.lanes {
            ensure!(
                lane.vault_type_hash != Byte32::default()
                    && !lane.vault.is_null()
                    && vaults.insert(lane.vault.clone())
                    && identities.insert(lane.vault_type_hash.clone()),
                "missing or duplicate vault"
            );
            let l1_hash = lane.l1_type.as_ref().map(Script::calc_script_hash);
            if let Some(l1) = &lane.l1_type {
                ScriptHashType::try_from(l1.hash_type())?;
            }
            ensure!(assets.insert(l1_hash.clone()), "duplicate L1 asset");
            let name = lane_id(&spec.l1_genesis, &lane.vault_type_hash, l1_hash.as_ref());
            config.extend_from_slice(name.as_slice());
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

        config.extend_from_slice(&u32::try_from(spec.recipient_locks.len())?.to_le_bytes());
        let recipient_locks = spec
            .recipient_locks
            .iter()
            .map(|lock| {
                config.extend_from_slice(lock.code_hash.as_slice());
                config.push(lock.hash_type as u8);
                config.extend_from_slice(&lock.args_len.to_le_bytes());
                (lock.code_hash.clone(), lock.hash_type as u8, lock.args_len)
            })
            .collect();
        config
            .extend_from_slice(&u32::try_from(spec.reserved_recipient_locks.len())?.to_le_bytes());
        for lock in &spec.reserved_recipient_locks {
            config.extend_from_slice(&u32::try_from(lock.as_slice().len())?.to_le_bytes());
            config.extend_from_slice(lock.as_slice());
        }
        records.insert(CONFIG.to_vec(), Some(config));

        let mut bridge = Self {
            l1_genesis: spec.l1_genesis.clone(),
            authority,
            burn: spec.burn_code_hash.clone(),
            lanes,
            recipient_locks,
            reserved_locks: spec.reserved_recipient_locks.clone(),
            mint_limits: spec.mint_limits,
            settlement_limits: spec.settlement_limits,
            deposits_per_block: spec.deposits_per_block,
            fee_funds: None,
        };
        if let Some(funds) = &spec.fee_funds {
            let (lane, _) = bridge
                .lanes
                .iter()
                .find(|(_, asset)| asset.l1.is_none())
                .context("fee funds require a native CKB vault")?;
            for fund in funds {
                bridge.check_recipient(&fund.recipient)?;
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
            budget: BlockBudget::new(self.mint_limits, zero, self.settlement_limits),
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
            &self.authority,
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
        changes
            .records
            .insert(ANCHOR.to_vec(), Some(encode_anchor(&observation.anchor)));

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
        let policy = self.fee_funds.as_ref().context("fee funds are disabled")?;
        let bit = 1 << claim.fund as u8;
        ensure!(
            state.claimed_funds & bit == 0,
            "fee fund already claimed in this block"
        );
        let view = View {
            store,
            staged: &state.records,
        };
        let destination = &policy.funds[claim.fund as usize];
        let output = CellOutput::new_builder()
            .capacity(claim.amount)
            .lock(destination.recipient.clone())
            .build();
        ensure!(
            !output.is_lack_of_capacity(Capacity::zero())?,
            "fee claim is below L1 occupied Capacity"
        );
        let mut payouts = view.fee_payouts()?;
        let allocation = FeeFund::allocation(store.fee_surplus()?, destination.basis_points)?;
        payouts.queue(claim, allocation)?;
        let backing = view.backing()?.reserve_fee_payout(claim.amount)?;
        let mut changes = BridgeChanges {
            claimed_funds: bit,
            ..Default::default()
        };
        let (nonce, receipt) = view.queue_withdrawal(
            &mut changes.records,
            Withdrawal {
                lane: &policy.lane,
                tx_id: &tx.id(store.network()),
                index: 0,
                capacity: claim.amount,
                units: 0,
                recipient: &destination.recipient,
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

    pub fn settle(
        &self,
        store: &Store,
        state: &StagedBridge,
        tx: &Transaction,
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
            &self.authority,
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
        changes
            .records
            .insert(ANCHOR.to_vec(), Some(encode_anchor(&settlement.anchor)));
        changes.protocol_result = settlement.anchor.hash.as_bytes();
        Ok(changes)
    }

    fn is_burn(&self, lock: &Script) -> bool {
        lock.code_hash() == self.burn
    }

    fn check_recipient(&self, recipient: &Script) -> Result<()> {
        let hash_type = recipient.hash_type().as_slice()[0];
        ensure!(
            recipient.as_slice().len() <= stem_bridge_wire::MAX_RECIPIENT_SIZE,
            "L1 recipient Lock exceeds the withdrawal proof size limit"
        );
        ensure!(
            !self.reserved_locks.contains(recipient),
            "reserved L1 recipient"
        );
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
}

impl Store {
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
        let mut receipt = Vec::new();
        receipt.extend_from_slice(withdrawal.lane.as_slice());
        receipt.extend_from_slice(&nonce.to_le_bytes());
        receipt.extend_from_slice(withdrawal.tx_id.as_slice());
        receipt.extend_from_slice(&withdrawal.index.to_le_bytes());
        receipt.extend_from_slice(&withdrawal.capacity.to_le_bytes());
        receipt.extend_from_slice(&withdrawal.units.to_le_bytes());
        receipt.extend_from_slice(withdrawal.recipient.as_slice());
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
        let encoded = self.read(ANCHOR)?.context("missing L1 backing anchor")?;
        ensure!(encoded.len() == 40, "invalid L1 backing anchor");
        Ok(L1Anchor {
            height: u64::from_le_bytes(encoded[..8].try_into()?),
            hash: Byte32::from_slice(&encoded[8..])?,
        })
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

fn encode_anchor(anchor: &L1Anchor) -> Vec<u8> {
    let mut value = anchor.height.to_le_bytes().to_vec();
    value.extend_from_slice(anchor.hash.as_slice());
    value
}

fn decode_units(data: &[u8]) -> Result<u128> {
    let units = u128::from_le_bytes(
        data.try_into()
            .context("wrapped asset data must be 16 bytes")?,
    );
    ensure!(units != 0, "wrapped asset amount must be positive");
    Ok(units)
}
