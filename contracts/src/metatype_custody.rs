//! Shared CKB resolution for the configured Metatype deployment and mailbox.

use ckb_std::{
    ckb_constants::Source,
    ckb_types::{core::ScriptHashType, packed::Script, prelude::*},
    error::SysError,
    high_level::{load_cell_lock, load_cell_type, load_cell_type_hash},
};
use stem_bridge_wire::{
    intercell::MailboxPolicy,
    metatype::{
        MAX_TRANSACTION_CELLS_PER_SIDE,
        custody::{Custody, Gate},
    },
};
use stem_stake::Hash32;

pub fn mailbox_policy(script: &Script, custody: &Custody) -> Option<MailboxPolicy> {
    if script.code_hash().as_slice() != custody.mailbox_code
        || u8::from(script.hash_type()) != ScriptHashType::Data2 as u8
    {
        return None;
    }
    let policy = MailboxPolicy::decode(&script.args().raw_data())?;
    (policy.type_id == custody.mailbox_id).then_some(policy)
}

pub fn find_unique(source: Source, target: &Hash32) -> Result<Option<usize>, i8> {
    let mut index = None;
    for i in 0..=MAX_TRANSACTION_CELLS_PER_SIDE {
        match load_cell_type_hash(i, source) {
            Ok(ty) if i < MAX_TRANSACTION_CELLS_PER_SIDE => {
                if ty.as_ref() == Some(target) && index.replace(i).is_some() {
                    return Err(48);
                }
            }
            Err(SysError::IndexOutOfBound) => return Ok(index),
            _ => return Err(48),
        }
    }
    unreachable!("bounded Cell enumeration")
}

/// A boundary has to consume the unique configured Intercell Mailbox
/// under the approved Gate. Its independent Lock and Type both execute.
pub fn require_gate(custody: &Custody, physical_type: &Hash32, genesis: &Hash32) -> Result<(), i8> {
    for index in 0..MAX_TRANSACTION_CELLS_PER_SIDE {
        let script = match load_cell_type(index, Source::Input) {
            Ok(Some(script)) => script,
            Ok(None) => continue,
            Err(_) => return Err(48),
        };
        let Some(policy) = mailbox_policy(&script, custody) else {
            continue;
        };
        let lock = load_cell_lock(index, Source::Input).map_err(|_| 48)?;
        if lock.code_hash().as_slice() != custody.guard_code
            || u8::from(lock.hash_type()) != ScriptHashType::Data2 as u8
            || lock.calc_script_hash().as_slice() != policy.owner
            || &policy.ckb_genesis != genesis
        {
            return Err(48);
        }
        let gate = Gate::decode(&lock.args().raw_data(), custody.guard_code).ok_or(48)?;
        return if &gate.custody == custody && &gate.metatype == physical_type {
            Ok(())
        } else {
            Err(48)
        };
    }
    Err(48)
}
