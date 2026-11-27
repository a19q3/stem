//! Reuse the assembly VM allocation when requested by the embedding node.

use std::cell::RefCell;

use ckb_vm::{
    DefaultMachine, DefaultMachineRunner, Error, SupportMachine,
    machine::asm::{AsmCoreMachine, AsmMachine},
};

thread_local! {
    static SPARE: RefCell<Option<Box<AsmCoreMachine>>> = const { RefCell::new(None) };
}

pub struct ReusableMachine(Option<AsmMachine>);

pub(crate) fn allocate_core(isa: u8, version: u32, max_cycles: u64) -> Box<AsmCoreMachine> {
    let Ok(Some(mut core)) = SPARE.try_with(|spare| spare.borrow_mut().take()) else {
        return <Box<AsmCoreMachine> as SupportMachine>::new(isa, version, max_cycles);
    };
    // Match a fresh ckb-vm 0.24.15 core. The VM lazily initializes guest
    // memory through frame flags. Invalidate its fast access lookups too.
    core.registers.fill(0);
    core.pc = 0;
    core.next_pc = 0;
    core.running = 0;
    core.cycles = 0;
    core.max_cycles = max_cycles;
    core.chaos_seed = 0;
    core.load_reservation_address = u64::MAX;
    core.reset_signal = 0;
    core.isa = isa;
    core.version = version;
    core.last_read_frame = u64::MAX;
    core.last_write_page = u64::MAX;
    core.flags.fill(0);
    core.frames.fill(0);
    core.clear_traces();
    core
}

impl DefaultMachineRunner for ReusableMachine {
    type Inner = Box<AsmCoreMachine>;

    fn new(machine: DefaultMachine<Self::Inner>) -> Self {
        Self(Some(AsmMachine::new(machine)))
    }

    fn machine(&self) -> &DefaultMachine<Self::Inner> {
        self.0.as_ref().expect("live VM").machine()
    }

    fn machine_mut(&mut self) -> &mut DefaultMachine<Self::Inner> {
        self.0.as_mut().expect("live VM").machine_mut()
    }

    fn run(&mut self) -> Result<i8, Error> {
        self.0.as_mut().expect("live VM").run_with_shared_traces()
    }
}

impl Drop for ReusableMachine {
    fn drop(&mut self) {
        let core = self.0.take().expect("live VM").machine.take_inner();
        if core.memory_size == ckb_vm::RISCV_MAX_MEMORY as u64 {
            let _ = SPARE.try_with(|spare| {
                let mut spare = spare.borrow_mut();
                if spare.is_none() {
                    *spare = Some(core);
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use ckb_vm::{
        CoreMachine, DefaultMachineBuilder, ISA_B, ISA_IMC, ISA_MOP, RISCV_PAGESIZE,
        bytes::Bytes,
        cost_model::estimate_cycles,
        machine::{VERSION0, VERSION1, VERSION2},
        memory::{FLAG_EXECUTABLE, FLAG_FREEZED, Memory},
    };

    use super::*;

    const CODE: u64 = RISCV_PAGESIZE as u64;
    const DATA: u64 = 512 * 1024;

    fn machine<M: DefaultMachineRunner<Inner = Box<AsmCoreMachine>>>(
        allocate: crate::scheduler::CoreAllocator<M>,
        version: u32,
        max_cycles: u64,
        program: &[u32],
    ) -> M {
        let isa = if version == VERSION0 {
            ISA_IMC
        } else {
            ISA_IMC | ISA_B | ISA_MOP
        };
        let mut core = allocate(isa, version, max_cycles);
        load_at_same_address(&mut core, program);
        M::new(
            DefaultMachineBuilder::new(core)
                .instruction_cycle_func(Box::new(estimate_cycles))
                .build(),
        )
    }

    fn load_at_same_address(core: &mut Box<AsmCoreMachine>, program: &[u32]) {
        let bytes: Vec<_> = program.iter().flat_map(|word| word.to_le_bytes()).collect();
        core.init_pages(
            CODE,
            RISCV_PAGESIZE as u64,
            FLAG_EXECUTABLE | FLAG_FREEZED,
            Some(Bytes::from(bytes)),
            0,
        )
        .unwrap();
        core.update_pc(CODE);
        core.commit_pc();
    }

    #[test]
    fn reused_core_matches_fresh_vm_after_success_and_cycle_limit() {
        // lui s0, 0x80; ld a0, 0(s0); sd a0, 0(s0); addi a0, a0, 7; addi a7, zero, 93; ecall
        let store_and_exit = [
            0x0008_0437,
            0x0004_3503,
            0x00a4_3023,
            0x0075_0513,
            0x05d0_0893,
            0x0000_0073,
        ];
        // lui s0, 0x80; ld a0, 0(s0); addi a7, zero, 93; ecall
        let load_and_exit = [0x0008_0437, 0x0004_3503, 0x05d0_0893, 0x0000_0073];
        let mut first =
            machine::<ReusableMachine>(allocate_core, VERSION2, 10_000, &store_and_exit);
        first.inner_mut().store64(&DATA, &42).unwrap();
        assert_eq!(first.run(), Ok(49));
        let previous = first.inner_mut();
        previous.set_lr(&DATA);
        let address = std::ptr::from_ref::<AsmCoreMachine>(previous.as_ref());
        drop(first);

        for (version, max_cycles) in [(VERSION1, 10_000), (VERSION2, 1), (VERSION0, 10_000)] {
            let mut reused =
                machine::<ReusableMachine>(allocate_core, version, max_cycles, &load_and_exit);
            let mut fresh = machine::<AsmMachine>(
                <Box<AsmCoreMachine> as SupportMachine>::new,
                version,
                max_cycles,
                &load_and_exit,
            );
            assert_eq!(std::ptr::from_ref(reused.inner_mut().as_ref()), address);
            assert_eq!(*reused.inner_mut().lr(), u64::MAX);
            let expected = if max_cycles == 1 {
                Err(Error::CyclesExceeded)
            } else {
                Ok(0)
            };
            assert_eq!(reused.run(), expected);
            assert_eq!(fresh.run(), expected);
            assert_eq!(reused.inner_mut().cycles(), fresh.inner_mut().cycles());
        }
    }

    #[test]
    fn reset_and_reuse_invalidate_traces_at_multiple_addresses() {
        let program = |entry: u32, target: u32| {
            let mut words = [0; 67];
            words[0] = (entry << 20) | 0x0000_0513; // addi a0, zero, entry
            words[1] = 0x0fc0_006f; // jal zero, 252; target is in another bitmap word
            words[64] = (target << 20) | 0x0005_0513; // addi a0, a0, target
            words[65] = 0x05d0_0893; // addi a7, zero, 93
            words[66] = 0x0000_0073; // ecall
            words
        };
        let first_code = program(1, 7);
        let reset_code = program(2, 9);
        let reused_code = program(3, 11);
        let mut first = machine::<ReusableMachine>(allocate_core, VERSION2, 10_000, &first_code);
        assert_eq!(first.run(), Ok(8));
        let address = std::ptr::from_ref(&**first.inner_mut());
        first.inner_mut().reset(10_000);
        load_at_same_address(first.inner_mut(), &reset_code);
        assert_eq!(first.run(), Ok(11));
        drop(first);

        let mut reused = machine::<ReusableMachine>(allocate_core, VERSION2, 10_000, &reused_code);
        let mut fresh = machine::<AsmMachine>(
            <Box<AsmCoreMachine> as SupportMachine>::new,
            VERSION2,
            10_000,
            &reused_code,
        );
        assert_eq!(std::ptr::from_ref(&**reused.inner_mut()), address);
        assert_eq!(reused.run(), Ok(14));
        assert_eq!(fresh.run(), Ok(14));
        assert_eq!(reused.inner_mut().cycles(), fresh.inner_mut().cycles());
    }

    #[test]
    fn reused_trace_charges_the_current_cycle_model() {
        let program = [0x0070_0513, 0x05d0_0893, 0x0000_0073];
        let mut warm = machine::<ReusableMachine>(allocate_core, VERSION2, 10_000, &program);
        assert_eq!(warm.run(), Ok(7));
        drop(warm);

        let build = |allocate: crate::scheduler::CoreAllocator<ReusableMachine>| {
            let mut core = allocate(ISA_IMC | ISA_B | ISA_MOP, VERSION2, 10_000);
            load_at_same_address(&mut core, &program);
            DefaultMachineBuilder::new(core)
                .instruction_cycle_func(Box::new(|_| 31))
                .build()
        };
        let mut reused = ReusableMachine::new(build(allocate_core));
        let mut fresh = AsmMachine::new(build(<Box<AsmCoreMachine> as SupportMachine>::new));
        assert_eq!(reused.run(), Ok(7));
        assert_eq!(fresh.run(), Ok(7));
        assert_eq!(reused.inner_mut().cycles(), 3 * 31);
        assert_eq!(reused.inner_mut().cycles(), fresh.inner_mut().cycles());
    }
}
