use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, ensure};
use ckb_hash::new_blake2b;
use ckb_types::{
    bytes::Bytes,
    core::{Capacity, ScriptHashType},
    packed::{Byte32, CellOutput, OutPoint, Script},
    prelude::*,
};
use stem_ledger::{
    Transaction,
    protocol::{
        BlockBudget, BridgeLedger, L1Anchor, Lane, Limits, PendingWithdrawal, SettlementChanges,
        VerifiedMint,
    },
    replay,
};
use stem_stake::{Authority, Member};

use super::Store;

const CONFIG: &[u8] = b"bridge/config";
const ANCHOR: &[u8] = b"bridge/anchor";
const DEPOSITS: &[u8] = b"bridge/replay-count";
const LANE: &[u8] = b"bridge/lane/";
const PENDING: &[u8] = b"bridge/pending/";
const DEPOSIT: &[u8] = b"bridge/deposit/";

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
}

#[derive(Default)]
pub(super) struct BridgeChanges {
    pub records: Records,
    pub omit_outputs: BTreeSet<usize>,
    pub protocol_result: Bytes,
    budget: Option<BlockBudget>,
    deposits_remaining: Option<u32>,
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
            records.insert(key(LANE, &name, None), Some(encode_lane(&lane, 0)));
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

        Ok((
            Self {
                l1_genesis: spec.l1_genesis.clone(),
                authority,
                burn: spec.burn_code_hash.clone(),
                lanes,
                recipient_locks,
                reserved_locks: spec.reserved_recipient_locks.clone(),
                mint_limits: spec.mint_limits,
                settlement_limits: spec.settlement_limits,
                deposits_per_block: spec.deposits_per_block,
            },
            records,
        ))
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
        changes.records.insert(
            key(LANE, &observation.vault.lane, None),
            Some(encode_lane(&lane, next_request)),
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

            let name = key(LANE, &lane_id, None);
            let (mut lane, nonce) = match changes.records.get(&name).and_then(Option::as_ref) {
                Some(encoded) => decode_lane(encoded)?,
                None => View {
                    store,
                    staged: &state.records,
                }
                .lane_state(&lane_id)?,
            };
            lane.reserved_capacity = lane
                .reserved_capacity
                .checked_add(cell.capacity())
                .context("pending withdrawal Capacity overflow")?;
            lane.reserved_units = lane
                .reserved_units
                .checked_add(units)
                .context("pending withdrawal units overflow")?;
            ensure!(
                lane.reserved_capacity <= lane.capacity && lane.reserved_units <= lane.units,
                "Burn Cell exceeds available vault reserve"
            );
            let next = nonce.checked_add(1).context("withdrawal nonce exhausted")?;
            let mut receipt = Vec::new();
            receipt.extend_from_slice(lane_id.as_slice());
            receipt.extend_from_slice(&nonce.to_le_bytes());
            receipt.extend_from_slice(tx_id.as_slice());
            receipt.extend_from_slice(&u32::try_from(index)?.to_le_bytes());
            receipt.extend_from_slice(&cell.capacity().to_le_bytes());
            receipt.extend_from_slice(&units.to_le_bytes());
            receipt.extend_from_slice(recipient.as_slice());
            let id = hash(
                b"Stem withdrawal receipt\0",
                &[store.network().as_slice(), &receipt],
            );
            let mut pending = id.as_slice().to_vec();
            pending.extend_from_slice(&receipt);
            let pending_key = key(PENDING, &lane_id, Some(nonce));
            ensure!(
                View {
                    store,
                    staged: &state.records
                }
                .read(&pending_key)?
                .is_none(),
                "withdrawal nonce already pending"
            );
            changes.records.insert(pending_key, Some(pending));
            changes.records.insert(name, Some(encode_lane(&lane, next)));
            changes.omit_outputs.insert(index);
            receipts.extend_from_slice(&u32::try_from(receipt.len())?.to_le_bytes());
            receipts.extend_from_slice(&receipt);
        }
        changes.protocol_result = receipts.into();
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
            changes
                .records
                .insert(key(LANE, id, None), Some(encode_lane(lane, next_request)));
        }
        if let Some((lane, nonce)) = settlement.paid {
            changes
                .records
                .insert(key(PENDING, &lane, Some(nonce)), None);
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
    pub fn bridge_lane(&self, lane: &Byte32) -> Result<Lane> {
        Ok(decode_lane(
            &self
                .system_record(&key(LANE, lane, None))?
                .context("unknown bridge lane")?,
        )?
        .0)
    }

    pub fn bridge_receipt(&self, lane: &Byte32, nonce: u64) -> Result<Option<Vec<u8>>> {
        self.system_record(&key(PENDING, lane, Some(nonce)))
    }
}

struct View<'a> {
    store: &'a Store,
    staged: &'a Records,
}

impl View<'_> {
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

    fn lane_state(&self, lane: &Byte32) -> Result<(Lane, u64)> {
        decode_lane(
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
            .read(&key(PENDING, id, Some(nonce)))?
            .context("missing pending withdrawal")?;
        ensure!(encoded.len() >= 132, "truncated pending withdrawal");
        Ok(PendingWithdrawal {
            receipt: Byte32::from_slice(&encoded[..32])?,
            capacity: u64::from_le_bytes(encoded[108..116].try_into()?),
            units: u128::from_le_bytes(encoded[116..132].try_into()?),
        })
    }
}

pub fn lane_id(genesis: &Byte32, vault_type: &Byte32, asset: Option<&Byte32>) -> Byte32 {
    let tag = [u8::from(asset.is_some())];
    hash(
        b"Stem bridge lane\0",
        &[
            genesis.as_slice(),
            vault_type.as_slice(),
            &tag,
            asset.map_or(&[], |id| id.as_slice()),
        ],
    )
}

fn key(prefix: &[u8], id: &Byte32, nonce: Option<u64>) -> Vec<u8> {
    let mut key = prefix.to_vec();
    key.extend_from_slice(id.as_slice());
    if let Some(nonce) = nonce {
        key.extend_from_slice(&nonce.to_le_bytes());
    }
    key
}

fn hash(domain: &[u8], chunks: &[&[u8]]) -> Byte32 {
    let mut hasher = new_blake2b();
    hasher.update(domain);
    for chunk in chunks {
        hasher.update(chunk);
    }
    let mut bytes = [0; 32];
    hasher.finalize(&mut bytes);
    bytes.pack()
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

fn encode_lane(lane: &Lane, next_request: u64) -> Vec<u8> {
    let mut bytes = lane.vault.as_slice().to_vec();
    bytes.extend_from_slice(&lane.next_nonce.to_le_bytes());
    bytes.extend_from_slice(&lane.capacity.to_le_bytes());
    bytes.extend_from_slice(&lane.units.to_le_bytes());
    bytes.extend_from_slice(&lane.reserved_capacity.to_le_bytes());
    bytes.extend_from_slice(&lane.reserved_units.to_le_bytes());
    bytes.extend_from_slice(&next_request.to_le_bytes());
    bytes
}

fn decode_lane(bytes: &[u8]) -> Result<(Lane, u64)> {
    ensure!(bytes.len() == 100, "invalid bridge lane encoding");
    Ok((
        Lane {
            vault: OutPoint::from_slice(&bytes[..36])?,
            next_nonce: u64::from_le_bytes(bytes[36..44].try_into()?),
            capacity: u64::from_le_bytes(bytes[44..52].try_into()?),
            units: u128::from_le_bytes(bytes[52..68].try_into()?),
            reserved_capacity: u64::from_le_bytes(bytes[68..76].try_into()?),
            reserved_units: u128::from_le_bytes(bytes[76..92].try_into()?),
        },
        u64::from_le_bytes(bytes[92..100].try_into()?),
    ))
}
