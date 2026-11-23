use crate::embedding::exports::*;
use crate::x86::instruction::Operation;
use crate::x86::processor::Instruction as Processor;

impl Core {
    pub(in crate::embedding::exports) fn numeric_admission_unrestricted(&self) -> bool {
        self.paging
            .as_ref()
            .is_none_or(|bank| bank.cr0 & 0x2c == 0x20 && bank.cr4 & 0x600 == 0x600)
    }

    // These checks precede the generated instruction-begin/RF effect. Numeric
    // enable faults win over the old pending x87 condition, including NE=0.
    pub(in crate::embedding::exports) fn instruction_admission(
        &mut self,
        operation: Operation,
        pc: u64,
        reached: bool,
    ) -> i32 {
        let required_cr4 = match operation {
            Operation::DebugRegister { register, .. }
                if self.debug_register_admission(register) != 0 =>
            {
                return INVALID;
            }
            Operation::SegmentBase { .. } => crate::x86::paging::CR4_FSGSBASE,
            Operation::NumericImage { op, .. } if op.is_xstate() => crate::x86::paging::CR4_OSXSAVE,
            Operation::Processor(Processor::Xgetbv | Processor::Xsetbv) => {
                crate::x86::paging::CR4_OSXSAVE
            }
            _ => 0,
        };
        if required_cr4 != 0 && self.paging.is_none_or(|bank| bank.cr4 & required_cr4 == 0) {
            self.decode_fault = Some(crate::x86::decode::DecodeFault::UndefinedOpcode);
            return INVALID;
        }
        // Both callers clear diagnostics first; NE1/all-enabled needs no saved numeric PC.
        if self.numeric_admission_unrestricted() {
            return 0;
        }
        let reason = self.numeric_admission(operation, pc);
        if reason != 0 {
            return reason;
        }
        if !reached {
            return 0;
        }
        let waiting = match operation {
            Operation::X87(instruction) => instruction.op.is_waiting(),
            Operation::Mmx { op, .. } => op.checks_pending(),
            Operation::NumericImage { op, .. } => op.is_waiting(),
            Operation::MaskedStore { mmx, .. } => mmx,
            _ => false,
        };
        if waiting
            && unsafe { (*self.state.get()).x87.pending() } != 0
            && self.pending_x87_reason() == UNSUPPORTED
        {
            UNSUPPORTED
        } else {
            0
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_probe(pc: u64) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.debug_candidate = 0;
        core.fault = None;
        core.alignment_fault = None;
        core.alignment_check = None;
        core.simd_exception = None;
        core.numeric_fault = None;
        core.numeric_form = None;
        core.general_protection = None;
        core.protection_fault = None;
        core.system_memory_fault = None;
        core.segment_fault = None;
        core.decode_fault = None;
        unsafe { (*core.state.get()).fault_segment = Segment::Cs as u32 };
        let (flags, shadow) = unsafe {
            (
                (*core.state.get()).flags,
                (*core.state.get()).interruptibility,
            )
        };
        let debug = core.debug_execute(pc, flags, shadow);
        if debug != 0 {
            return debug;
        }
        let decoded = if core.paging.is_some() {
            core.paged_decode(pc)
        } else {
            decode::decode_for_translation(pc, &core.context, false, |offset| {
                core.context
                    .fetch(&core.image, offset)
                    .map_err(|failure| match failure {
                        AccessFailure::Linear(fault) => DecodeError::Fetch(fault),
                        AccessFailure::Segment(fault) => DecodeError::Segment(fault),
                        AccessFailure::Paging(fault) => DecodeError::Paging(fault),
                        AccessFailure::AlignmentCheck(_) => {
                            unreachable!("instruction fetch is alignment exempt")
                        }
                    })
            })
        };
        core.decode_fault = decoded
            .as_ref()
            .err()
            .and_then(|error| error.architectural_fault());
        match decoded {
            Err(DecodeError::PagingRestart) => 26,
            Err(DecodeError::Paging(failure)) => core.paging_failure(failure),
            Ok(instruction) => core.instruction_admission(instruction.operation, pc, false),
            Err(DecodeError::Segment(fault)) => {
                core.segment_fault = Some(fault);
                SEGMENT
            }
            Err(DecodeError::Unsupported) => UNSUPPORTED,
            Err(DecodeError::Undefined) => UNSUPPORTED,
            Err(
                DecodeError::UndefinedEncoding
                | DecodeError::Invalid
                | DecodeError::InstructionLength,
            ) => INVALID,
            Err(DecodeError::Fetch(fault)) => {
                core.fault = Some(fault);
                FETCH
            }
        }
    })
}
