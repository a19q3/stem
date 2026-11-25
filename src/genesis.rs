use std::{
    collections::HashSet,
    sync::{Arc, LazyLock},
};

use anyhow::{Context, Result, ensure};
use ckb_chain_spec::{
    ChainSpec,
    consensus::{Consensus, ConsensusBuilder},
};
use ckb_hash::blake2b_256;
use ckb_jsonrpc_types::JsonBytes;
use ckb_resource::{Resource, SPEC_DEV_FILE_NAME};
use ckb_types::{
    core::{
        Capacity, EpochNumberWithFraction, HeaderBuilder, HeaderView, ScriptHashType,
        TransactionView, hardfork::HardForks,
    },
    packed::{Byte32, CellOutput, OutPoint, Script, Transaction},
    prelude::*,
    utilities::merkle_root,
};
use serde::{Deserialize, Serialize};

use crate::{
    bridge::{self, Bridge},
    governance,
};

pub const MAX_GENESIS_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TX_BYTES: usize = 512 * 1024;
pub const MAX_BLOCK_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_BLOCK_TXS: usize = 4096;
pub const MAX_TX_CYCLES: u64 = 200_000_000;
pub const MAX_BLOCK_CYCLES: u64 = 30_000_000_000;

pub static SYSTEM_LOCK: LazyLock<Script> = LazyLock::new(|| {
    Script::new_builder()
        .code_hash(blake2b_256(b"CKB sidechain immutable genesis cells v1"))
        .hash_type(ScriptHashType::Data)
        .build()
});

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Genesis {
    pub chain_id: String,
    pub timestamp_ms: u64,
    pub epoch_length: u16,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub checkpoint_interval: u64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checkpoint_cell_proofs: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checkpoint_compact_transactions: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub checkpoint_live_cell_replay: bool,
    pub validators: Vec<Validator>,
    pub allocations: Vec<JsonBytes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<Bridge>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub authenticate_withdrawals: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub governance: Option<ckb_jsonrpc_types::OutPoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guardian_governance: Option<ckb_jsonrpc_types::OutPoint>,
}

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Validator {
    pub ed25519: JsonBytes,
    pub power: i64,
}

impl Genesis {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= MAX_GENESIS_BYTES,
            "genesis exceeds size limit"
        );
        Ok(serde_json::from_slice(bytes)?)
    }

    pub fn validate(&self) -> Result<(HeaderView, Vec<TransactionView>)> {
        ensure!(
            !self.authenticate_withdrawals || self.bridge.is_some(),
            "authenticated withdrawals require a bridge"
        );
        ensure!(
            self.guardian_governance.is_none() || self.bridge.is_some(),
            "guardian governance requires a bridge"
        );
        ensure!(
            self.guardian_governance.is_none() || self.guardian_governance != self.governance,
            "validator and guardian governance require separate Cells"
        );
        ensure!(
            self.chain_id.len() <= 50
                && !self.chain_id.is_empty()
                && self
                    .chain_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c)),
            "invalid chain ID"
        );
        ensure!(self.epoch_length > 0, "epoch length must be positive");
        ensure!(
            self.checkpoint_interval == 0 || self.checkpoint_interval >= 4,
            "checkpoints require a minimum interval of four blocks"
        );
        ensure!(
            !self.checkpoint_cell_proofs || self.checkpoint_interval > 0,
            "Live Cell proofs require periodic checkpoints"
        );
        ensure!(
            !self.checkpoint_compact_transactions || self.checkpoint_cell_proofs,
            "compact checkpoints require Live Cell proofs"
        );
        ensure!(
            !self.checkpoint_live_cell_replay || self.checkpoint_compact_transactions,
            "Live Cell replay requires compact checkpoints"
        );
        validate_committee(&self.validators)?;
        if let Some(bridge) = &self.bridge {
            bridge.validate()?;
        }
        let mut hashes = HashSet::new();
        let mut capacity = Capacity::zero();
        let mut allocations = Vec::with_capacity(self.allocations.len());
        let asset = self.bridge.as_ref().and_then(|bridge| bridge.xudt.as_ref());
        let wrapped = asset.map(|asset| asset.sidechain_type());
        let mut found_xudt_program = false;
        let governance = self.governance.clone().map(OutPoint::from);
        let guardian_governance = self.guardian_governance.clone().map(OutPoint::from);
        let mut found_governance = false;
        let mut found_guardian_governance = false;
        for entry in &self.allocations {
            let tx = Transaction::from_slice(entry.as_bytes())?.into_view();
            ensure!(
                tx.inputs().is_empty()
                    && tx.cell_deps().is_empty()
                    && tx.header_deps_iter().eq([self.allocation_tag()])
                    && tx.witnesses().is_empty()
                    && tx.version() == 0
                    && !tx.outputs().is_empty()
                    && tx.outputs().len() == tx.outputs_data().len(),
                "invalid genesis allocation"
            );
            ensure!(hashes.insert(tx.hash()), "duplicate genesis transaction");
            for (index, (output, data)) in tx.outputs_with_data_iter().enumerate() {
                ensure!(
                    !bridge::is_burn(&output.lock()),
                    "genesis cannot include peg-out burns"
                );
                if let Some(asset) = asset {
                    ensure!(
                        output
                            .type_()
                            .to_opt()
                            .as_ref()
                            .is_none_or(|script| Some(script) != wrapped.as_ref()),
                        "genesis cannot issue wrapped xUDT"
                    );
                    if output.lock() == *SYSTEM_LOCK
                        && output.type_().is_none()
                        && CellOutput::calc_data_hash(&data).as_slice()
                            == asset.l1_type.code_hash.as_bytes()
                    {
                        found_xudt_program = true;
                    }
                }
                let names_output = |point: &OutPoint| {
                    point.tx_hash() == tx.hash() && u32::from(point.index()) as usize == index
                };
                let reserved = governance.as_ref().is_some_and(names_output);
                let guardian_reserved = guardian_governance.as_ref().is_some_and(names_output);
                if reserved {
                    governance::validate_cell(&output)?;
                    let members = governance::decode(&data)?;
                    ensure!(
                        governance::committee(&members) == governance::committee(&self.validators),
                        "genesis governance cell must name the genesis committee"
                    );
                    found_governance = true;
                }
                if guardian_reserved {
                    governance::validate_cell(&output)?;
                    let members = governance::decode_guardians(&data)?;
                    let bridge = self
                        .bridge
                        .as_ref()
                        .context("guardian governance requires a bridge")?;
                    ensure!(
                        governance::committee(&members) == governance::committee(&bridge.guardians),
                        "genesis guardian Cell must name the genesis guardians"
                    );
                    found_guardian_governance = true;
                }
                ensure!(
                    self.bridge.is_none()
                        || reserved
                        || guardian_reserved
                        || output.lock() == *SYSTEM_LOCK,
                    "bridged chains cannot bootstrap transferable capacity"
                );
                ensure!(
                    !output.is_lack_of_capacity(Capacity::bytes(data.len())?)?,
                    "insufficient genesis cell capacity"
                );
                capacity = capacity.safe_add::<Capacity>(output.capacity().unpack())?;
            }
            allocations.push(tx);
        }
        ensure!(
            governance.is_none() || found_governance,
            "genesis governance outpoint is missing"
        );
        ensure!(
            guardian_governance.is_none() || found_guardian_governance,
            "genesis guardian governance outpoint is missing"
        );
        ensure!(
            asset.is_none() || found_xudt_program,
            "missing immutable xUDT program in genesis"
        );
        // The format tag commits genesis to this execution rule set and database schema.
        let mut spec = b"CKB sidechain genesis v1\0".to_vec();
        spec.extend(serde_json::to_vec(self)?);
        let domain: Byte32 = blake2b_256(spec).into();
        let header = header(0, self.timestamp_ms, self.epoch_length)
            .extra_hash(domain)
            .transactions_root(transactions_root(&allocations))
            .build();
        Ok((header, allocations))
    }

    pub fn allocation_tag(&self) -> Byte32 {
        let mut tag = b"CKB sidechain allocations v1\0".to_vec();
        tag.extend(self.chain_id.as_bytes());
        tag.extend(self.timestamp_ms.to_le_bytes());
        blake2b_256(tag).into()
    }

    pub fn checkpoint(&self, height: u64) -> bool {
        height > 0
            && self.checkpoint_interval > 0
            && height.is_multiple_of(self.checkpoint_interval)
    }

    pub fn checkpoint_format(&self) -> u8 {
        if self.checkpoint_live_cell_replay {
            4
        } else if self.checkpoint_compact_transactions {
            3
        } else if self.checkpoint_cell_proofs {
            2
        } else {
            1
        }
    }
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

pub fn validate_committee(committee: &[Validator]) -> Result<()> {
    ensure!(
        (1..=256).contains(&committee.len()),
        "invalid committee size"
    );
    let mut public_keys = HashSet::new();
    let mut power = 0i64;
    for validator in committee {
        let pk = validator.ed25519.as_bytes();
        ensure!(
            pk.len() == 32 && validator.power > 0 && public_keys.insert(pk),
            "invalid or duplicate validator"
        );
        power = power
            .checked_add(validator.power)
            .context("validator power overflow")?;
    }
    ensure!(
        power <= i64::MAX / 8,
        "committee power exceeds CometBFT limit"
    );
    Ok(())
}

pub fn header(height: u64, timestamp: u64, epoch_length: u16) -> HeaderBuilder {
    let length = u64::from(epoch_length);
    HeaderBuilder::default()
        .number(height)
        .timestamp(timestamp)
        .epoch(EpochNumberWithFraction::new(
            height / length,
            height % length,
            length,
        ))
}

pub fn transactions_root(txs: &[TransactionView]) -> Byte32 {
    let raw: Vec<_> = txs.iter().map(TransactionView::hash).collect();
    let witnesses: Vec<_> = txs.iter().map(TransactionView::witness_hash).collect();
    merkle_root(&[merkle_root(&raw), merkle_root(&witnesses)])
}

pub fn script_consensus() -> Result<Arc<Consensus>> {
    let dev = ChainSpec::load_from(&Resource::bundled(SPEC_DEV_FILE_NAME.to_owned()))
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("load the bundled CKB VM rules")?;
    let genesis = dev
        .build_genesis()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(Arc::new(
        ConsensusBuilder::default()
            .genesis_block(genesis)
            .hardfork_switch(HardForks::new_dev())
            .max_block_bytes(MAX_TX_BYTES as u64 + 4)
            .build(),
    ))
}
