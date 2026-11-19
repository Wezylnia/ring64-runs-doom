use super::*;
use crate::embedding::layout::{
    ALIGNMENT_CHECK, DEBUG_EXCEPTION, DISPATCH, PAGE_FAULT, PHYSICAL_MEMORY, PROTECTION, SHUTDOWN,
    SYSTEM_MEMORY, TASK_CAPTURED, TASK_EVENT_BASE,
};
use crate::x86::{
    exception::{self, EventState, ExceptionEvent},
    flags, paging,
    protection::{
        self, Failure, ProtectionFault, ProtectionPhase,
        task::switch32::{self, Error, Incoming, Start},
    },
};

// Private return to the matching host. An incomplete qualification must resume
// in the incoming task after private backing is repaired, without nesting again.
const TASK_RETRY_BASE: i32 = -128;

#[derive(Clone, Copy)]
pub(super) enum Work {
    Captured { start: Start, slots: u8, int1: bool },
    Qualifying(Incoming, Origin),
}

#[derive(Clone, Copy)]
pub(super) enum Origin {
    Instruction {
        pc: u64,
        slots: u8,
        deferred_origin: Option<u64>,
        int1: bool,
    },
    Delivery(EventState),
}

impl Core {
    pub(super) fn capture_task(&mut self, start: Start, slots: u8, int1: bool) -> i32 {
        if !self.physical_controls {
            return UNSUPPORTED;
        }
        if self.task_work.is_some() {
            return -1;
        }
        self.task_work = Some(Work::Captured { start, slots, int1 });
        TASK_CAPTURED
    }

    fn task_paging(&mut self, pdbr: u32) -> Result<paging::State, i32> {
        let old = self.paging.expect("physical task controls");
        let mut candidate = old;
        if old.cr0 & (1 << 31) != 0 {
            let gp = |core: &mut Core| {
                core.protection_fault = Some(ProtectionFault {
                    vector: 13,
                    error_code: 0,
                    phase: ProtectionPhase::Target,
                    selector: None,
                });
                PROTECTION
            };
            candidate = old
                .prepare_physical_cr_write_in_context(3, u64::from(pdbr), &self.context)
                .map_err(|_| gp(self))?;
            if old.cr3 != candidate.cr3 && old.needs_pdpte_reload(candidate, 3) {
                candidate.pdpte = Some(match candidate.load_pdpte(&self.image) {
                    Ok(entries) => entries,
                    Err(paging::PdpteFailure::Reserved) => return Err(gp(self)),
                    Err(paging::PdpteFailure::Physical { slot, fault }) => {
                        self.pdpte_load_fault = Some((3, slot, fault));
                        return Err(PHYSICAL_MEMORY);
                    }
                });
            }
        }
        candidate.cr0 |= 8;
        Ok(candidate)
    }

    fn task_failure(&mut self, error: Error) -> i32 {
        match error {
            Error::Unsupported => UNSUPPORTED,
            Error::Fault(Failure::Protection(fault)) => {
                self.protection_fault = Some(fault);
                PROTECTION
            }
            Error::Fault(Failure::SystemPaging(fault)) => self.system_paging_failure(fault),
            Error::Fault(Failure::SystemMemory(fault)) => {
                self.system_memory_fault = Some(fault);
                SYSTEM_MEMORY
            }
            Error::Fault(Failure::Operand(failure)) => {
                unsafe {
                    (*self.state.get()).fault_segment = Segment::Ss as u32;
                }
                self.protection_fault =
                    protection::access_fault(&self.context, Segment::Ss, failure);
                self.access_failure(failure);
                self.memory_failure_reason()
            }
        }
    }

    fn task_exception(&self, reason: i32, origin: Origin) -> Option<ExceptionEvent> {
        let state = unsafe { &*self.state.get() };
        let fault = |vector, code| {
            ExceptionEvent::fault(vector, state.rip, state.flags, &self.context, Some(code))
        };
        match reason {
            PAGE_FAULT => match self.paging_failure? {
                paging::walk::Failure::Page(p) => Some(ExceptionEvent::page_fault(
                    state.rip,
                    state.flags,
                    &self.context,
                    p.error_code as u16,
                    p.address,
                )),
                _ => None,
            },
            PROTECTION | SEGMENT | DATA => {
                self.protection_fault.map(|p| fault(p.vector, p.error_code))
            }
            ALIGNMENT_CHECK => Some(fault(17, u16::from(matches!(origin, Origin::Delivery(_))))),
            SYSTEM_MEMORY => self
                .system_memory_fault
                .and_then(|p| protection::system_fault(&self.context, p))
                .map(|p| fault(p.vector, p.error_code)),
            DEBUG_EXCEPTION => self.debug_exception.as_ref().map(|d| ExceptionEvent {
                numeric: None,
                page_fault_address: None,
                debug: d.source,
                vector: 1,
                instruction_pc: d.instruction_pc,
                return_pc: d.resume_pc,
                saved_flags: d.resume_flags,
                error_code: None,
            }),
            _ => None,
        }
    }

    fn task_result(&mut self, origin: Origin, reason: i32, committed: bool) -> i32 {
        let Some(mut fault) = self.task_exception(reason, origin) else {
            return reason;
        };
        let next = match origin {
            Origin::Instruction { .. } if !committed => return reason,
            Origin::Delivery(old) if reason != DEBUG_EXCEPTION => {
                if !committed && let EventState::Pending(event) = old {
                    fault.instruction_pc = event.instruction_pc;
                }
                exception::entry_secondary(old, fault)
            }
            _ => EventState::Pending(fault),
        };
        unsafe { (*self.state.get()).set_event(next) };
        if next == EventState::Shutdown {
            SHUTDOWN
        } else if matches!(origin, Origin::Delivery(_)) {
            DISPATCH
        } else {
            TASK_EVENT_BASE - reason
        }
    }

    pub(super) fn switch_task(&mut self, start: Start, origin: Origin) -> i32 {
        if !self.physical_controls {
            return UNSUPPORTED;
        }
        let state = unsafe { &*self.state.get() };
        let old_regs = state.registers;
        let flags = state.flags;
        self.paging_work.clear();
        let prepared = guest_semantics!(self, true, switch32::prepare, start, &old_regs, flags);
        let prepared = match prepared {
            Ok(p) => p,
            Err(error) => {
                self.paging_work.clear();
                let reason = self.task_failure(error);
                return self.task_result(origin, reason, false);
            }
        };
        let candidate = match self.task_paging(prepared.cr3) {
            Ok(bank) => bank,
            Err(reason) => {
                self.paging_work.clear();
                return self.task_result(origin, reason, false);
            }
        };
        let old = self.paging.unwrap();
        let mut mem = paging::access::Memory::new(
            &mut self.image,
            &old,
            self.context.cpl(),
            flags,
            &mut self.paging_work,
            unsafe { &(*self.state.get()).protection_keys },
        )
        .with_cache(&self.translation_cache);
        let incoming = prepared.commit(&mut self.context, &mut mem);
        self.paging_work.clear();
        let incoming = match incoming {
            Ok(value) => value,
            Err(error) => {
                let reason = self.task_failure(error);
                return self.task_result(origin, reason, false);
            }
        };
        if old.cr3 != candidate.cr3 {
            self.translation_cache.invalidate_pcid(candidate.pcid());
            self.guest_root_changed();
        } else {
            self.context_changed();
        }
        self.paging = Some(candidate);
        let state = unsafe { &mut *self.state.get() };
        for (reg, low) in state.registers.iter_mut().zip(incoming.registers) {
            *reg = (*reg & !(u32::MAX as u64)) | u64::from(low);
        }
        state.flags = (u64::from(incoming.flags) & flags::IMPORTED_MASK) | 2;
        state.rip = u64::from(incoming.eip);
        state.interruptibility = 0;
        state.debug.continuation = Default::default();
        state.debug.registers.switch_task();
        self.debug_candidate = 0;
        state.set_event(EventState::Ready);
        if let Origin::Instruction { .. } = origin {
            state.remaining -= 1;
            state.time.tsc = state.time.tsc.wrapping_add(1);
        }
        self.qualify_task(incoming, origin)
    }

    fn qualify_task(&mut self, incoming: Incoming, origin: Origin) -> i32 {
        let state = unsafe { &mut *self.state.get() };
        let mut sp = state.registers[4];
        let mut mem = paging::access::Memory::new(
            &mut self.image,
            self.paging.as_ref().unwrap(),
            self.context.cpl(),
            state.flags,
            &mut self.paging_work,
            &state.protection_keys,
        )
        .with_cache(&self.translation_cache);
        let result = incoming.qualify(&mut self.context, &mut mem, &mut sp);
        self.paging_work.clear();
        state.registers[4] = sp;
        if let Err(error) = result {
            let reason = self.task_failure(error);
            if reason == PHYSICAL_MEMORY {
                self.task_work = Some(Work::Qualifying(incoming, origin));
                return TASK_RETRY_BASE - reason;
            }
            return self.task_result(origin, reason, true);
        }
        let pc = state.rip;
        let flags = state.flags;
        if matches!(origin, Origin::Instruction { int1: true, .. })
            || matches!(origin, Origin::Delivery(EventState::Pending(e)) if e.vector == 1)
        {
            state.debug.registers.enter();
        }
        if incoming.trap {
            let slots = match origin {
                Origin::Instruction { slots, .. } => slots,
                Origin::Delivery(_) => 0,
            };
            let reason = self.select_debug(
                pc,
                pc,
                false,
                flags,
                Some(crate::x86::debug::Source::TaskSwitch(slots)),
            );
            return self.task_result(origin, reason, true);
        }
        if let Origin::Instruction {
            pc: at,
            slots,
            deferred_origin,
            ..
        } = origin
            && slots != 0
        {
            let reason = self.select_debug(
                at,
                pc,
                true,
                flags,
                Some(crate::x86::debug::Source::Data {
                    slots,
                    single_step: false,
                    completed: true,
                    origin: deferred_origin,
                }),
            );
            return self.task_result(origin, reason, true);
        }
        if matches!(origin, Origin::Instruction { .. }) {
            external_interrupts::step(self, pc, flags, 0, false)
        } else {
            DISPATCH
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_complete_task() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        match core.task_work.take() {
            Some(Work::Captured { start, slots, int1 }) => {
                let state = unsafe { &*core.state.get() };
                if state.remaining == 0 || state.event.kind != 0 {
                    return -1;
                }
                let deferred = state.debug.continuation;
                core.switch_task(
                    start,
                    Origin::Instruction {
                        pc: state.rip,
                        slots: slots | deferred.slots as u8,
                        deferred_origin: (deferred.slots != 0).then_some(deferred.origin),
                        int1,
                    },
                )
            }
            Some(Work::Qualifying(incoming, origin)) => core.qualify_task(incoming, origin),
            None => -1,
        }
    })
}
