//! Bounded RISC-V execution for a Lineage command. Unlike a validation group,
//! an Executor returns replacement Data and Capacity. It cannot access the store.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use ckb_script::runtime::{store_data, transferred_byte_cycles};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::*};
use ckb_vm::{
    DefaultCoreMachine, DefaultMachineBuilder, Error, ISA_B, ISA_IMC, ISA_MOP, Memory, Register,
    SparseMemory, SupportMachine, Syscalls, WXorXMemory,
    cost_model::estimate_cycles,
    machine::VERSION2,
    registers::{A0, A1, A2, A3, A4, A7},
};
use stem_ledger::{
    Transaction,
    lineage::{MAX_DATA_BYTES, SEAL_LEN, actor_id},
};

use crate::crypto::{SECP256K1_RECOVERY_CYCLES, recover_cached};

pub const LOAD_EXECUTOR_CONTEXT: u64 = 2126;
pub const LOAD_EXECUTOR_DATA: u64 = 2127;
pub const WRITE_EXECUTOR_OUTPUT: u64 = 2128;

pub fn business_signature_cycles(tx: &Transaction) -> u64 {
    tx.lineage_intent().map_or(0, |intent| {
        intent.actors.len() as u64 * SECP256K1_RECOVERY_CYCLES
    })
}

/// Recover every actor from the same business scope, in canonical actor order.
pub fn verify_business_actors(tx: &Transaction, network: &Byte32) -> Result<Vec<Byte32>> {
    let Some(intent) = tx.lineage_intent() else {
        return Ok(vec![]);
    };
    let seals = tx.lineage_seals();
    ensure!(
        seals.len() == intent.actors.len() * SEAL_LEN,
        "missing Lineage business seals"
    );
    let digest: [u8; 32] = tx.lineage_intent_digest(network)?.unpack();
    for (expected, seal) in intent.actors.iter().zip(seals.chunks_exact(SEAL_LEN)) {
        let key = recover_cached(seal.try_into().expect("fixed business seal"), digest)
            .context("invalid Lineage business signature")?;
        ensure!(
            &actor_id(&key) == expected,
            "Lineage business signature names another actor"
        );
    }
    Ok(intent.actors.clone())
}

pub struct Input {
    pub id: Byte32,
    pub version: u64,
    pub capacity: u64,
    pub data: Bytes,
}

#[derive(Clone)]
pub struct Output {
    pub capacity: u64,
    pub data: Bytes,
}

pub struct ContextData {
    pub height: u64,
    pub parent_time_ms: u64,
    pub payer_lock: Byte32,
    pub actors: Vec<Byte32>,
    pub payload: Bytes,
    pub inputs: Vec<Input>,
}

pub fn run(program: &Bytes, context: ContextData, max_cycles: u64) -> Result<(Vec<Output>, u64)> {
    run_with_crypto(program, context, max_cycles, false)
}

pub fn run_with_crypto(
    program: &Bytes,
    context: ContextData,
    max_cycles: u64,
    crypto_v1: bool,
) -> Result<(Vec<Output>, u64)> {
    let outputs = Arc::new(Mutex::new(vec![None; context.inputs.len()]));
    let host = Executor {
        context,
        outputs: Arc::clone(&outputs),
    };
    let core = DefaultCoreMachine::<u64, WXorXMemory<SparseMemory<u64>>>::new(
        ISA_IMC | ISA_B | ISA_MOP,
        VERSION2,
        max_cycles,
    );
    let mut builder = DefaultMachineBuilder::new(core)
        .instruction_cycle_func(Box::new(estimate_cycles))
        .syscall(Box::new(host));
    if crypto_v1 {
        builder = builder.syscall(Box::new(crate::crypto::V1));
    }
    let mut machine = builder.build();
    let loaded = machine.load_program(program, std::iter::empty())?;
    machine.add_cycles(transferred_byte_cycles(loaded))?;
    let exit = machine.run()?;
    ensure!(
        exit == 0,
        "Lineage Executor rejected the operation ({exit})"
    );
    let cycles = machine.cycles();
    drop(machine);
    let outputs = Arc::into_inner(outputs)
        .expect("Executor VM released its output buffer")
        .into_inner()
        .expect("Executor output buffer");
    Ok((
        outputs
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .context("Lineage Executor omitted a successor")?,
        cycles,
    ))
}

struct Executor {
    context: ContextData,
    outputs: Arc<Mutex<Vec<Option<Output>>>>,
}

impl<M: SupportMachine> Syscalls<M> for Executor {
    fn initialize(&mut self, _: &mut M) -> Result<(), Error> {
        Ok(())
    }

    fn ecall(&mut self, machine: &mut M) -> Result<bool, Error> {
        let call = machine.registers()[A7].to_u64();
        let status = match call {
            LOAD_EXECUTOR_CONTEXT | LOAD_EXECUTOR_DATA => {
                let index = machine.registers()[A3].to_u64();
                let source = machine.registers()[A4].to_u64();
                let mut fixed = [0; 52];
                let data = match (call, source, index) {
                    (LOAD_EXECUTOR_CONTEXT, 0, 0) => {
                        fixed[..8].copy_from_slice(&self.context.height.to_le_bytes());
                        fixed[8..16].copy_from_slice(&self.context.parent_time_ms.to_le_bytes());
                        fixed[16..48].copy_from_slice(self.context.payer_lock.as_slice());
                        fixed[48..]
                            .copy_from_slice(&(self.context.inputs.len() as u32).to_le_bytes());
                        Some(fixed.as_slice())
                    }
                    (LOAD_EXECUTOR_DATA, 0, 0) => Some(self.context.payload.as_ref()),
                    (_, 1, index) => self.context.inputs.get(index as usize).map(|input| {
                        if call == LOAD_EXECUTOR_CONTEXT {
                            fixed[..32].copy_from_slice(input.id.as_slice());
                            fixed[32..40].copy_from_slice(&input.version.to_le_bytes());
                            fixed[40..48].copy_from_slice(&input.capacity.to_le_bytes());
                            &fixed[..48]
                        } else {
                            input.data.as_ref()
                        }
                    }),
                    (LOAD_EXECUTOR_CONTEXT, 2, index) => self
                        .context
                        .actors
                        .get(index as usize)
                        .map(|actor| actor.as_slice()),
                    _ => None,
                };
                if let Some(data) = data {
                    let copied = store_data(machine, data)?;
                    machine.add_cycles(transferred_byte_cycles(copied))?;
                    0
                } else {
                    1
                }
            }
            WRITE_EXECUTOR_OUTPUT => {
                let mut outputs = self.outputs.lock().expect("Executor output buffer");
                let index = machine.registers()[A2].to_u64();
                let length = machine.registers()[A1].to_u64();
                if let Some(slot) = outputs
                    .get_mut(index as usize)
                    .filter(|slot| slot.is_none() && length <= MAX_DATA_BYTES as u64)
                {
                    let pointer = machine.registers()[A0].to_u64();
                    let capacity = machine.registers()[A3].to_u64();
                    machine.add_cycles(transferred_byte_cycles(length))?;
                    let data = machine.memory_mut().load_bytes(pointer, length)?;
                    *slot = Some(Output { capacity, data });
                    0
                } else {
                    1
                }
            }
            _ => return Ok(false),
        };
        machine.set_register(A0, M::REG::from_u8(status));
        Ok(true)
    }
}
