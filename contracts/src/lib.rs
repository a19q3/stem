//! Shared CKB syscalls for authenticating a direct checkpoint dependency.
#![no_std]

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, prelude::*},
    error::SysError,
    high_level::{load_cell_data, load_cell_type},
};
use stem_stake::{
    Hash32,
    finality::{CheckpointId, CheckpointPolicy, Tip},
};

pub fn load_checkpoint(
    id: &CheckpointId,
    ckb_genesis: &Hash32,
    network: &Hash32,
    chain_id: Option<&str>,
) -> Result<Option<Tip>, SysError> {
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
        if policy.ckb_genesis != *ckb_genesis
            || policy.trust.network != *network
            || chain_id.is_some_and(|chain| chain != policy.trust.chain_id)
        {
            return Ok(None);
        }
        let tip = Tip::decode(&load_cell_data(index, Source::CellDep)?)
            .map_err(|_| SysError::Encoding)?;
        return Ok(Some(tip));
    }
    unreachable!()
}
