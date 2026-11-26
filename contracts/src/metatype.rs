//! CKB host operations for Metatype and its independently deployed adapters.

extern crate alloc;

use alloc::{collections::BTreeMap, rc::Rc, vec::Vec};

use ckb_std::{
    ckb_constants::Source,
    env,
    error::SysError,
    high_level::{
        decode_hex, load_cell_data_hash, load_cell_lock_hash, load_cell_type_hash, load_script_hash,
    },
    syscalls::{load_cell_data, load_witness},
};
use ed25519_dalek::{Signature, VerifyingKey};
use stem_bridge_wire::metatype::{
    Invocation, MAX_CELLS_PER_SIDE, MAX_INVOCATION_BYTES, MAX_MANIFEST_BYTES,
    MAX_MANIFEST_DEFINITION_BYTES, MAX_TRANSACTION_CELLS_PER_SIDE, ManifestCell,
    ManifestDefinition, ManifestReference, MetatypeManifest,
    witness::{MAGIC, MAX_WITNESS_BYTES},
};
use stem_stake::Hash32;

const MAX_DEPS: usize = 128;

pub fn workflow_key(key: &Hash32) -> Option<VerifyingKey> {
    let key = VerifyingKey::from_bytes(key).ok()?;
    (!key.is_weak()).then_some(key)
}

pub fn verify_workflow_signature(key: &Hash32, digest: &Hash32, signature: &[u8; 64]) -> bool {
    workflow_key(key).is_some_and(|key| {
        key.verify_strict(digest, &Signature::from_bytes(signature))
            .is_ok()
    })
}

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

pub fn typed_dependency_data(hash: &Hash32, max_bytes: usize) -> Option<Vec<u8>> {
    let mut found = None;
    for index in 0..=MAX_DEPS {
        match load_cell_type_hash(index, Source::CellDep) {
            Ok(current) if index < MAX_DEPS => {
                if current.as_ref() == Some(hash) && found.replace(index).is_some() {
                    return None;
                }
            }
            Err(SysError::IndexOutOfBound) => {
                return bounded_data(found?, Source::CellDep, max_bytes).ok();
            }
            _ => return None,
        }
    }
    None
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
    definitions: BTreeMap<Hash32, Rc<ManifestDefinition>>,
}

pub struct CellData {
    raw: Vec<u8>,
    definition: Option<Rc<ManifestDefinition>>,
    expanded_len: usize,
}

impl CellData {
    pub fn view(&self) -> Option<MetatypeManifest<'_>> {
        match self.definition.as_deref() {
            Some(definition) => {
                ManifestReference::decode(&self.raw)?.view(definition, self.expanded_len)
            }
            None => MetatypeManifest::decode(&self.raw),
        }
    }
}

pub type Cells = Vec<(u16, CellData)>;

pub fn views(cells: &Cells) -> Option<Vec<ManifestCell<'_>>> {
    cells
        .iter()
        .map(|(index, data)| Some((*index, data.view()?)))
        .collect()
}

impl ManifestResolver {
    pub fn cells(
        &mut self,
        source: Source,
        own_type: &Hash32,
        remaining: &mut usize,
    ) -> Result<(usize, Cells), i8> {
        let mut matched = Vec::new();
        for index in 0..=MAX_TRANSACTION_CELLS_PER_SIDE {
            match load_cell_type_hash(index, source) {
                Ok(ty) if index < MAX_TRANSACTION_CELLS_PER_SIDE => {
                    if ty.as_ref() == Some(own_type) {
                        if matched.len() == MAX_CELLS_PER_SIDE {
                            return Err(43);
                        }
                        let data = self.load(index as u16, source, *remaining).ok_or(43)?;
                        *remaining -= data.expanded_len;
                        matched.push((index as u16, data));
                    }
                }
                Err(SysError::IndexOutOfBound) => return Ok((index, matched)),
                _ => return Err(43),
            }
        }
        unreachable!("bounded Cell enumeration")
    }

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

    pub fn dependency_data(&mut self, hash: &Hash32, max_bytes: usize) -> Option<Vec<u8>> {
        let index = self
            .dependency_hashes()
            .ok()?
            .iter()
            .position(|item| item == hash)?;
        bounded_data(index, Source::CellDep, max_bytes).ok()
    }

    fn definition(&mut self, hash: Hash32) -> Option<Rc<ManifestDefinition>> {
        if let Some(definition) = self.definitions.get(&hash) {
            return Some(Rc::clone(definition));
        }
        let raw = self.dependency_data(&hash, MAX_MANIFEST_DEFINITION_BYTES)?;
        let definition = Rc::new(ManifestDefinition::decode(raw)?);
        self.definitions.insert(hash, Rc::clone(&definition));
        Some(definition)
    }

    fn load(&mut self, index: u16, source: Source, limit: usize) -> Option<CellData> {
        let limit = MAX_MANIFEST_BYTES.min(limit);
        let raw = bounded_data(usize::from(index), source, limit).ok()?;
        if raw.first() != Some(&0) {
            return Some(CellData {
                expanded_len: raw.len(),
                raw,
                definition: None,
            });
        }
        let reference = ManifestReference::decode(&raw)?;
        let definition = self.definition(reference.definition_hash)?;
        let expanded_len = reference.expanded_len(&definition, limit)?;
        Some(CellData {
            raw,
            definition: Some(definition),
            expanded_len,
        })
    }

    pub fn manifest_data(&mut self, index: u16, source: Source) -> Option<CellData> {
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
            let manifest = data.view()?;
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
