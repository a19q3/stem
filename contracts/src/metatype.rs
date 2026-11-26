//! CKB host operations for Metatype and its independently deployed adapters.

extern crate alloc;

use alloc::vec::Vec;

use ckb_std::{
    ckb_constants::Source,
    env,
    error::SysError,
    high_level::{decode_hex, load_cell_lock_hash, load_script_hash},
    syscalls::load_cell_data,
};
use stem_bridge_wire::metatype::{
    Invocation, MAX_CELLS_PER_SIDE, MAX_MANIFEST_BYTES, MAX_SCRIPT_BYTES,
    MAX_TRANSACTION_CELLS_PER_SIDE, MetatypeManifest,
};
use stem_stake::Hash32;

pub fn bounded_data(index: usize, source: Source, limit: usize) -> Result<Vec<u8>, SysError> {
    let length = match load_cell_data(&mut [], 0, index, source) {
        Ok(length) | Err(SysError::LengthNotEnough(length)) if length <= limit => length,
        Ok(_) | Err(SysError::LengthNotEnough(_)) => return Err(SysError::Encoding),
        Err(error) => return Err(error),
    };
    let mut data = alloc::vec![0; length];
    if load_cell_data(&mut data, 0, index, source)? != length {
        return Err(SysError::Encoding);
    }
    Ok(data)
}

pub fn with_invocation(validate: impl FnOnce(Invocation<'_>) -> Option<()>) -> i8 {
    const MAX_INVOCATION_BYTES: usize =
        32 + 2 + MAX_SCRIPT_BYTES + 2 * (1 + 2 * MAX_CELLS_PER_SIDE);
    let result = || {
        let [argument] = env::argv() else { return None };
        if argument.to_bytes().len() > 2 * MAX_INVOCATION_BYTES {
            return None;
        }
        let raw = decode_hex(argument).ok()?;
        let invocation = Invocation::decode(&raw)?;
        if load_script_hash().ok()? != *invocation.deployment_type {
            return None;
        }
        validate(invocation)
    };
    match result() {
        Some(()) => 0,
        None => 40,
    }
}

pub fn manifest_data(index: u16, source: Source) -> Option<Vec<u8>> {
    bounded_data(usize::from(index), source, MAX_MANIFEST_BYTES).ok()
}

pub fn total(
    indices: &[u16],
    source: Source,
    mut amount: impl FnMut(&MetatypeManifest<'_>, u16) -> Option<u128>,
) -> Option<u128> {
    indices.iter().try_fold(0u128, |sum, &index| {
        let data = manifest_data(index, source)?;
        let manifest = MetatypeManifest::decode(&data)?;
        let units = amount(&manifest, index)?;
        if units == 0 {
            return None;
        }
        sum.checked_add(units)
    })
}

pub fn authorized(authority: &Hash32) -> bool {
    for index in 0..MAX_TRANSACTION_CELLS_PER_SIDE {
        match load_cell_lock_hash(index, Source::Input) {
            Ok(hash) if hash == *authority => return true,
            Ok(_) => (),
            Err(_) => return false,
        }
    }
    false
}
