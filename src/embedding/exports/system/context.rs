use crate::embedding::exports::*;

impl Core {
    pub(in crate::embedding::exports) fn access_failure(&mut self, failure: AccessFailure) {
        match failure {
            AccessFailure::AlignmentCheck(fault) => {
                self.alignment_fault = None;
                self.alignment_check = Some(fault);
            }
            AccessFailure::Linear(fault) => self.fault = Some(fault),
            AccessFailure::Segment(fault) => self.segment_fault = Some(fault),
            AccessFailure::Paging(fault) => {
                self.paging_failure(fault);
            }
        }
    }

    #[inline(always)]
    pub(in crate::embedding::exports) fn segment_address(
        &mut self,
        offset: u64,
        width: u32,
        segment: Segment,
        access: Access,
        rmw: bool,
    ) -> Option<u64> {
        self.fault = None;
        self.alignment_check = None;
        self.segment_fault = None;
        unsafe {
            (*self.state.get()).fault_segment = segment as u32;
        }
        match self.context.translate(offset, width, segment, access, rmw) {
            Ok(address) => Some(address),
            Err(failure) => {
                self.access_failure(failure);
                None
            }
        }
    }
}

pub(in crate::embedding::exports) fn segment_fault_json(fault: &SegmentFault) -> serde_json::Value {
    serde_json::json!({
        "segment":fault.segment.name(),
        "offset":format!("0x{:x}",fault.offset),
        "width":fault.width,
        "access":match fault.access {Access::Read=>"read",Access::Write=>"write",Access::Execute=>"execute"},
        "cause":match fault.cause {SegmentCause::Unusable=>"unusable",SegmentCause::Type=>"type",SegmentCause::Limit=>"limit"},
        "failingOffset":format!("0x{:x}",fault.failing),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_context_info() {
    BRIDGE.with_borrow_mut(|bridge| {
        bridge.info = serde_json::to_vec(&bridge.core.as_ref().unwrap().context.to_json()).unwrap();
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn core_context_version() -> u64 {
    BRIDGE.with_borrow(|bridge| bridge.core.as_ref().unwrap().context_version)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_set_context() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let physical = bridge.core.as_ref().unwrap().physical_controls;
        let result = serde_json::from_slice(&bridge.input)
            .map_err(|_| "invalid execution context")
            .and_then(|value| ExecutionContext::from_json_profile(&value, physical));
        let context = match result {
            Ok(context) => context,
            Err(message) => return bridge.error(message),
        };
        let core = bridge.core.as_mut().unwrap();
        if core.physical_controls {
            if let Err(message) = core
                .paging
                .expect("physical controls")
                .validate_physical_context(&context)
            {
                return bridge.error(message);
            }
        } else if core.paging.is_some()
            && !matches!(
                context.mode(),
                crate::x86::context::Mode::Long64 | crate::x86::context::Mode::Compatibility
            )
        {
            return bridge.error("paged execution requires an IA32e context");
        }
        unsafe {
            (*core.state.get()).interruptibility = 0;
            (*core.state.get()).set_event(crate::x86::exception::EventState::Ready);
            (*core.state.get()).debug.continuation =
                crate::embedding::layout::debug::Continuation::default();
            (*core.state.get()).interrupts.nmi_blocked = 0;
        };
        core.task_work = None;
        core.debug_exception = None;
        core.clear_instruction_faults();
        if core.context != context {
            core.context = context;
            core.context_changed();
        }
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_target_check(target: u64) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.fault = None;
        core.segment_fault = None;
        unsafe {
            (*core.state.get()).fault_segment = Segment::Cs as u32;
        }
        match core.context.check_target(target) {
            Ok(()) => 1,
            Err(failure) => {
                core.access_failure(failure);
                0
            }
        }
    })
}
