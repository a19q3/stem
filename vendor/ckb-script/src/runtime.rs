//! VM scheduling and syscall primitives for hosts with their own ledger context.

#[cfg(all(has_asm, feature = "vm-reuse"))]
pub use crate::reusable_asm::ReusableMachine as Machine;
#[cfg(not(all(has_asm, feature = "vm-reuse")))]
pub use crate::types::Machine;

pub use crate::{
    cost_model::transferred_byte_cycles,
    syscalls::{
        Close, CurrentCycles, ExecV2, INDEX_OUT_OF_BOUND, ITEM_MISSING, InheritedFd,
        LOAD_CELL_BY_FIELD_SYSCALL_NUMBER, LOAD_CELL_SYSCALL_NUMBER,
        LOAD_HEADER_BY_FIELD_SYSCALL_NUMBER, LOAD_HEADER_SYSCALL_NUMBER,
        LOAD_INPUT_BY_FIELD_SYSCALL_NUMBER, LOAD_INPUT_SYSCALL_NUMBER,
        LOAD_SCRIPT_HASH_SYSCALL_NUMBER, LOAD_SCRIPT_SYSCALL_NUMBER,
        LOAD_TRANSACTION_SYSCALL_NUMBER, LOAD_TX_HASH_SYSCALL_NUMBER, LOAD_WITNESS_SYSCALL_NUMBER,
        LoadCellData, Pipe, ProcessID, Read, SUCCESS, Spawn, VMVersion, Wait, Write,
        utils::store_data,
    },
    types::{DataPieceId, RunMode, ScriptContext, ScriptVersion, VmContext, VmId},
};

use crate::{Scheduler, types::SyscallGenerator};
use ckb_vm::DefaultMachineRunner;

pub fn scheduler<C: ScriptContext, V: Clone>(
    group: C,
    syscalls: SyscallGenerator<C, V, <Machine as DefaultMachineRunner>::Inner>,
    context: V,
) -> Scheduler<C, V, Machine> {
    let scheduler = Scheduler::new(group, syscalls, context);
    #[cfg(all(has_asm, feature = "vm-reuse"))]
    let scheduler = {
        let mut scheduler = scheduler;
        scheduler.core_allocator = Some(crate::reusable_asm::allocate_core);
        scheduler
    };
    scheduler
}
