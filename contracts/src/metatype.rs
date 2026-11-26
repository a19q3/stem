//! CKB host operations for Metatype and its independently deployed adapters.

extern crate alloc;

use alloc::{collections::BTreeMap, vec::Vec};

use ckb_std::{
    ckb_constants::Source,
    env,
    error::SysError,
    high_level::{decode_hex, load_cell_data_hash, load_cell_lock_hash, load_script_hash},
    syscalls::{load_cell_data, load_witness},
};
use stem_bridge_wire::metatype::{
    Invocation, MAX_INVOCATION_BYTES, MAX_MANIFEST_BYTES, MAX_MANIFEST_DEFINITION_BYTES,
    MAX_TRANSACTION_CELLS_PER_SIDE, ManifestReference, MetatypeManifest,
    witness::{MAGIC, MAX_WITNESS_BYTES},
};
use stem_stake::Hash32;

const MAX_DEPS: usize = 128;

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

pub fn witness_data(index: usize) -> Result<Option<Vec<u8>>, SysError> {
    let mut prefix = [0; MAGIC.len()];
    let length = match load_witness(&mut prefix, 0, index, Source::Input) {
        Ok(length) | Err(SysError::LengthNotEnough(length)) => length,
        Err(SysError::IndexOutOfBound) => return Ok(None),
        Err(error) => return Err(error),
    };
    if &prefix != MAGIC || length < MAGIC.len() {
        return Ok(None);
    }
    if length > MAX_WITNESS_BYTES {
        return Err(SysError::Encoding);
    }
    let mut data = alloc::vec![0; length];
    if load_witness(&mut data, 0, index, Source::Input)? != length {
        return Err(SysError::Encoding);
    }
    Ok(Some(data))
}

pub fn with_invocation(
    validate: impl FnOnce(Invocation<'_>, &mut ManifestResolver) -> Option<()>,
) -> i8 {
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
        validate(invocation, &mut ManifestResolver::default())
    };
    match result() {
        Some(()) => 0,
        None => 40,
    }
}

#[derive(Default)]
pub struct ManifestResolver {
    dependencies: Option<Vec<Hash32>>,
    definitions: BTreeMap<Hash32, Vec<u8>>,
}

impl ManifestResolver {
    pub fn dependency_hashes(&mut self) -> Result<&[Hash32], SysError> {
        if self.dependencies.is_none() {
            let mut hashes = Vec::new();
            for index in 0..=MAX_DEPS {
                match load_cell_data_hash(index, Source::CellDep) {
                    Ok(hash) if index < MAX_DEPS => hashes.push(hash),
                    Err(SysError::IndexOutOfBound) => break,
                    _ => return Err(SysError::Encoding),
                }
            }
            self.dependencies = Some(hashes);
        }
        Ok(self.dependencies.as_deref().expect("resolved Cell Deps"))
    }

    /// Return the same bounded inline view to the dispatcher and each adapter.
    pub fn load(&mut self, index: u16, source: Source, limit: usize) -> Option<Vec<u8>> {
        let limit = MAX_MANIFEST_BYTES.min(limit);
        let raw = bounded_data(usize::from(index), source, limit).ok()?;
        if raw.first() != Some(&0) {
            return Some(raw);
        }
        let reference = ManifestReference::decode(&raw)?;
        if !self.definitions.contains_key(&reference.definition_hash) {
            let index = self
                .dependency_hashes()
                .ok()?
                .iter()
                .position(|hash| *hash == reference.definition_hash)?;
            let definition =
                bounded_data(index, Source::CellDep, MAX_MANIFEST_DEFINITION_BYTES).ok()?;
            self.definitions
                .insert(reference.definition_hash, definition);
        }
        reference.expand(self.definitions.get(&reference.definition_hash)?, limit)
    }

    pub fn manifest_data(&mut self, index: u16, source: Source) -> Option<Vec<u8>> {
        self.load(index, source, MAX_MANIFEST_BYTES)
    }

    pub fn total(
        &mut self,
        indices: &[u16],
        source: Source,
        mut amount: impl FnMut(&MetatypeManifest<'_>, u16) -> Option<u128>,
    ) -> Option<u128> {
        indices.iter().try_fold(0u128, |sum, &index| {
            let data = self.manifest_data(index, source)?;
            let manifest = MetatypeManifest::decode(&data)?;
            let units = amount(&manifest, index)?;
            if units == 0 {
                return None;
            }
            sum.checked_add(units)
        })
    }
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
