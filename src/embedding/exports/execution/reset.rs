use crate::embedding::exports::*;

impl Core {
    fn reset(&mut self) {
        // Retire proofs before replacing architectural banks. Keep Image and its
        // monotonic owner allocator: old pending handles can never name new code.
        self.revoke_host_translations();
        self.context = ExecutionContext::reset();
        self.paging = Some(crate::x86::paging::State::reset());
        self.fast_system = Default::default();
        unsafe { *self.state.get() = RunState::reset() };
        self.io_effect = false;
        self.task_work = None;
        self.paging_work.clear();
        self.paging_failure = None;
        self.pdpte_load_fault = None;
        self.paging_system = None;
        self.clear_instruction_faults();
        self.decode_fault = None;
        self.numeric_form = None;
        self.debug_exception = None;
        self.debug_candidate = 0;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_reset() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let Some(core) = bridge.core.as_mut() else {
            return bridge.error("CPU is disposed");
        };
        if !core.physical_controls {
            return bridge.error("reset requires physical memory");
        }
        core.reset();
        0
    })
}
