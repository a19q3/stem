use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use ckb_jsonrpc_types::JsonBytes;
use ckb_types::{
    core::{TransactionView, cell::ResolvedTransaction},
    packed::{CellOutput, OutPoint},
};
use ed25519_dalek::VerifyingKey;
use tendermint_proto::v0_38::{
    abci::ValidatorUpdate,
    crypto::{PublicKey, public_key::Sum},
};

use crate::{
    bridge,
    genesis::{Validator, validate_committee},
};

const TAG: &[u8] = b"CKB-GOV1";
const GUARDIAN_TAG: &[u8] = b"CKB-BRG1";
const MEMBER_BYTES: usize = 40;

pub fn decode(data: &[u8]) -> Result<Vec<Validator>> {
    decode_members(data, TAG)
}

pub fn decode_guardians(data: &[u8]) -> Result<Vec<Validator>> {
    decode_members(data, GUARDIAN_TAG)
}

fn decode_members(data: &[u8], tag: &[u8]) -> Result<Vec<Validator>> {
    let body = data
        .strip_prefix(tag)
        .context("invalid governance data version")?;
    ensure!(
        !body.is_empty() && body.len() <= 256 * MEMBER_BYTES && body.len() % MEMBER_BYTES == 0,
        "invalid governance committee length"
    );
    let members = body
        .chunks_exact(MEMBER_BYTES)
        .map(|entry| {
            let bytes: [u8; 32] = entry[..32].try_into()?;
            let key = VerifyingKey::from_bytes(&bytes)?;
            ensure!(!key.is_weak(), "weak validator key");
            Ok(Validator {
                ed25519: JsonBytes::from_vec(bytes.to_vec()),
                power: i64::from_le_bytes(entry[32..].try_into()?),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    ensure!(
        members
            .windows(2)
            .all(|pair| pair[0].ed25519.as_bytes() < pair[1].ed25519.as_bytes()),
        "governance committee keys must be sorted and unique"
    );
    validate_committee(&members)?;
    Ok(members)
}

pub fn committee(members: &[Validator]) -> BTreeMap<Vec<u8>, i64> {
    members
        .iter()
        .map(|v| (v.ed25519.as_bytes().to_vec(), v.power))
        .collect()
}

pub fn encode(members: &[Validator]) -> Result<Vec<u8>> {
    encode_members(members, TAG)
}

pub fn encode_guardians(members: &[Validator]) -> Result<Vec<u8>> {
    encode_members(members, GUARDIAN_TAG)
}

fn encode_members(members: &[Validator], tag: &[u8]) -> Result<Vec<u8>> {
    validate_committee(members)?;
    let mut data = tag.to_vec();
    for (key, power) in committee(members) {
        data.extend(key);
        data.extend(power.to_le_bytes());
    }
    decode_members(&data, tag)?;
    Ok(data)
}

pub fn guard(
    tx: &ResolvedTransaction,
    committee: Option<&OutPoint>,
    guardians: Option<&OutPoint>,
) -> Result<()> {
    let required = |point: &OutPoint| {
        tx.resolved_inputs
            .iter()
            .find(|cell| &cell.out_point == point)
    };
    let validator = committee.and_then(required);
    let guardian = guardians.and_then(required);
    ensure!(
        validator.is_none() || guardian.is_none(),
        "validator and guardian governance require separate transactions"
    );
    let Some((previous, tag)) = validator
        .map(|cell| (cell, TAG))
        .or_else(|| guardian.map(|cell| (cell, GUARDIAN_TAG)))
    else {
        return Ok(());
    };
    // The old CKB lock authorizes the update. Its named successor is output
    // zero and must retain the full governance capacity, including on a bridged chain.
    let next = tx
        .transaction
        .output(0)
        .context("missing governance successor")?;
    validate_cell(&next)?;
    ensure!(
        next.capacity() == previous.cell_output.capacity(),
        "governance capacity is reserved"
    );
    decode_members(&successor_data(&tx.transaction)?, tag)?;
    Ok(())
}

pub fn successor_point(tx: &TransactionView, current: Option<&OutPoint>) -> Option<OutPoint> {
    current
        .filter(|point| !bridge::is_mint(tx) && tx.input_pts_iter().any(|input| &input == *point))
        .map(|_| OutPoint::new(tx.hash(), 0))
}

pub fn validate_cell(output: &CellOutput) -> Result<()> {
    ensure!(
        output.type_().is_none()
            && !bridge::is_burn(&output.lock())
            && output.lock() != *crate::genesis::SYSTEM_LOCK,
        "governance cell requires a spendable native CKB lock"
    );
    Ok(())
}

pub fn successor_data(tx: &TransactionView) -> Result<ckb_types::bytes::Bytes> {
    Ok(tx
        .outputs_data()
        .get(0)
        .context("missing governance data")?
        .raw_data())
}

pub fn updates(old: &[Validator], new: &[Validator]) -> Vec<ValidatorUpdate> {
    let mut previous = committee(old);
    let mut changed = Vec::new();
    for (key, power) in committee(new) {
        if previous.remove(&key) != Some(power) {
            changed.push(update(key, power));
        }
    }
    changed.extend(previous.into_keys().map(|key| update(key, 0)));
    changed
}

fn update(key: Vec<u8>, power: i64) -> ValidatorUpdate {
    ValidatorUpdate {
        pub_key: Some(PublicKey {
            sum: Some(Sum::Ed25519(key)),
        }),
        power,
    }
}
