//! CKB-VM host for Stem's native Telegraph ledger.

mod crypto;

pub use crypto::{SECP256K1_RECOVER, SECP256K1_RECOVERY_CYCLES};

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::*};
use ckb_vm::{
    Error, Register, SupportMachine, Syscalls,
    memory::Memory,
    registers::{A0, A3, A4, A5, A7},
    snapshot2::DataSource,
};
use lru::LruCache;
use rayon::prelude::*;
use stem_ledger::{CapacityPolicy, Cell, ResolvedTransaction, ScriptGroupKind, block::Header};

use ckb_script::runtime::{
    Close, CurrentCycles, DataPieceId, ExecV2, INDEX_OUT_OF_BOUND, ITEM_MISSING, InheritedFd,
    LOAD_CELL_BY_FIELD_SYSCALL_NUMBER, LOAD_CELL_SYSCALL_NUMBER,
    LOAD_HEADER_BY_FIELD_SYSCALL_NUMBER, LOAD_HEADER_SYSCALL_NUMBER,
    LOAD_INPUT_BY_FIELD_SYSCALL_NUMBER, LOAD_INPUT_SYSCALL_NUMBER, LOAD_SCRIPT_HASH_SYSCALL_NUMBER,
    LOAD_SCRIPT_SYSCALL_NUMBER, LOAD_TRANSACTION_SYSCALL_NUMBER, LOAD_TX_HASH_SYSCALL_NUMBER,
    LOAD_WITNESS_SYSCALL_NUMBER, LoadCellData, Pipe, ProcessID, Read, RunMode, SUCCESS,
    ScriptContext, ScriptVersion, Spawn, VMVersion, VmContext, VmId, Wait, Write, scheduler,
    store_data, transferred_byte_cycles,
};

const INPUT: u64 = 1;
const OUTPUT: u64 = 2;
const CELL_DEP: u64 = 3;
const HEADER_DEP: u64 = 4;
const COMMITTED_PARENT: u64 = 5;
const GROUP_INPUT: u64 = 0x0100_0000_0000_0001;
const GROUP_OUTPUT: u64 = 0x0100_0000_0000_0002;

pub const LOAD_CELL_TYPE_SCRIPTS: u64 = 2110;
pub const LOAD_CELL_TYPE_SCRIPT: u64 = 2111;
pub const LOAD_CELL_TYPE_SCRIPT_HASH: u64 = 2112;
pub const FIND_CELL_TYPE_SCRIPT: u64 = 2113;
pub const LOAD_SCRIPT_GROUP: u64 = 2114;
pub const LOAD_GROUP_INDEX: u64 = 2115;
pub const LOAD_NETWORK_ID: u64 = 2116;
pub const LOAD_GROUP_PROOF: u64 = 2117;
pub const LOAD_GROUP_SEAL: u64 = 2118;
pub const LOAD_GROUP_SIGNING_DIGEST: u64 = 2119;
pub const LOAD_GROUP_MINTED_UNITS: u64 = 2120;
pub const LOAD_BLOCK_CONTEXT: u64 = 2122;
pub const LOAD_GROUP_MESSAGE: u64 = 2123;

#[derive(Clone)]
struct GroupContext {
    tx: Arc<ResolvedTransaction>,
    group: usize,
    program: u32,
    transaction_id: Byte32,
    script_hash: Byte32,
}

impl GroupContext {
    fn group_index(&self, source: u64, index: usize) -> Option<usize> {
        let group = &self.tx.groups()[self.group];
        match source {
            GROUP_INPUT => group.inputs.get(index).copied(),
            GROUP_OUTPUT => group.outputs.get(index).copied(),
            _ => None,
        }
    }

    fn cell(&self, source: u64, index: usize) -> Option<&Cell> {
        Some(match source {
            INPUT => &self.tx.inputs().get(index)?.cell,
            OUTPUT => self.tx.outputs().get(index)?,
            CELL_DEP => &self.tx.cell_deps().get(index)?.cell,
            GROUP_INPUT => &self.tx.inputs().get(self.group_index(source, index)?)?.cell,
            GROUP_OUTPUT => self.tx.outputs().get(self.group_index(source, index)?)?,
            _ => return None,
        })
    }

    fn header(&self, source: u64, index: usize) -> Result<&Header, u8> {
        let origin = match source {
            INPUT => {
                &self
                    .tx
                    .inputs()
                    .get(index)
                    .ok_or(INDEX_OUT_OF_BOUND)?
                    .origin
            }
            CELL_DEP => {
                &self
                    .tx
                    .cell_deps()
                    .get(index)
                    .ok_or(INDEX_OUT_OF_BOUND)?
                    .origin
            }
            GROUP_INPUT => {
                return self.header(
                    INPUT,
                    self.group_index(source, index).ok_or(INDEX_OUT_OF_BOUND)?,
                );
            }
            HEADER_DEP => return self.tx.header(index).ok_or(INDEX_OUT_OF_BOUND),
            COMMITTED_PARENT if index == 0 => return Ok(self.tx.parent_header()),
            _ => return Err(INDEX_OUT_OF_BOUND),
        };
        self.tx
            .header_by_origin(&origin.context_id)
            .ok_or(ITEM_MISSING)
    }
}

impl ScriptContext for GroupContext {
    fn script_version(&self) -> ScriptVersion {
        ScriptVersion::V2
    }

    fn program_data_piece_id(&self) -> DataPieceId {
        DataPieceId::CellDep(self.program)
    }
}

impl DataSource<DataPieceId> for GroupContext {
    fn load_data(&self, id: &DataPieceId, offset: u64, length: u64) -> Option<(Bytes, u64)> {
        let data = match *id {
            DataPieceId::Input(index) => self.cell(INPUT, index as usize)?.data(),
            DataPieceId::Output(index) => self.cell(OUTPUT, index as usize)?.data(),
            DataPieceId::CellDep(index) => self.cell(CELL_DEP, index as usize)?.data(),
            DataPieceId::GroupInput(index) => self.cell(GROUP_INPUT, index as usize)?.data(),
            DataPieceId::GroupOutput(index) => self.cell(GROUP_OUTPUT, index as usize)?.data(),
            DataPieceId::Witness(index) => {
                self.tx
                    .transaction()
                    .witness(u16::try_from(index).ok()?)?
                    .proof_data
            }
            DataPieceId::WitnessGroupInput(_) | DataPieceId::WitnessGroupOutput(_) => return None,
        };
        let offset = offset.min(data.len() as u64) as usize;
        let remaining = data.len() - offset;
        let read = if length == 0 {
            remaining
        } else {
            length.min(remaining as u64) as usize
        };
        Some((data.slice(offset..offset + read), remaining as u64))
    }
}

/// Verify the automatically derived Lock and Type Script Groups.
/// The same transaction cycle budget applies whether groups run serially or in parallel.
pub fn verify(tx: Arc<ResolvedTransaction>, max_cycles: u64, parallel: bool) -> Result<u64> {
    verify_groups(tx, max_cycles, parallel, &[], |_, _| {})
}

fn verify_groups(
    tx: Arc<ResolvedTransaction>,
    max_cycles: u64,
    parallel: bool,
    estimates: &[Option<u64>],
    mut observe: impl FnMut(usize, u64),
) -> Result<u64> {
    let transaction_id = tx.transaction().id(tx.network_id());
    let run = |group, limit| execute_group(Arc::clone(&tx), &transaction_id, group, limit);
    let mut total = 0;
    let group_count = tx.groups().len();
    let workers = if parallel {
        rayon::current_num_threads()
    } else {
        1
    };
    for start in (0..group_count).step_by(workers) {
        let end = (start + workers).min(group_count);
        let remaining = max_cycles - total;
        if end - start == 1 {
            let cycles = run(start, remaining)?;
            total = charge(total, cycles, max_cycles)?;
            observe(start, cycles);
            continue;
        }

        let equal_share = remaining / (end - start) as u64;
        let results: Vec<_> = (start..end)
            .into_par_iter()
            .map(|group| {
                let limit = estimates
                    .get(group)
                    .copied()
                    .flatten()
                    .map_or(equal_share, |cycles| {
                        cycles.saturating_mul(2).max(equal_share)
                    })
                    .min(remaining);
                run(group, limit)
            })
            .collect();
        for (index, result) in results.into_iter().enumerate() {
            let cycles = match result {
                Err(error) if error.downcast_ref::<Error>() == Some(&Error::CyclesExceeded) => {
                    run(start + index, max_cycles - total)?
                }
                other => other?,
            };
            total = charge(total, cycles, max_cycles)?;
            observe(start + index, cycles);
        }
    }
    Ok(total)
}

/// Caches verified transactions and remembers recent Script Group costs for scheduling.
pub struct VerificationCache {
    verified: Mutex<LruCache<Byte32, u64>>,
    estimates: Mutex<LruCache<(u8, Byte32), u64>>,
}

impl VerificationCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            verified: Mutex::new(LruCache::new(capacity)),
            estimates: Mutex::new(LruCache::new(capacity)),
        }
    }

    pub fn verify(
        &self,
        tx: Arc<ResolvedTransaction>,
        max_cycles: u64,
        parallel: bool,
    ) -> Result<u64> {
        let key = tx.script_cache_key().clone();
        let found = self
            .verified
            .lock()
            .expect("VM cache lock")
            .get(&key)
            .copied();
        if let Some(cycles) = found {
            return charge(0, cycles, max_cycles);
        }

        let cycles = if tx.groups().len() < 2 {
            verify(tx, max_cycles, parallel)?
        } else {
            let groups: Vec<_> = tx
                .groups()
                .iter()
                .map(|group| (group.kind as u8, group.script.calc_script_hash()))
                .collect();
            let estimated = if parallel {
                let mut cache = self.estimates.lock().expect("VM estimates lock");
                groups.iter().map(|id| cache.get(id).copied()).collect()
            } else {
                Vec::new()
            };
            let mut measured = Vec::with_capacity(groups.len());
            let cycles = verify_groups(tx, max_cycles, parallel, &estimated, |index, cost| {
                measured.push((index, cost));
            })?;
            let mut cache = self.estimates.lock().expect("VM estimates lock");
            for (index, cost) in measured {
                cache.put(groups[index].clone(), cost);
            }
            cycles
        };
        self.verified
            .lock()
            .expect("VM cache lock")
            .put(key, cycles);
        Ok(cycles)
    }
}

fn charge(total: u64, cycles: u64, limit: u64) -> Result<u64> {
    let total = total
        .checked_add(cycles)
        .context("transaction cycle overflow")?;
    ensure!(total <= limit, "transaction cycle budget exceeded");
    Ok(total)
}

fn execute_group(
    tx: Arc<ResolvedTransaction>,
    transaction_id: &Byte32,
    index: usize,
    limit: u64,
) -> Result<u64> {
    let group = &tx.groups()[index];
    let script_hash = group.script.calc_script_hash();
    let kind = group.kind;
    let program = tx.program_index(&group.script)?.try_into()?;
    let context = GroupContext {
        tx,
        group: index,
        program,
        transaction_id: transaction_id.clone(),
        script_hash: script_hash.clone(),
    };
    let mut scheduler = scheduler(context, syscalls, ());
    let result = scheduler
        .run(RunMode::LimitCycles(limit))
        .with_context(|| format!("{kind:?} Script Group {script_hash:#x}"))?;
    ensure!(
        result.exit_code == 0,
        "{kind:?} Script Group {script_hash:#x} exited with {}",
        result.exit_code
    );
    Ok(result.consumed_cycles)
}

fn syscalls<M: SupportMachine>(
    id: &VmId,
    group: &GroupContext,
    context: &VmContext<GroupContext>,
    _: &(),
) -> Vec<Box<dyn Syscalls<M>>> {
    vec![
        Box::new(LoadContext(group.clone())),
        Box::new(crypto::Recover),
        Box::new(LoadCellData::new(context)),
        Box::new(VMVersion::new()),
        Box::new(CurrentCycles::new(context)),
        Box::new(ExecV2::new(id, context)),
        Box::new(Spawn::new(id, context)),
        Box::new(ProcessID::new(id)),
        Box::new(Pipe::new(id, context)),
        Box::new(Wait::new(id, context)),
        Box::new(Write::new(id, context)),
        Box::new(Read::new(id, context)),
        Box::new(InheritedFd::new(id, context)),
        Box::new(Close::new(id, context)),
    ]
}

struct LoadContext(GroupContext);

impl<M: SupportMachine> Syscalls<M> for LoadContext {
    fn initialize(&mut self, _: &mut M) -> Result<(), Error> {
        Ok(())
    }

    fn ecall(&mut self, machine: &mut M) -> Result<bool, Error> {
        let number = machine.registers()[A7].to_u64();
        let index = machine.registers()[A3].to_u64() as usize;
        let source = machine.registers()[A4].to_u64();
        let arg = machine.registers()[A5].to_u64();
        let context = &self.0;
        let tx = context.tx.transaction();
        let group = &context.tx.groups()[context.group];
        let result = match number {
            LOAD_SCRIPT_SYSCALL_NUMBER => store_data(machine, group.script.as_slice()),
            LOAD_SCRIPT_HASH_SYSCALL_NUMBER => store_data(machine, context.script_hash.as_slice()),
            LOAD_TX_HASH_SYSCALL_NUMBER => store_data(machine, context.transaction_id.as_slice()),
            LOAD_TRANSACTION_SYSCALL_NUMBER => store_data(machine, tx.as_slice()),
            LOAD_NETWORK_ID => store_data(machine, context.tx.network_id().as_slice()),
            LOAD_BLOCK_CONTEXT => {
                if source != 0 || index != 0 {
                    return Ok(status(machine, INDEX_OUT_OF_BOUND));
                }
                let mut data = [0; 48];
                data[..8].copy_from_slice(&context.tx.height().to_le_bytes());
                data[8..16].copy_from_slice(
                    &context
                        .tx
                        .parent_header()
                        .context
                        .timestamp_ms
                        .to_le_bytes(),
                );
                data[16..].copy_from_slice(context.tx.parent_id().as_slice());
                store_data(machine, &data)
            }
            LOAD_SCRIPT_GROUP => {
                let mut value = [0; 9];
                value[0] = match group.kind {
                    ScriptGroupKind::Lock => 0,
                    ScriptGroupKind::Type => 1,
                };
                value[1..5].copy_from_slice(&(group.inputs.len() as u32).to_le_bytes());
                value[5..9].copy_from_slice(&(group.outputs.len() as u32).to_le_bytes());
                store_data(machine, &value)
            }
            LOAD_GROUP_INDEX => {
                let Some(index) = context.group_index(source, index) else {
                    return Ok(status(machine, INDEX_OUT_OF_BOUND));
                };
                store_data(machine, &(index as u32).to_le_bytes())
            }
            LOAD_CELL_SYSCALL_NUMBER
            | LOAD_CELL_BY_FIELD_SYSCALL_NUMBER
            | LOAD_CELL_TYPE_SCRIPTS
            | LOAD_CELL_TYPE_SCRIPT
            | LOAD_CELL_TYPE_SCRIPT_HASH
            | FIND_CELL_TYPE_SCRIPT => {
                let Some(cell) = context.cell(source, index) else {
                    return Ok(status(machine, INDEX_OUT_OF_BOUND));
                };
                return load_cell(machine, cell, context.tx.capacity_policy(), number, arg);
            }
            LOAD_WITNESS_SYSCALL_NUMBER | LOAD_GROUP_PROOF | LOAD_GROUP_SEAL => {
                let index = match source {
                    0 if index == 0 => u16::try_from(context.group).ok(),
                    1 => u16::try_from(index)
                        .ok()
                        .filter(|index| usize::from(*index) < context.tx.groups().len()),
                    _ => None,
                };
                let Some(index) = index else {
                    return Ok(status(machine, INDEX_OUT_OF_BOUND));
                };
                let Some(witness) = tx.witness(index) else {
                    return Ok(status(machine, ITEM_MISSING));
                };
                let data = match number {
                    LOAD_GROUP_PROOF => witness.proof_data,
                    LOAD_GROUP_SEAL => witness.seal,
                    _ => witness.encode_bytes(),
                };
                store_data(machine, &data)
            }
            LOAD_GROUP_MINTED_UNITS => {
                if source != 0 || index != 0 || group.kind != ScriptGroupKind::Type {
                    return Ok(status(machine, ITEM_MISSING));
                }
                store_data(machine, &context.tx.minted_units(group).to_le_bytes())
            }
            LOAD_GROUP_MESSAGE => {
                let selected = match source {
                    0 if index == 0 => context.group,
                    1 if index < context.tx.groups().len() => index,
                    _ => return Ok(status(machine, INDEX_OUT_OF_BOUND)),
                };
                let Some(message) = context.tx.group_message(selected as u16) else {
                    return Ok(status(machine, ITEM_MISSING));
                };
                store_data(machine, message)
            }
            LOAD_GROUP_SIGNING_DIGEST => {
                if source != 0 || index != 0 || group.kind != ScriptGroupKind::Lock {
                    return Ok(status(machine, ITEM_MISSING));
                }
                // The digest reads the transaction body and its proof data.
                machine
                    .add_cycles_no_checking(transferred_byte_cycles(tx.as_slice().len() as u64))?;
                let digest = context.tx.lock_digest(group);
                store_data(machine, digest.as_slice())
            }
            LOAD_INPUT_SYSCALL_NUMBER | LOAD_INPUT_BY_FIELD_SYSCALL_NUMBER => {
                let input = match source {
                    INPUT => tx.input(index),
                    GROUP_INPUT => context.group_index(source, index).and_then(|i| tx.input(i)),
                    _ => None,
                };
                let Some(input) = input else {
                    return Ok(status(machine, INDEX_OUT_OF_BOUND));
                };
                match (number, arg) {
                    (LOAD_INPUT_SYSCALL_NUMBER, _) => store_data(machine, &input.encode_bytes()),
                    (_, 0) => store_data(machine, input.previous_output.as_slice()),
                    (_, 1) => store_data(machine, &input.since_bytes()),
                    _ => return Ok(status(machine, ITEM_MISSING)),
                }
            }
            LOAD_HEADER_SYSCALL_NUMBER | LOAD_HEADER_BY_FIELD_SYSCALL_NUMBER => {
                let header = match context.header(source, index) {
                    Ok(header) => header,
                    Err(code) => return Ok(status(machine, code)),
                };
                if number == LOAD_HEADER_SYSCALL_NUMBER {
                    store_data(machine, &header.encode())
                } else {
                    let field = match arg {
                        0 => {
                            let result = store_data(machine, &header.context.height.to_le_bytes());
                            return finish(machine, result);
                        }
                        1 => {
                            let result =
                                store_data(machine, &header.context.timestamp_ms.to_le_bytes());
                            return finish(machine, result);
                        }
                        2 => &header.context.parent,
                        3 => &header.context.transaction_root,
                        4 => &header.state.cell_root,
                        5 => &header.state.system_root,
                        6 => &header.state.history_root,
                        7 => &header.receipt_root,
                        _ => return Ok(status(machine, ITEM_MISSING)),
                    };
                    store_data(machine, field.as_slice())
                }
            }
            _ => return Ok(false),
        };
        finish(machine, result)
    }
}

fn load_cell<M: SupportMachine>(
    machine: &mut M,
    cell: &Cell,
    policy: CapacityPolicy,
    number: u64,
    arg: u64,
) -> Result<bool, Error> {
    let result = match (number, arg) {
        (LOAD_CELL_SYSCALL_NUMBER, _) => store_data(machine, cell.as_slice()),
        (LOAD_CELL_BY_FIELD_SYSCALL_NUMBER, 0) => {
            store_data(machine, &cell.capacity().to_le_bytes())
        }
        (LOAD_CELL_BY_FIELD_SYSCALL_NUMBER, 1) => {
            let data = cell.data();
            machine.add_cycles_no_checking(transferred_byte_cycles(data.len() as u64))?;
            store_data(machine, cell.data_hash().as_slice())
        }
        (LOAD_CELL_BY_FIELD_SYSCALL_NUMBER, 2) => store_data(machine, cell.lock().as_slice()),
        (LOAD_CELL_BY_FIELD_SYSCALL_NUMBER, 3) => {
            let lock = cell.lock();
            machine
                .add_cycles_no_checking(transferred_byte_cycles(lock.as_slice().len() as u64))?;
            store_data(machine, lock.calc_script_hash().as_slice())
        }
        (LOAD_CELL_BY_FIELD_SYSCALL_NUMBER, 6) => {
            let capacity = policy
                .bond(cell.as_slice().len())
                .map_err(|error| Error::External(error.to_string()))?;
            store_data(machine, &capacity.to_le_bytes())
        }
        (LOAD_CELL_TYPE_SCRIPTS, _) => store_data(machine, &cell.type_scripts_bytes()),
        (LOAD_CELL_TYPE_SCRIPT | LOAD_CELL_TYPE_SCRIPT_HASH, _) => {
            let Some(script) = cell.type_script(arg as usize) else {
                return Ok(status(machine, INDEX_OUT_OF_BOUND));
            };
            if number == LOAD_CELL_TYPE_SCRIPT {
                store_data(machine, script.as_slice())
            } else {
                machine.add_cycles_no_checking(transferred_byte_cycles(
                    script.as_slice().len() as u64
                ))?;
                store_data(machine, script.calc_script_hash().as_slice())
            }
        }
        (FIND_CELL_TYPE_SCRIPT, _) => {
            let requested = machine.memory_mut().load_bytes(arg, 32)?;
            let mut matched = None;
            for script in cell.type_scripts() {
                machine.add_cycles_no_checking(transferred_byte_cycles(
                    script.as_slice().len() as u64
                ))?;
                if script.calc_script_hash().as_slice() == requested.as_ref() {
                    matched = Some(script);
                    break;
                }
            }
            let Some(matched) = matched else {
                return Ok(status(machine, ITEM_MISSING));
            };
            store_data(machine, matched.as_slice())
        }
        _ => return Ok(status(machine, ITEM_MISSING)),
    };
    finish(machine, result)
}

fn finish<M: SupportMachine>(machine: &mut M, written: Result<u64, Error>) -> Result<bool, Error> {
    machine.add_cycles_no_checking(transferred_byte_cycles(written?))?;
    Ok(status(machine, SUCCESS))
}

fn status<M: SupportMachine>(machine: &mut M, code: u8) -> bool {
    machine.set_register(A0, M::REG::from_u8(code));
    true
}
