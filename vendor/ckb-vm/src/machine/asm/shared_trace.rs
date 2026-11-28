use std::cell::RefCell;

use super::{
    AsmCoreMachine, AsmCoreMachineRevealer, FLAG_EXECUTABLE, RISCV_PAGESIZE, Trace, check_memory,
    check_permission,
};

pub(super) const CODE_BYTES: usize = 128;
const CACHE_SLOTS: usize = 1024;

pub(super) struct Template {
    isa: u8,
    version: u32,
    length: usize,
    code: [u8; CODE_BYTES],
    pub decoded: usize,
    pub trace: Trace,
    #[cfg(feature = "trace-cycle-reuse")]
    pub estimated_cycles: bool,
}

pub(super) struct Cache(Box<[Option<Template>]>);

impl Cache {
    fn new() -> Self {
        Self(std::iter::repeat_with(|| None).take(CACHE_SLOTS).collect())
    }

    pub fn slot(&self, isa: u8, version: u32, pc: u64, code: &[u8]) -> usize {
        let word = u32::from_le_bytes(code[..4].try_into().expect("code prefix"));
        let hash = word ^ word.rotate_right(10) ^ word.rotate_right(21);
        ((pc as usize >> 1) ^ hash as usize ^ usize::from(isa) ^ version as usize)
            & (CACHE_SLOTS - 1)
    }

    pub fn get(
        &self,
        slot: usize,
        isa: u8,
        version: u32,
        pc: u64,
        code: &[u8],
    ) -> Option<&Template> {
        self.0[slot].as_ref().filter(|template| {
            template.isa == isa
                && template.version == version
                && template.trace.address == pc
                && template.code[..template.length] == code[..template.length]
        })
    }

    pub fn insert(
        &mut self,
        slot: usize,
        isa: u8,
        version: u32,
        code: &[u8],
        length: usize,
        decoded: usize,
        trace: &Trace,
        #[cfg(feature = "trace-cycle-reuse")] estimated_cycles: bool,
    ) {
        self.0[slot] = Some(Template {
            isa,
            version,
            length,
            code: code.try_into().expect("code window"),
            decoded,
            trace: trace.clone(),
            #[cfg(feature = "trace-cycle-reuse")]
            estimated_cycles,
        });
    }
}

thread_local! {
    static SPARE: RefCell<Option<Cache>> = RefCell::new(Some(Cache::new()));
}

pub(super) struct Lease(Option<Cache>);

impl Lease {
    pub fn take() -> Self {
        Self(
            SPARE
                .try_with(|spare| spare.borrow_mut().take())
                .ok()
                .flatten(),
        )
    }

    pub fn cache(&mut self) -> Option<&mut Cache> {
        self.0.as_mut()
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Some(cache) = self.0.take() {
            let _ = SPARE.try_with(|spare| *spare.borrow_mut() = Some(cache));
        }
    }
}

/// The fast path uses one executable page. Multi-page and out-of-range
/// accesses retain the normal decoder and its exact errors.
pub(super) fn code<R: AsmCoreMachineRevealer>(core: &mut R, pc: u64) -> Option<&[u8]> {
    let start = usize::try_from(pc).ok()?;
    let end = start.checked_add(CODE_BYTES)?;
    let page = start / RISCV_PAGESIZE;
    if end as u64 > core.as_ref().memory_size || page != (end - 1) / RISCV_PAGESIZE {
        return None;
    }
    check_permission(core, page as u64, FLAG_EXECUTABLE).ok()?;
    check_memory(core, page as u64);
    let machine: &AsmCoreMachine = R::as_ref(core);
    Some(&machine.memory[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ISA_IMC, Memory, SupportMachine, machine::VERSION2};

    #[test]
    fn template_rejects_changes_to_any_observed_code_byte() {
        let mut cache = Cache::new();
        let pc = 16;
        let code = [0; CODE_BYTES];
        let trace = Trace {
            address: pc,
            length: 4,
            ..Default::default()
        };
        let slot = cache.slot(0, 2, pc, &code);
        cache.insert(
            slot, 0, 2, &code, 12, 1, &trace,
            #[cfg(feature = "trace-cycle-reuse")]
            false,
        );
        assert!(cache.get(slot, 0, 2, pc, &code).is_some());
        assert!(cache.get(slot, 1, 2, pc, &code).is_none());
        assert!(cache.get(slot, 0, 1, pc, &code).is_none());
        assert!(cache.get(slot, 0, 2, pc + 2, &code).is_none());
        for i in 0..12 {
            let mut changed = code;
            changed[i] = 1;
            assert!(cache.get(slot, 0, 2, pc, &changed).is_none());
        }
    }

    #[test]
    fn only_one_currently_executable_page_can_supply_cached_code() {
        let pc = RISCV_PAGESIZE as u64;
        let mut core = <Box<AsmCoreMachine> as SupportMachine>::new(ISA_IMC, VERSION2, 10_000);
        assert!(code(&mut core, pc).is_none());
        core.set_flag(1, FLAG_EXECUTABLE).unwrap();
        assert!(code(&mut core, pc).is_some());
        assert!(code(&mut core, 2 * pc - 2).is_none());
        assert!(code(&mut core, u64::MAX).is_none());
    }
}
