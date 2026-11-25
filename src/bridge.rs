use anyhow::{Context, Result, ensure};
use ckb_hash::{blake2b_256, new_blake2b};
use ckb_jsonrpc_types::{JsonBytes, OutPoint as JsonOutPoint, Script as JsonScript};
use ckb_types::{
    H256,
    bytes::Bytes,
    core::{ScriptHashType, TransactionBuilder, TransactionView},
    packed::{self, Byte32, CellInput, CellOutput, OutPoint, Script},
    prelude::*,
};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use fern_bridge_wire::{self as wire, Gate};
use serde::{Deserialize, Serialize};

use crate::genesis::Validator;

const PEG_IN: u64 = u64::MAX;
const DEPOSIT_MEMO: &[u8] = b"CKB-PIN1";
const PAYOUT_MEMO: &[u8] = b"CKB-POU1";

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Bridge {
    pub l1_genesis: H256,
    pub deposit_lock: JsonScript,
    pub vault_type_hash: H256,
    pub confirmations: u64,
    pub guardians: Vec<Validator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xudt: Option<Xudt>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Xudt {
    pub gate_code_hash: H256,
    pub l1_type: JsonScript,
}

impl Xudt {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.gate_code_hash != H256::default() && self.l1_type.code_hash != H256::default(),
            "missing xUDT program or custody lock identity"
        );
        let args = self.l1_type.args.as_bytes();
        ensure!(
            self.l1_type.hash_type == ckb_jsonrpc_types::ScriptHashType::Data1
                && (args.len() == 32 || (args.len() == 36 && args[32..] == [0; 4])),
            "bridge requires plain Data1 xUDT with default owner flags"
        );
        Ok(())
    }

    pub fn sidechain_type(&self) -> Script {
        let l1: Script = self.l1_type.clone().into();
        let issuer = crate::genesis::SYSTEM_LOCK
            .clone()
            .as_builder()
            .args(l1.calc_script_hash().as_slice().pack())
            .build();
        l1.as_builder()
            .args(issuer.calc_script_hash().as_slice().pack())
            .build()
    }
}

pub struct Payout {
    pub recipient: Script,
    pub units: Option<u128>,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Deposit {
    pub deposit_tx: JsonBytes,
    pub output_index: u32,
    pub deposit_block: H256,
    pub deposit_height: u64,
    pub consolidation_tx: JsonBytes,
    pub consolidation_block: H256,
    pub consolidation_height: u64,
    pub confirmed_block: H256,
    pub confirmed_height: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    pub guardian: u16,
    pub signature: JsonBytes,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SignedObservation {
    pub deposit: Deposit,
    pub attestation: Attestation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<JsonOutPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_backing_anchor: Option<BackingAnchor>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Certificate {
    pub deposit: Deposit,
    pub attestations: Vec<Attestation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<JsonOutPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_backing_anchor: Option<BackingAnchor>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Guardians {
    pub members: Vec<Validator>,
    pub generation: Option<JsonOutPoint>,
    pub backing_anchor: Option<BackingAnchor>,
}

impl Guardians {
    pub fn fixed(bridge: &Bridge) -> Self {
        Self {
            members: bridge.guardians.clone(),
            generation: None,
            backing_anchor: None,
        }
    }

    pub fn point(&self) -> Option<OutPoint> {
        self.generation.clone().map(Into::into)
    }
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct BackingAnchor {
    pub height: u64,
    pub hash: H256,
}

impl Certificate {
    pub fn from_mint(tx: &TransactionView) -> Result<Self> {
        ensure!(
            tx.witnesses().len() == 1,
            "peg-in requires one certificate witness"
        );
        let witness = tx.witnesses().get(0).context("missing peg-in witness")?;
        Ok(serde_json::from_slice(&witness.raw_data())?)
    }

    pub fn anchor(&self) -> BackingAnchor {
        BackingAnchor {
            height: self.deposit.confirmed_height,
            hash: self.deposit.confirmed_block.clone(),
        }
    }

    pub fn uses_current_authority(&self, guardians: &Guardians) -> bool {
        self.generation == guardians.generation
            && self.previous_backing_anchor == guardians.backing_anchor
    }
}

impl Bridge {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.confirmations > 0,
            "bridge requires a positive L1 confirmation depth"
        );
        ensure!(
            self.l1_genesis != H256::default() && self.vault_type_hash != H256::default(),
            "invalid L1 anchor"
        );
        ensure!(
            self.deposit_lock.hash_type == ckb_jsonrpc_types::ScriptHashType::Data1
                && self.deposit_lock.args.as_bytes() == self.vault_type_hash.as_bytes(),
            "deposit lock must be Data1 and name the vault type hash"
        );
        crate::genesis::validate_committee(&self.guardians)?;
        for member in &self.guardians {
            ensure!(!key(member)?.is_weak(), "weak guardian key");
        }
        if let Some(asset) = &self.xudt {
            asset.validate()?;
            ensure!(
                asset.gate_code_hash != self.deposit_lock.code_hash,
                "native and xUDT custody locks must be distinct"
            );
        }
        Ok(())
    }

    pub fn build_mint(
        &self,
        anchor: &Byte32,
        certificate: &Certificate,
        guardians: &Guardians,
    ) -> Result<TransactionView> {
        ensure!(
            certificate.generation == guardians.generation,
            "certificate names inactive guardian generation"
        );
        ensure!(
            certificate.previous_backing_anchor == guardians.backing_anchor,
            "certificate names an outdated L1 backing anchor"
        );
        if let Some(known) = &guardians.backing_anchor {
            ensure!(
                known.height != certificate.deposit.confirmed_height
                    || known.hash == certificate.deposit.confirmed_block,
                "peg-in confirmation conflicts with the committed L1 backing anchor"
            );
        }
        let tx = self.mint_payload(anchor, &certificate.deposit)?;
        let digest = self.digest(anchor, &tx, &certificate.deposit, guardians);
        let mut last = None;
        let mut weight = 0i128;
        let total = guardians
            .members
            .iter()
            .map(|v| i128::from(v.power))
            .sum::<i128>();
        for vote in &certificate.attestations {
            ensure!(
                last.is_none_or(|prior| prior < vote.guardian),
                "guardians must be ordered and unique"
            );
            let member = guardians
                .members
                .get(usize::from(vote.guardian))
                .context("unknown guardian")?;
            let sig = Signature::from_slice(vote.signature.as_bytes())?;
            key(member)?.verify_strict(&digest, &sig)?;
            weight += i128::from(member.power);
            last = Some(vote.guardian);
        }
        ensure!(
            3 * weight > 2 * total,
            "peg-in requires more than two thirds of guardian power"
        );
        let witness = Bytes::from(serde_json::to_vec(certificate)?);
        Ok(tx.as_advanced_builder().witness(witness).build())
    }

    pub fn verify_mint(
        &self,
        anchor: &Byte32,
        tx: &TransactionView,
        guardians: &Guardians,
    ) -> Result<BackingAnchor> {
        let certificate = Certificate::from_mint(tx)?;
        let expected = self.build_mint(anchor, &certificate, guardians)?;
        ensure!(
            tx.data().raw().as_slice() == expected.data().raw().as_slice(),
            "peg-in differs from the authenticated L1 deposit"
        );
        Ok(certificate.anchor())
    }

    pub fn attest(
        &self,
        anchor: &Byte32,
        deposit: &Deposit,
        signer: &SigningKey,
        guardians: &Guardians,
    ) -> Result<Attestation> {
        let public = signer.verifying_key().to_bytes();
        let guardian = guardians
            .members
            .iter()
            .position(|v| v.ed25519.as_bytes() == public)
            .context("key is not a configured bridge guardian")?;
        let tx = self.mint_payload(anchor, deposit)?;
        let signature = signer.sign(&self.digest(anchor, &tx, deposit, guardians));
        Ok(Attestation {
            guardian: guardian.try_into()?,
            signature: JsonBytes::from_vec(signature.to_vec()),
        })
    }

    pub fn mint_payload(&self, anchor: &Byte32, proof: &Deposit) -> Result<TransactionView> {
        ensure!(
            proof.deposit_height <= proof.consolidation_height
                && proof
                    .consolidation_height
                    .checked_add(self.confirmations)
                    .is_some_and(|confirmed| confirmed == proof.confirmed_height),
            "inconsistent L1 confirmation proof"
        );
        ensure!(
            proof.deposit_block != H256::default()
                && proof.consolidation_block != H256::default()
                && proof.confirmed_block != H256::default(),
            "missing L1 block identity"
        );
        let transaction = packed::Transaction::from_slice(proof.deposit_tx.as_bytes())?.into_view();
        let index = proof.output_index as usize;
        let output = transaction
            .output(index)
            .context("missing deposit output")?;
        let packed_data = transaction
            .outputs_data()
            .get(index)
            .context("missing deposit memo")?;
        let data = packed_data.raw_data();
        let (recipient, wrapped, minted_data) = match output.type_().to_opt() {
            None => {
                let deposit_lock: Script = self.deposit_lock.clone().into();
                ensure!(
                    output.lock() == deposit_lock,
                    "not a native CKB deposit Cell"
                );
                let memo = data
                    .strip_prefix(DEPOSIT_MEMO)
                    .and_then(|memo| memo.strip_prefix(anchor.as_slice()))
                    .context("deposit memo identifies another sidechain")?;
                (Script::from_slice(memo)?, None, Bytes::new())
            }
            Some(token) => {
                let asset = self.xudt.as_ref().context("xUDT bridge is disabled")?;
                let expected: Script = asset.l1_type.clone().into();
                ensure!(token == expected, "unregistered L1 asset");
                ensure!(
                    wire::amount(&data).is_some_and(|units| units > 0),
                    "invalid xUDT deposit amount"
                );
                let lock = output.lock();
                let args = lock.args().raw_data();
                ensure!(
                    lock.code_hash().as_slice() == asset.gate_code_hash.as_bytes()
                        && lock.hash_type() == ScriptHashType::Data1.into(),
                    "invalid xUDT deposit lock"
                );
                let (vault, role) = wire::gate(&args).context("invalid xUDT deposit lock args")?;
                let Gate::Deposit { chain, recipient } = role else {
                    anyhow::bail!("xUDT escrow cannot be minted as a deposit");
                };
                ensure!(
                    vault == self.vault_type_hash.as_bytes(),
                    "deposit names a different L1 vault"
                );
                ensure!(
                    chain == anchor.as_slice(),
                    "deposit names a different sidechain"
                );
                (
                    Script::from_slice(recipient)?,
                    Some(asset.sidechain_type()),
                    data,
                )
            }
        };
        ScriptHashType::try_from(recipient.hash_type())?;
        ensure!(
            !is_burn(&recipient),
            "deposit recipient is the reserved burn lock"
        );
        let point = OutPoint::new(transaction.hash(), proof.output_index);
        let consolidation =
            packed::Transaction::from_slice(proof.consolidation_tx.as_bytes())?.into_view();
        ensure!(
            consolidation
                .input_pts_iter()
                .filter(|input| *input == point)
                .count()
                == 1,
            "vault consolidation did not consume the deposit"
        );
        let mut vaults = consolidation
            .outputs_with_data_iter()
            .filter(|(output, _)| {
                output.type_().to_opt().is_some_and(|script| {
                    script.calc_script_hash().as_slice() == self.vault_type_hash.as_bytes()
                })
            });
        let (_, state) = vaults
            .next()
            .context("consolidation did not recreate the configured vault")?;
        ensure!(
            vaults.next().is_none(),
            "consolidation recreated multiple vaults"
        );
        if wrapped.is_some() {
            self.verify_xudt_escrow(&consolidation, &state, &minted_data)?;
        }
        let output = CellOutput::new_builder()
            .capacity(output.capacity())
            .lock(recipient)
            .type_(wrapped)
            .build();
        ensure!(
            !output.is_lack_of_capacity(ckb_types::core::Capacity::bytes(minted_data.len())?)?,
            "deposit is below occupied capacity"
        );
        Ok(TransactionBuilder::default()
            .input(CellInput::new(point, PEG_IN))
            .header_dep(anchor.clone())
            .output(output)
            .output_data(minted_data)
            .build())
    }

    fn verify_xudt_escrow(
        &self,
        consolidation: &TransactionView,
        state: &[u8],
        deposit: &[u8],
    ) -> Result<()> {
        let asset = self.xudt.as_ref().context("xUDT bridge is disabled")?;
        let l1_type: Script = asset.l1_type.clone().into();
        let profile =
            wire::vault_asset(state).context("vault did not commit to the xUDT profile")?;
        ensure!(
            state[1] == 0
                && profile.gate_code == *asset.gate_code_hash.as_bytes()
                && profile.type_hash == l1_type.calc_script_hash().as_slice(),
            "vault consolidation uses a different xUDT profile"
        );
        let lock = self.xudt_escrow_lock()?;
        let mut units = 0u128;
        for (output, data) in consolidation
            .outputs_with_data_iter()
            .filter(|(output, _)| output.lock() == lock)
        {
            ensure!(
                output.type_().to_opt().as_ref() == Some(&l1_type),
                "escrow contains a different xUDT asset"
            );
            let amount = wire::amount(&data).context("invalid xUDT escrow amount")?;
            units = units
                .checked_add(amount)
                .context("xUDT escrow amount overflow")?;
        }
        ensure!(
            units >= wire::amount(deposit).context("invalid xUDT deposit amount")?,
            "consolidation escrow cannot back the deposit"
        );
        Ok(())
    }

    pub fn xudt_deposit_lock(&self, anchor: &Byte32, recipient: &Script) -> Result<Script> {
        ensure!(
            recipient.as_slice().len() <= wire::MAX_RECIPIENT_SIZE,
            "xUDT recipient is too large"
        );
        self.xudt_gate_lock(wire::DEPOSIT, &[anchor.as_slice(), recipient.as_slice()])
    }

    pub fn xudt_escrow_lock(&self) -> Result<Script> {
        self.xudt_gate_lock(wire::ESCROW, &[])
    }

    fn xudt_gate_lock(&self, role: u8, fields: &[&[u8]]) -> Result<Script> {
        let asset = self.xudt.as_ref().context("xUDT bridge is disabled")?;
        let mut args = self.vault_type_hash.as_bytes().to_vec();
        args.push(role);
        for field in fields {
            args.extend_from_slice(field);
        }
        Ok(Script::new_builder()
            .code_hash(asset.gate_code_hash.pack())
            .hash_type(ScriptHashType::Data1)
            .args(Bytes::from(args))
            .build())
    }

    pub fn payout(&self, output: &CellOutput, data: &[u8], anchor: &Byte32) -> Result<Payout> {
        let (recipient, units, l1_type) = match output.type_().to_opt() {
            None => {
                ensure!(
                    output.lock() == burn_lock(anchor),
                    "invalid native CKB burn lock"
                );
                let memo = data
                    .strip_prefix(PAYOUT_MEMO)
                    .context("invalid payout memo")?;
                (Script::from_slice(memo)?, None, None)
            }
            Some(wrapped) => {
                let asset = self.xudt.as_ref().context("xUDT bridge is disabled")?;
                ensure!(
                    wrapped == asset.sidechain_type(),
                    "unregistered sidechain asset"
                );
                let lock = output.lock();
                let args = lock.args().raw_data();
                let recipient = args
                    .strip_prefix(anchor.as_slice())
                    .context("incorrect xUDT burn chain")?;
                let recipient = Script::from_slice(recipient)?;
                ensure!(
                    lock == xudt_burn_lock(anchor, &recipient),
                    "invalid xUDT burn lock"
                );
                let amount = wire::amount(data)
                    .filter(|amount| *amount > 0)
                    .context("invalid xUDT burn amount")?;
                (recipient, Some(amount), Some(asset.l1_type.clone().into()))
            }
        };
        self.validate_payout_lock(&recipient)?;
        let payout = CellOutput::new_builder()
            .capacity(output.capacity())
            .lock(recipient.clone())
            .type_(l1_type)
            .build();
        let data_size = if units.is_some() {
            wire::AMOUNT_SIZE
        } else {
            0
        };
        ensure!(
            !payout.is_lack_of_capacity(ckb_types::core::Capacity::bytes(data_size)?)?,
            "payout is below L1 occupied capacity"
        );
        Ok(Payout { recipient, units })
    }

    pub fn validate_payout_lock(&self, recipient: &Script) -> Result<()> {
        ensure!(
            recipient.as_slice().len() <= wire::MAX_RECIPIENT_SIZE,
            "L1 payout recipient is too large"
        );
        ensure!(
            matches!(
                ScriptHashType::try_from(recipient.hash_type())?,
                ScriptHashType::Data
                    | ScriptHashType::Type
                    | ScriptHashType::Data1
                    | ScriptHashType::Data2
            ),
            "L1 payout recipient uses an unsupported Script hash type"
        );
        ensure!(
            recipient.code_hash() != Byte32::default()
                && recipient.code_hash().as_slice() != self.deposit_lock.code_hash.as_bytes()
                && self.xudt.as_ref().is_none_or(
                    |asset| recipient.code_hash().as_slice() != asset.gate_code_hash.as_bytes()
                ),
            "L1 payout recipient uses a reserved code hash"
        );
        Ok(())
    }

    pub fn verify_asset_balance(
        &self,
        resolved: &ckb_types::core::cell::ResolvedTransaction,
    ) -> Result<()> {
        let Some(asset) = &self.xudt else {
            return Ok(());
        };
        let wrapped = asset.sidechain_type();
        let units = |output: &CellOutput, data: Option<&[u8]>| -> Result<u128> {
            if output.type_().to_opt().as_ref() != Some(&wrapped) {
                return Ok(0);
            }
            data.and_then(wire::amount)
                .context("invalid wrapped xUDT cell data")
        };
        let mut input = 0u128;
        for cell in &resolved.resolved_inputs {
            let amount = units(&cell.cell_output, cell.mem_cell_data.as_deref())?;
            input = input
                .checked_add(amount)
                .context("wrapped xUDT input overflow")?;
        }
        let mut output = 0u128;
        for (cell, data) in resolved.transaction.outputs_with_data_iter() {
            output = output
                .checked_add(units(&cell, Some(&data))?)
                .context("wrapped xUDT output overflow")?;
        }
        // Burns remain outputs for the balance check and are dropped from the live set.
        ensure!(input == output, "wrapped xUDT must conserve units");
        Ok(())
    }

    fn digest(
        &self,
        anchor: &Byte32,
        tx: &TransactionView,
        proof: &Deposit,
        guardians: &Guardians,
    ) -> [u8; 32] {
        let mut hash = new_blake2b();
        if let Some(point) = guardians.point() {
            hash.update(b"CKB sidechain peg-in certificate v2\0");
            hash.update(point.as_slice());
            if let Some(backing) = &guardians.backing_anchor {
                hash.update(&[1]);
                hash.update(&backing.height.to_le_bytes());
                hash.update(backing.hash.as_bytes());
            } else {
                hash.update(&[0]);
            }
        } else {
            hash.update(b"CKB sidechain peg-in certificate v1\0");
        }
        hash.update(self.l1_genesis.as_bytes());
        hash.update(self.vault_type_hash.as_bytes());
        hash.update(anchor.as_slice());
        hash.update(tx.hash().as_slice());
        // The canonical packed transaction bytes and their block identities are
        // covered so a relayer cannot substitute a different confirmation receipt.
        hash.update(&blake2b_256(proof.deposit_tx.as_bytes()));
        hash.update(proof.deposit_block.as_bytes());
        hash.update(&proof.deposit_height.to_le_bytes());
        hash.update(&blake2b_256(proof.consolidation_tx.as_bytes()));
        hash.update(proof.consolidation_block.as_bytes());
        hash.update(&proof.consolidation_height.to_le_bytes());
        hash.update(proof.confirmed_block.as_bytes());
        hash.update(&proof.confirmed_height.to_le_bytes());
        let mut digest = [0; 32];
        hash.finalize(&mut digest);
        digest
    }
}

fn key(guardian: &Validator) -> Result<VerifyingKey> {
    Ok(VerifyingKey::from_bytes(
        guardian.ed25519.as_bytes().try_into()?,
    )?)
}

pub fn is_mint(tx: &TransactionView) -> bool {
    tx.inputs()
        .get(0)
        .is_some_and(|input| u64::from(input.since()) == PEG_IN)
}

pub fn is_burn(script: &Script) -> bool {
    script.code_hash().as_slice() == blake2b_256(b"CKB sidechain peg-out v1")
}

pub fn withdrawal_outputs(tx: &TransactionView) -> impl Iterator<Item = usize> {
    tx.outputs()
        .into_iter()
        .enumerate()
        .filter_map(|(index, output)| is_burn(&output.lock()).then_some(index))
}

pub fn burn_lock(anchor: &Byte32) -> Script {
    Script::new_builder()
        .code_hash(blake2b_256(b"CKB sidechain peg-out v1"))
        .hash_type(ScriptHashType::Data1)
        .args(anchor.as_slice().pack())
        .build()
}

pub fn deposit_memo(anchor: &Byte32, recipient: &Script) -> Bytes {
    let mut bytes = DEPOSIT_MEMO.to_vec();
    bytes.extend(anchor.as_slice());
    bytes.extend(recipient.as_slice());
    bytes.into()
}

pub fn payout_memo(recipient: &Script) -> Bytes {
    let mut bytes = PAYOUT_MEMO.to_vec();
    bytes.extend(recipient.as_slice());
    bytes.into()
}

pub fn xudt_burn_lock(anchor: &Byte32, recipient: &Script) -> Script {
    burn_lock(anchor)
        .as_builder()
        .args(Bytes::from(
            [anchor.as_slice(), recipient.as_slice()].concat(),
        ))
        .build()
}

pub fn withdrawal_id(vault_type: &H256, nonce: u64) -> Byte32 {
    let mut hash = new_blake2b();
    hash.update(b"CKB bridge withdrawal v1\0");
    hash.update(vault_type.as_bytes());
    hash.update(&nonce.to_le_bytes());
    let mut id = [0; 32];
    hash.finalize(&mut id);
    id.into()
}
