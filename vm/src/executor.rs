//! Bounded RISC-V execution for a Lineage command. Unlike a validation group,
//! an Executor returns replacement Data and Capacity. It cannot access the store.

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, ensure};
use ckb_script::runtime::{ScriptVersion, machine, store_data, transferred_byte_cycles};
use ckb_types::{bytes::Bytes, packed::Byte32, prelude::*};
use ckb_vm::{
    DefaultMachineRunner, Error, Memory, Register, SupportMachine, Syscalls,
    registers::{A0, A1, A2, A3, A4, A7},
};
use stem_ledger::lineage::MAX_DATA_BYTES;

pub const LOAD_EXECUTOR_CONTEXT: u64 = 2126;
pub const LOAD_EXECUTOR_DATA: u64 = 2127;
pub const WRITE_EXECUTOR_OUTPUT: u64 = 2128;

pub struct Input {
    pub id: Byte32,
    pub version: u64,
    pub capacity: u64,
    pub data: Bytes,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Output {
    pub capacity: u64,
    pub data: Bytes,
}

pub struct ContextData {
    pub height: u64,
    pub parent_time_ms: u64,
    pub payer_lock: Byte32,
    pub payload: Bytes,
    pub inputs: Vec<Input>,
}

pub fn run(program: &Bytes, context: ContextData, max_cycles: u64) -> Result<(Vec<Output>, u64)> {
    run_on(program, context, |host| {
        machine(ScriptVersion::V2, max_cycles, host)
    })
}

fn run_on<R: DefaultMachineRunner>(
    program: &Bytes,
    context: ContextData,
    create: impl FnOnce(Executor) -> R,
) -> Result<(Vec<Output>, u64)> {
    let outputs = Arc::new(Mutex::new(vec![None; context.inputs.len()]));
    let host = Executor {
        context,
        outputs: Arc::clone(&outputs),
    };
    let mut machine = create(host);
    let loaded = machine.load_program(program, std::iter::empty())?;
    machine
        .inner_mut()
        .add_cycles(transferred_byte_cycles(loaded))?;
    let exit = machine.run()?;
    ensure!(
        exit == 0,
        "Lineage Executor rejected the operation ({exit})"
    );
    let cycles = machine.machine().cycles();
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

#[cfg(test)]
#[allow(
    dead_code,
    reason = "shared contract builder also provides a Lock convenience function"
)]
#[path = "../../contracts/test_support.rs"]
mod contract;

#[cfg(test)]
mod tests {
    use super::*;
    use ckb_vm::{
        DefaultCoreMachine, DefaultMachineBuilder, SparseMemory, WXorXMemory,
        cost_model::estimate_cycles,
    };

    fn portable(
        program: &Bytes,
        context: ContextData,
        max_cycles: u64,
    ) -> Result<(Vec<Output>, u64)> {
        let version = ScriptVersion::V2;
        run_on(program, context, |host| {
            let core = DefaultCoreMachine::<u64, WXorXMemory<SparseMemory<u64>>>::new(
                version.vm_isa(),
                version.vm_version(),
                max_cycles,
            );
            DefaultMachineBuilder::new(core)
                .instruction_cycle_func(Box::new(estimate_cycles))
                .syscall(Box::new(host))
                .build()
        })
    }

    fn counters(increment: u64, heads: usize) -> ContextData {
        ContextData {
            height: 31,
            parent_time_ms: 42,
            payer_lock: [12; 32].pack(),
            payload: Bytes::copy_from_slice(&increment.to_le_bytes()),
            inputs: (0..heads)
                .map(|i| Input {
                    id: [i as u8; 32].pack(),
                    version: 2,
                    capacity: i as u64 + 1,
                    data: Bytes::copy_from_slice(&(i as u128).to_le_bytes()),
                })
                .collect(),
        }
    }

    #[test]
    fn reusable_executor_matches_portable_on_outputs_cycles_and_failures() -> Result<()> {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let program = contract::native_contract(&workspace, "stem-lineage-counter")?;
        for (increment, heads, limit) in [(1, 2, 100_000), (2, 16, 100_000), (5, 1, 100_000)] {
            let reference = portable(&program, counters(increment, heads), limit)?;
            let actual = run(&program, counters(increment, heads), limit)?;
            assert_eq!(reference, actual);
            assert_eq!(actual.0.len(), heads);
            for (index, output) in actual.0.iter().enumerate() {
                assert_eq!(output.capacity, index as u64 + 1);
                assert_eq!(
                    output.data.as_ref(),
                    &(index as u128 + u128::from(increment)).to_le_bytes()
                );
            }
            let too_short = actual.1 - 1;
            assert!(portable(&program, counters(increment, heads), too_short).is_err());
            assert!(run(&program, counters(increment, heads), too_short).is_err());
            assert_eq!(actual, run(&program, counters(increment, heads), limit)?);
        }
        assert_eq!(
            portable(&program, counters(0, 2), 100_000)
                .unwrap_err()
                .to_string(),
            run(&program, counters(0, 2), 100_000)
                .unwrap_err()
                .to_string()
        );
        Ok(())
    }

    #[test]
    #[ignore = "alternating portable and assembly Executor microbenchmark"]
    fn measure_executor_backends() -> Result<()> {
        use std::time::Instant;

        const SAMPLES: usize = 1_000;
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let program = contract::native_contract(&workspace, "stem-lineage-counter")?;
        for heads in [1, 8, 16] {
            let mut samples = [Vec::with_capacity(SAMPLES), Vec::with_capacity(SAMPLES)];
            let expected = portable(&program, counters(2, heads), 100_000)?;
            assert_eq!(run(&program, counters(2, heads), 100_000)?, expected);
            for index in 0..SAMPLES {
                for backend in [index % 2, 1 - index % 2] {
                    let context = counters(2, heads);
                    let start = Instant::now();
                    let actual = if backend == 0 {
                        portable(&program, context, 100_000)?
                    } else {
                        run(&program, context, 100_000)?
                    };
                    samples[backend].push(start.elapsed().as_nanos());
                    assert_eq!(actual, expected);
                }
            }
            let percentiles = samples.map(|mut durations| {
                durations.sort_unstable();
                [50, 95, 99].map(|p| durations[(p * SAMPLES).div_ceil(100) - 1])
            });
            println!(
                "Executor heads={heads} samples={SAMPLES} portable_ns={:?} host_ns={:?} cycles={}",
                percentiles[0], percentiles[1], expected.1,
            );
        }
        Ok(())
    }
}
