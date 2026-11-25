use crate::types::DataPieceId;
use crate::{syscalls::CURRENT_CYCLES, types::VmContext};
use ckb_vm::snapshot2::DataSource;
use ckb_vm::{
    Error as VMError, Register, SupportMachine, Syscalls,
    registers::{A0, A7},
};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Default)]
pub struct CurrentCycles {
    base: Arc<AtomicU64>,
}

impl CurrentCycles {
    pub fn new<C>(vm_context: &VmContext<C>) -> Self
    where
        C: DataSource<DataPieceId> + Send + Sync + 'static,
    {
        Self {
            base: Arc::clone(&vm_context.base_cycles),
        }
    }
}

impl<Mac: SupportMachine> Syscalls<Mac> for CurrentCycles {
    fn initialize(&mut self, _machine: &mut Mac) -> Result<(), VMError> {
        Ok(())
    }

    fn ecall(&mut self, machine: &mut Mac) -> Result<bool, VMError> {
        if machine.registers()[A7].to_u64() != CURRENT_CYCLES {
            return Ok(false);
        }
        let cycles = self
            .base
            .load(Ordering::Acquire)
            .checked_add(machine.cycles())
            .ok_or(VMError::CyclesOverflow)?;
        machine.set_register(A0, Mac::REG::from_u64(cycles));
        Ok(true)
    }
}
