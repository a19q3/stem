//! Native system records whose byte layouts predate the Strand schema.
//! The optional bridge and Keeper tails keep the exact historical lengths.
use super::{Limits, byte32, bytes, uint64};
use crate::{
    stake_proof::encode_members,
    strand::prelude::{Builder, Entity, Reader},
    wire,
};
use anyhow::{Context, Result, ensure};
use ckb_types::{
    bytes::Bytes,
    packed::{Byte32, Script},
    prelude::Entity as CkbEntity,
};
use stem_stake::{Member, StakeConfig};

fn u16_field(value: u16) -> wire::Uint16 {
    wire::Uint16::new_unchecked(Bytes::copy_from_slice(&value.to_le_bytes()))
}
fn u32_field(value: u32) -> wire::Uint32 {
    wire::Uint32::new_unchecked(Bytes::copy_from_slice(&value.to_le_bytes()))
}

fn budget(limits: Limits) -> wire::ControlBudget {
    wire::ControlBudget::new_builder()
        .operations(u32_field(limits.operations))
        .bytes(uint64(limits.bytes))
        .signatures(u32_field(limits.signatures))
        .build()
}

pub struct BridgeConfig<'a> {
    pub l1_genesis: &'a Byte32,
    pub guardian_generation: &'a Byte32,
    pub wrapped_code: &'a Byte32,
    pub burn_code: &'a Byte32,
    pub budgets: [Limits; 3],
    pub confirmations: u64,
    pub deposits_per_block: u32,
    pub guardians: &'a [Member],
    pub lanes: &'a [Byte32],
    pub recipients: &'a [(Byte32, u8, u32)],
    pub reserved: &'a [Script],
    pub mailbox: Option<&'a Byte32>,
    pub checkpoint_fund: Option<&'a Byte32>,
}

const FEE_FUND_TAG: &[u8] = b"Stem checkpoint fee fund\0";

impl BridgeConfig<'_> {
    pub fn encode(&self) -> Result<Vec<u8>> {
        for count in [
            self.guardians.len(),
            self.lanes.len(),
            self.recipients.len(),
            self.reserved.len(),
        ] {
            u32::try_from(count)?;
        }
        let mut guardians = Vec::new();
        wire::StakeMembers::write_count(self.guardians.len(), &mut guardians)?;
        guardians.extend(encode_members(self.guardians));
        let recipients = self
            .recipients
            .iter()
            .map(|(code, ty, args_len)| {
                wire::BridgeRecipientTemplate::new_builder()
                    .code_hash(byte32(code))
                    .hash_type((*ty).into())
                    .args_len(u32_field(*args_len))
                    .build()
            })
            .collect();
        let reserved = self
            .reserved
            .iter()
            .map(|lock| {
                u32::try_from(lock.as_slice().len())?;
                Ok(bytes(lock.as_bytes()))
            })
            .collect::<Result<_>>()?;
        let [mint, settlement, anchor] = self.budgets;
        let core = wire::BridgeGenesisConfig::new_builder()
            .l1_genesis(byte32(self.l1_genesis))
            .guardian_generation(byte32(self.guardian_generation))
            .wrapped_code(byte32(self.wrapped_code))
            .burn_code(byte32(self.burn_code))
            .mint(budget(mint))
            .settlement(budget(settlement))
            .anchor(budget(anchor))
            .confirmations(uint64(self.confirmations))
            .deposits_per_block(u32_field(self.deposits_per_block))
            .guardians(wire::StakeMembers::from_slice(&guardians)?)
            .lanes(
                wire::Byte32Vec::new_builder()
                    .set(self.lanes.iter().map(byte32).collect())
                    .build(),
            )
            .recipients(
                wire::BridgeRecipientTemplates::new_builder()
                    .set(recipients)
                    .build(),
            )
            .reserved(
                wire::ReservedLockScripts::new_builder()
                    .set(reserved)
                    .build(),
            )
            .build();
        let mut value = core.as_slice().to_vec();
        if let Some(mailbox) = self.mailbox {
            value.extend_from_slice(mailbox.as_slice());
        }
        if let Some(fund) = self.checkpoint_fund {
            value.extend_from_slice(FEE_FUND_TAG);
            value.extend_from_slice(fund.as_slice());
        }
        Ok(value)
    }
}

pub fn stake_config(
    config: &StakeConfig,
    window: u64,
    limits: Limits,
    guardians: &[Member],
) -> Result<Vec<u8>> {
    let prefix = wire::StakeGenesisParameters::new_builder()
        .minimum_bond(uint64(config.minimum_bond))
        .shannons_per_power(uint64(config.shannons_per_power))
        .max_validators(uint64(config.max_validators.try_into()?))
        .max_total_power(uint64(config.max_total_power))
        .blocks_per_epoch(uint64(config.blocks_per_epoch))
        .signing_window(uint64(window))
        .limit_bytes(uint64(limits.bytes))
        .limit_operations(uint64(limits.operations.into()))
        .limit_signatures(uint64(limits.signatures.into()))
        .build();
    let mut record = prefix.as_slice().to_vec();
    record.extend(encode_members(guardians));
    Ok(record)
}

pub fn keeper_policy(
    max_delay: u16,
    per_height: u16,
    fee: u64,
    time: Option<(u16, u64)>,
) -> Vec<u8> {
    let base = wire::KeeperHeightPolicy::new_builder()
        .max_delay(u16_field(max_delay))
        .per_height(u16_field(per_height))
        .reservation_fee(uint64(fee))
        .build();
    let mut value = base.as_slice().to_vec();
    if let Some((per_height, max_delay_ms)) = time {
        let tail = wire::KeeperTimePolicy::new_builder()
            .per_height(u16_field(per_height))
            .max_delay_ms(uint64(max_delay_ms))
            .build();
        value.extend_from_slice(tail.as_slice());
    }
    value
}

/// Split only the fixed Strand prefix and the original unprefixed chain name and committee tail.
pub fn network_parameters(
    limits: &[u8],
    fees: &[u8],
    bond: u64,
    overhead: u64,
    chain_id: &str,
    validators: &[([u8; 32], i64)],
) -> Result<Vec<u8>> {
    let prefix = wire::NetworkParameters::new_builder()
        .limits(wire::GenesisLimits::from_slice(limits)?)
        .fees(wire::GenesisFeeRates::from_slice(fees)?)
        .bond_per_byte(uint64(bond))
        .state_overhead(uint64(overhead))
        .chain_id_len(
            u8::try_from(chain_id.len())
                .context("chain id exceeds its genesis field")?
                .into(),
        )
        .build();
    let mut value = prefix.as_slice().to_vec();
    value.extend_from_slice(chain_id.as_bytes());
    for (key, power) in validators {
        value.extend_from_slice(key);
        value.extend_from_slice(&power.to_le_bytes());
    }
    Ok(value)
}

/// Check a proof's bridge policy record, including the two optional historical suffixes.
pub fn verify_bridge_config(value: &[u8]) -> Result<()> {
    let end = wire::BridgeGenesisConfigReader::span(value)?;
    let core = wire::BridgeGenesisConfigReader::from_slice(&value[..end])?;
    for raw in core.reserved().iter() {
        Script::from_slice(raw.raw_data())?;
    }
    let tail = &value[end..];
    let fee_size = FEE_FUND_TAG.len() + 32;
    let tail = if tail.len() == 32 || tail.len() == 32 + fee_size {
        &tail[32..]
    } else {
        tail
    };
    ensure!(
        tail.is_empty() || (tail.len() == fee_size && tail.starts_with(FEE_FUND_TAG)),
        "invalid bridge policy extension"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ckb_types::{
        core::ScriptHashType,
        prelude::{Builder as CkbBuilder, Pack},
    };

    #[test]
    fn bridge_policy_preserves_the_original_genesis_commitment_bytes() -> Result<()> {
        let hashes: Vec<Byte32> = (1..=8).map(|n| [n; 32].pack()).collect();
        let members = [Member {
            key: [7; 32],
            power: 123,
        }];
        let recipients = [(hashes[5].clone(), 4, 20)];
        let reserved = [Script::new_builder()
            .code_hash(hashes[6].clone())
            .hash_type(ScriptHashType::Type)
            .args(Bytes::from_static(b"receiver"))
            .build()];
        let budget = Limits {
            operations: 3,
            bytes: 32_768,
            signatures: 4,
        };
        let mut base = Vec::new();
        for hash in &hashes[..4] {
            base.extend_from_slice(hash.as_slice());
        }
        for _ in 0..3 {
            base.extend(3u32.to_le_bytes());
            base.extend(32_768u64.to_le_bytes());
            base.extend(4u32.to_le_bytes());
        }
        base.extend(12u64.to_le_bytes());
        base.extend(5u32.to_le_bytes());
        base.extend(1u32.to_le_bytes());
        base.extend([7; 32]);
        base.extend(123u64.to_le_bytes());
        base.extend(2u32.to_le_bytes());
        base.extend([4; 32]);
        base.extend([5; 32]);
        base.extend(1u32.to_le_bytes());
        base.extend([6; 32]);
        base.push(4);
        base.extend(20u32.to_le_bytes());
        base.extend(1u32.to_le_bytes());
        base.extend((reserved[0].as_slice().len() as u32).to_le_bytes());
        base.extend(reserved[0].as_slice());
        for mailbox in [None, Some(&hashes[6])] {
            for fund in [None, Some(&hashes[7])] {
                let mut expected = base.clone();
                if let Some(hash) = mailbox {
                    expected.extend(hash.as_slice());
                }
                if let Some(hash) = fund {
                    expected.extend(b"Stem checkpoint fee fund\0");
                    expected.extend(hash.as_slice());
                }
                let policy = BridgeConfig {
                    l1_genesis: &hashes[0],
                    guardian_generation: &hashes[1],
                    wrapped_code: &hashes[2],
                    burn_code: &hashes[3],
                    budgets: [budget; 3],
                    confirmations: 12,
                    deposits_per_block: 5,
                    guardians: &members,
                    lanes: &hashes[3..5],
                    recipients: &recipients,
                    reserved: &reserved,
                    mailbox,
                    checkpoint_fund: fund,
                };
                let encoded = policy.encode()?;
                assert_eq!(encoded, expected);
                verify_bridge_config(&expected)?;
                expected.push(0);
                assert!(verify_bridge_config(&expected).is_err());
            }
        }
        Ok(())
    }
}
