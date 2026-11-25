use std::{
    fs::File,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::{CellDep, JsonBytes, OutPoint as JsonPoint, Script as JsonScript};
use ckb_types::{
    H256,
    core::{ScriptHashType, TransactionView},
    packed::{Byte32, CellOutput, OutPoint, Script, Transaction as L1Transaction},
    prelude::*,
};
use clap::Subcommand;
use ed25519_dalek::{Signer, SigningKey};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::json;
use stem::native_store::{BridgeGenesis, LaneGenesis};
use stem_bridge_wire::{Gate, NATIVE_VAULT_SIZE, XUDT_VAULT_SIZE};
use stem_ledger::{
    Cell, Transaction,
    protocol::{
        BridgeMint, Certificate, FeeClaim, FeeFund, L1Anchor, MintObservation, VaultUpdate,
    },
};
use stem_stake::Attestation;

use crate::{
    l1::{Journal, Rpc},
    native_spec,
};

mod payout;
mod reference;
mod vault;

const DEPOSIT_MEMO: &[u8] = b"STEM-PIN";

#[derive(Subcommand)]
pub enum BridgeCommand {
    /// Encode a native CKB deposit output lock and memo for an L1 wallet.
    Deposit {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        l1_rpc: String,
        #[arg(long, value_parser = parse_hash)]
        lane: H256,
        /// JSON string containing the canonical destination Stem Cell bytes.
        #[arg(long)]
        cell: PathBuf,
    },
    /// Observe a confirmed L1 vault consolidation and sign its exact native mint.
    AttestMint {
        #[arg(long)]
        genesis: PathBuf,
        #[arg(long)]
        l1_rpc: String,
        #[arg(long)]
        request: PathBuf,
        #[arg(long)]
        key_file: PathBuf,
        #[arg(long)]
        journal: PathBuf,
    },
    /// Assemble the canonical Bridge Mint transaction from guardian observations.
    Mint {
        #[arg(long)]
        observations: PathBuf,
    },
    /// Claim reconciled fees for an immutable L1 destination.
    ClaimFees {
        /// 0 validators and delegators, 1 development, 2 ecosystem, 3 PoW security.
        #[arg(long)]
        fund: u8,
        /// Claim size in shannons.
        #[arg(long)]
        amount: u64,
        /// Fund's cumulative queued amount in shannons, from /stem/fee-funds.
        #[arg(long)]
        claimed_before: u64,
    },
    /// Verify a pending withdrawal against an authenticated Header and build its CKB input_type.
    WithdrawalWitness(payout::WithdrawalWitness),
    /// Verify a Stem execution proof and confirmed CKB payout before signing.
    AttestPayout(payout::AttestPayout),
    /// Observe a confirmed L1 vault rotation or two-vault reserve adjustment.
    AttestVault(vault::AttestVault),
    /// Observe and sign a historical output or live-at-anchor CKB reference.
    AttestReference(reference::AttestReference),
    /// Attach verified CKB reference certificates to an unsigned Stem transaction.
    AttachReferences(reference::AttachReferences),
    /// Assemble a native settlement from guardian attestations.
    Settle {
        #[arg(long)]
        observations: PathBuf,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    lane: H256,
    vault_input: JsonPoint,
    consolidation: H256,
    previous_l1_height: u64,
    previous_l1_hash: H256,
    #[serde(default)]
    cell_deps: Vec<CellDep>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MintBody {
    l1_genesis: H256,
    deposits: Vec<JsonPoint>,
    authority: H256,
    previous_height: u64,
    previous_hash: H256,
    confirmed_height: u64,
    confirmed_hash: H256,
    lane: H256,
    vault_input: JsonPoint,
    vault_output: JsonPoint,
    capacity: u64,
    units: String,
    cell_deps: Vec<CellDep>,
    outputs: Vec<JsonBytes>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedMint {
    mint: MintBody,
    signer: u16,
    signature: JsonBytes,
}

impl MintBody {
    fn statement(&self) -> BridgeMint {
        BridgeMint {
            l1_genesis: self.l1_genesis.pack(),
            deposits: self.deposits.iter().cloned().map(Into::into).collect(),
        }
    }

    fn observation(&self) -> Result<MintObservation> {
        Ok(MintObservation {
            authority: self.authority.pack(),
            previous_anchor: L1Anchor {
                height: self.previous_height,
                hash: self.previous_hash.pack(),
            },
            anchor: L1Anchor {
                height: self.confirmed_height,
                hash: self.confirmed_hash.pack(),
            },
            vault: VaultUpdate {
                lane: self.lane.pack(),
                input: self.vault_input.clone().into(),
                output: self.vault_output.clone().into(),
            },
            capacity: self.capacity,
            units: self.units.parse()?,
        })
    }

    fn cells(&self) -> Result<Vec<Cell>> {
        self.outputs
            .iter()
            .map(|encoded| Cell::from_slice(encoded.as_bytes()))
            .collect()
    }

    fn deps(&self) -> Vec<ckb_types::packed::CellDep> {
        self.cell_deps.iter().cloned().map(Into::into).collect()
    }
}

impl BridgeCommand {
    pub fn run(self) -> Result<()> {
        match self {
            Self::ClaimFees {
                fund,
                amount,
                claimed_before,
            } => {
                let claim = Transaction::fee_claim(FeeClaim {
                    fund: FeeFund::from_id(fund)?,
                    amount,
                    claimed_before,
                })?;
                println!("{}", json!(JsonBytes::from_vec(claim.as_slice().to_vec())));
                Ok(())
            }
            Self::WithdrawalWitness(args) => args.run(),
            Self::AttestPayout(args) => args.run(),
            Self::AttestVault(args) => args.run(),
            Self::AttestReference(args) => args.run(),
            Self::AttachReferences(args) => args.run(),
            Self::Settle { observations } => payout::settlement(&observations),
            Self::Deposit {
                genesis,
                l1_rpc,
                lane,
                cell,
            } => {
                let spec = native_spec::load(&genesis)?;
                let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
                let lane = find_lane(bridge, &lane.pack())?;
                ensure!(
                    lane.l1_type.is_none(),
                    "this deposit command expects the native CKB lane"
                );
                let encoded: JsonBytes = read_json_limited(&cell, 2 * 1024 * 1024)?;
                let cell = Cell::from_slice(encoded.as_bytes())?;
                ensure!(
                    cell.type_scripts().len() == 0,
                    "native CKB deposit cannot issue a Type Script"
                );
                spec.capacity.verify(&cell)?;
                let rpc = Rpc::new(&l1_rpc)?;
                check_l1(&rpc, bridge)?;
                let (output, data, _) = l1_cell(&rpc, &lane.vault)?;
                ensure!(
                    output
                        .type_()
                        .to_opt()
                        .is_some_and(|script| script == lane.vault_type),
                    "initial vault differs from configured Type ID"
                );
                let lock = deposit_lock(&lane.vault_type.calc_script_hash(), &data)?;
                let mut data = DEPOSIT_MEMO.to_vec();
                data.extend_from_slice(spec.network.as_slice());
                data.extend_from_slice(cell.as_slice());
                let output = CellOutput::new_builder()
                    .capacity(cell.capacity())
                    .lock(lock.clone())
                    .build();
                ensure!(
                    !output.is_lack_of_capacity(ckb_types::core::Capacity::bytes(data.len())?)?,
                    "deposit Cell is below CKB occupied Capacity"
                );
                print_json(&json!({
                    "capacity": cell.capacity(), "lock": JsonScript::from(lock), "data": JsonBytes::from_vec(data),
                }))
            }
            Self::AttestMint {
                genesis,
                l1_rpc,
                request,
                key_file,
                journal,
            } => {
                let spec = native_spec::load(&genesis)?;
                let bridge = spec.bridge.as_ref().context("native bridge disabled")?;
                let request: Request = read_json_limited(&request, 256 * 1024)?;
                let rpc = Rpc::new(&l1_rpc)?;
                check_l1(&rpc, bridge)?;
                let journal = Journal::open(&journal, spec.network.clone())?;
                journal.pin(&rpc, request.previous_l1_height, &request.previous_l1_hash)?;
                let body = observe(&rpc, &spec, bridge, request)?;
                let digest = Transaction::bridge_mint_signing_digest(
                    &spec.network,
                    &body.statement(),
                    &body.observation()?,
                    body.deps(),
                    body.cells()?,
                )?;
                let key = read_key(&key_file)?;
                let signer = bridge
                    .guardians
                    .iter()
                    .position(|member| member.key == key.verifying_key().to_bytes())
                    .context("key is absent from the configured deposit guardians")?;
                journal.pin(&rpc, body.confirmed_height, &body.confirmed_hash)?;
                print_json(&SignedMint {
                    mint: body,
                    signer: u16::try_from(signer)?,
                    signature: JsonBytes::from_vec(key.sign(digest.as_slice()).to_bytes().to_vec()),
                })
            }
            Self::Mint { observations } => {
                let mut approvals: Vec<SignedMint> =
                    read_json_limited(&observations, 32 * 1024 * 1024)?;
                ensure!(!approvals.is_empty(), "no guardian approvals");
                approvals.sort_unstable_by_key(|approval| approval.signer);
                let body = &approvals[0].mint;
                let approvals = approvals
                    .iter()
                    .map(|approval| {
                        ensure!(
                            &approval.mint == body,
                            "guardians approved different mint payloads"
                        );
                        Ok(Attestation {
                            guardian: approval.signer,
                            signature: approval.signature.as_bytes().try_into()?,
                        })
                    })
                    .collect::<Result<_>>()?;
                let certificate = Certificate::new(body.observation()?.encode(), approvals)?;
                let tx = Transaction::bridge_mint(
                    body.statement(),
                    certificate,
                    body.deps(),
                    body.cells()?,
                )?;
                print_json(&JsonBytes::from_vec(tx.as_slice().to_vec()))
            }
        }
    }
}

fn observe(
    rpc: &Rpc,
    spec: &stem::native_store::Spec,
    bridge: &BridgeGenesis,
    request: Request,
) -> Result<MintBody> {
    let lane = find_lane(bridge, &request.lane.pack())?;
    let input: OutPoint = request.vault_input.clone().into();
    let (before, vault_data, vault_height) = l1_cell(rpc, &input)?;
    ensure!(
        before
            .type_()
            .to_opt()
            .is_some_and(|script| script == lane.vault_type),
        "consolidation input is outside the configured vault Type ID"
    );
    let (tx, height, consolidation_block) = committed(rpc, &request.consolidation)?;
    ensure!(
        height > vault_height && tx.input_pts_iter().any(|point| point == input),
        "consolidation did not consume the configured vault cell"
    );
    let mut vaults = tx
        .outputs_with_data_iter()
        .enumerate()
        .filter(|(_, (output, _))| {
            output
                .type_()
                .to_opt()
                .is_some_and(|script| script == lane.vault_type)
        });
    let (index, (after, successor)) = vaults
        .next()
        .context("consolidation did not recreate the vault")?;
    ensure!(
        vaults.next().is_none() && successor.get(1) == Some(&0) && before.lock() == after.lock(),
        "L1 transaction is not a vault consolidation"
    );

    let expected = deposit_lock(&lane.vault_type.calc_script_hash(), &vault_data)?;
    let gate = match &lane.l1_type {
        Some(token) => {
            let asset = stem_bridge_wire::vault_asset(&vault_data)
                .context("vault has no xUDT configuration")?;
            ensure!(
                asset.type_hash.as_slice() == token.calc_script_hash().as_slice(),
                "vault has a different L1 xUDT identity"
            );
            Some(asset.gate_code)
        }
        None => {
            ensure!(
                vault_data.len() == NATIVE_VAULT_SIZE,
                "CKB lane requires a CKB-only vault"
            );
            None
        }
    };

    let mut deposits = Vec::new();
    let mut capacity = 0u64;
    let mut units = 0u128;
    for point in tx.input_pts_iter().filter(|point| point != &input) {
        let (output, data, origin) = l1_cell(rpc, &point)?;
        ensure!(
            origin <= height,
            "consolidation input not present on the confirmed L1 history"
        );
        let lock = output.lock();
        let amount = u64::from(output.capacity());
        let cell = if lock == expected && output.type_().to_opt().is_none() {
            ensure!(
                gate.is_none(),
                "native deposits must use the CKB vault lane"
            );
            let raw = data
                .strip_prefix(DEPOSIT_MEMO)
                .and_then(|memo| memo.strip_prefix(spec.network.as_slice()))
                .context("native deposit has a different network or memo")?;
            let cell = Cell::from_slice(raw)?;
            ensure!(
                cell.type_scripts().len() == 0 && cell.capacity() == amount,
                "CKB deposit memo exceeds its L1 backing"
            );
            cell
        } else if gate.is_some_and(|code| code.as_slice() == lock.code_hash().as_slice())
            && lock.hash_type() == ScriptHashType::Data1.into()
        {
            let args = lock.args().raw_data();
            let (vault, role) = stem_bridge_wire::gate(&args).context("invalid xUDT gate")?;
            if vault.as_slice() != lane.vault_type.calc_script_hash().as_slice()
                || role == Gate::Escrow
            {
                continue;
            }
            let Gate::Deposit { chain, recipient } = role else {
                unreachable!()
            };
            let token = lane.l1_type.as_ref().context("missing L1 xUDT asset")?;
            ensure!(
                chain.as_slice() == spec.network.as_slice()
                    && output.type_().to_opt().as_ref() == Some(token),
                "xUDT deposit identifies a different Stem network or asset"
            );
            let units_in = stem_bridge_wire::amount(&data)
                .filter(|&amount| amount != 0)
                .context("invalid xUDT amount")?;
            units = units
                .checked_add(units_in)
                .context("xUDT amount overflow")?;
            let asset = Script::new_builder()
                .code_hash(bridge.wrapped_code_hash.clone())
                .hash_type(ScriptHashType::Data)
                .args(token.calc_script_hash().as_bytes())
                .build();
            Cell::new(amount, Script::from_slice(recipient)?, vec![asset], data)?
        } else {
            continue;
        };
        ensure!(
            cell.lock().code_hash() != bridge.burn_code_hash,
            "deposit recipient is a Burn Lock"
        );
        spec.capacity.verify(&cell)?;
        capacity = capacity
            .checked_add(amount)
            .context("deposit Capacity overflow")?;
        deposits.push((point, cell));
    }
    ensure!(
        !deposits.is_empty() && deposits.len() <= 128,
        "missing or too many L1 deposits"
    );
    ensure!(
        u64::from(before.capacity()).checked_add(capacity) == Some(u64::from(after.capacity())),
        "vault Capacity did not increase by the observed deposit total"
    );
    deposits.sort_unstable_by(|(a, _), (b, _)| a.as_slice().cmp(b.as_slice()));
    let confirmed_height = height
        .checked_add(bridge.confirmations)
        .context("L1 confirmation height overflow")?
        .max(
            request
                .previous_l1_height
                .checked_add(1)
                .context("L1 anchor height overflow")?,
        );
    let confirmed_hash = rpc.block_hash(confirmed_height)?;
    ensure!(
        rpc.block_hash(height)? == consolidation_block,
        "consolidation was reorganized during observation"
    );
    Ok(MintBody {
        l1_genesis: bridge.l1_genesis.unpack(),
        deposits: deposits
            .iter()
            .map(|(point, _)| point.clone().into())
            .collect(),
        authority: bridge.guardian_generation.unpack(),
        previous_height: request.previous_l1_height,
        previous_hash: request.previous_l1_hash,
        confirmed_height,
        confirmed_hash,
        lane: request.lane,
        vault_input: request.vault_input,
        vault_output: OutPoint::new(tx.hash(), u32::try_from(index)?).into(),
        capacity,
        units: units.to_string(),
        cell_deps: request.cell_deps,
        outputs: deposits
            .iter()
            .map(|(_, cell)| JsonBytes::from_vec(cell.as_slice().to_vec()))
            .collect(),
    })
}

fn deposit_lock(vault_type: &Byte32, data: &[u8]) -> Result<Script> {
    ensure!(
        [NATIVE_VAULT_SIZE, XUDT_VAULT_SIZE].contains(&data.len()),
        "invalid L1 vault state"
    );
    Ok(Script::new_builder()
        .code_hash(Byte32::from_slice(&data[10..42])?)
        .hash_type(ScriptHashType::Data1)
        .args(vault_type.as_bytes())
        .build())
}

fn check_l1(rpc: &Rpc, bridge: &BridgeGenesis) -> Result<()> {
    ensure!(
        rpc.block_hash(0)?.as_bytes() == bridge.l1_genesis.as_slice(),
        "incorrect CKB L1 genesis"
    );
    Ok(())
}

fn find_lane<'a>(bridge: &'a BridgeGenesis, id: &Byte32) -> Result<&'a LaneGenesis> {
    bridge
        .lanes
        .iter()
        .find(|lane| lane.id(&bridge.l1_genesis) == *id)
        .context("unknown bridge lane")
}

fn committed(rpc: &Rpc, tx: &H256) -> Result<(TransactionView, u64, H256)> {
    let (raw, height, block) = rpc.committed(tx)?;
    ensure!(
        rpc.block_hash(height)? == block,
        "L1 transaction was reorganized"
    );
    Ok((
        L1Transaction::from_slice(raw.as_bytes())?.into_view(),
        height,
        block,
    ))
}

fn l1_cell(rpc: &Rpc, point: &OutPoint) -> Result<(CellOutput, ckb_types::bytes::Bytes, u64)> {
    let (tx, height, _) = committed(rpc, &point.tx_hash().unpack())?;
    let index = point.index().unpack();
    let cell = tx
        .output(index)
        .context("L1 Cell OutPoint index is out of range")?;
    let data = tx
        .outputs_data()
        .get(index)
        .context("L1 Cell lacks data")?
        .raw_data();
    Ok((cell, data, height))
}

pub(crate) fn parse_hash(value: &str) -> Result<H256> {
    let bytes = hex::decode(value.strip_prefix("0x").unwrap_or(value))?;
    let value: [u8; 32] = bytes.try_into().ok().context("expected a 32-byte hash")?;
    Ok(value.into())
}

pub(crate) fn read_key(path: &Path) -> Result<SigningKey> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.permissions().mode() & 0o077 == 0,
        "guardian key file must be private"
    );
    let mut key = Vec::new();
    file.take(33).read_to_end(&mut key)?;
    Ok(SigningKey::try_from(key.as_slice())?)
}

pub fn read_json_limited<T: DeserializeOwned>(path: &Path, limit: usize) -> Result<T> {
    let mut raw = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut raw)?;
    ensure!(raw.len() <= limit, "JSON input file exceeds size limit");
    Ok(serde_json::from_slice(&raw)?)
}

pub fn print_json(value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(std::io::stdout().lock(), value)?;
    Ok(())
}
