//! Shared CKB syscalls for authenticating a direct checkpoint dependency.
#![no_std]

pub mod metatype;
pub mod metatype_custody;

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, prelude::*},
    error::SysError,
    high_level::{load_cell_data, load_cell_type},
};
#[cfg(not(feature = "jolteon"))]
use stem_stake::finality::{CheckpointPolicy, Tip};
#[cfg(feature = "jolteon")]
use stem_stake::jolteon::checkpoint::{Policy as CheckpointPolicy, Tip};
use stem_stake::{Hash32, finality::CheckpointId, l1::CheckpointState};

pub struct Checkpoint {
    pub state: CheckpointState,
    #[cfg(feature = "jolteon")]
    pub policy: CheckpointPolicy,
    #[cfg(feature = "jolteon")]
    pub tip: Tip,
}

pub fn load_checkpoint(
    id: &CheckpointId,
    ckb_genesis: &Hash32,
    network: &Hash32,
    _chain_id: Option<&str>,
) -> Result<Option<Checkpoint>, SysError> {
    for index in 0.. {
        let script = match load_cell_type(index, Source::CellDep) {
            Ok(Some(script)) => script,
            Ok(None) => continue,
            Err(SysError::IndexOutOfBound) => return Ok(None),
            Err(error) => return Err(error),
        };
        if script.code_hash().as_slice() != id.code_hash
            || u8::from(script.hash_type()) != ScriptHashType::Data2 as u8
        {
            continue;
        }
        let args = script.args().raw_data();
        if !args.starts_with(&id.type_id) {
            continue;
        }
        let policy = CheckpointPolicy::decode(&args).map_err(|_| SysError::Encoding)?;
        #[cfg(feature = "jolteon")]
        let matches_network = policy.network == *network;
        #[cfg(not(feature = "jolteon"))]
        let matches_network = policy.trust.network == *network
            && _chain_id.is_none_or(|chain| chain == policy.trust.chain_id);
        if policy.ckb_genesis != *ckb_genesis || !matches_network {
            return Ok(None);
        }
        let tip = Tip::decode(&load_cell_data(index, Source::CellDep)?)
            .map_err(|_| SysError::Encoding)?;
        return Ok(Some(Checkpoint {
            state: CheckpointState {
                height: tip.height(),
                system_root: tip.system_root(),
            },
            #[cfg(feature = "jolteon")]
            policy,
            #[cfg(feature = "jolteon")]
            tip,
        }));
    }
    unreachable!()
}
