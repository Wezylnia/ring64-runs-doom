use super::protection::{ProtectionFault, ProtectionPhase};
use crate::x86::context::{ExecutionContext, Mode};
use crate::x86::flags;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExceptionEvent {
    pub numeric: Option<crate::x86::numeric_controls::Fault>,
    pub debug: Option<super::debug::Source>,
    pub vector: u8,
    pub instruction_pc: u64,
    pub return_pc: u64,
    pub saved_flags: u64,
    pub error_code: Option<u16>,
    pub page_fault_address: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptSource {
    Irq(u8),
    Nmi,
}

impl InterruptSource {
    pub fn vector(self) -> u8 {
        match self {
            Self::Irq(vector) => vector,
            Self::Nmi => 2,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterruptEvent {
    pub source: InterruptSource,
    pub return_pc: u64,
    pub saved_flags: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InterruptState {
    pub irq: Option<u8>,
    pub nmi_pending: bool,
    pub nmi_blocked: bool,
}

impl InterruptState {
    pub fn eligible(self, live_flags: u64, shadow: u32) -> Option<InterruptSource> {
        if self.nmi_pending && !self.nmi_blocked && shadow != 2 {
            Some(InterruptSource::Nmi)
        } else if live_flags & flags::IF != 0 && shadow == 0 {
            self.irq.map(InterruptSource::Irq)
        } else {
            None
        }
    }

    pub fn accept(
        &mut self,
        pc: u64,
        live_flags: u64,
        context: &ExecutionContext,
        shadow: u32,
    ) -> Option<InterruptEvent> {
        let source = self.eligible(live_flags, shadow)?;
        match source {
            InterruptSource::Irq(_) => self.irq = None,
            InterruptSource::Nmi => {
                self.nmi_pending = false;
                self.nmi_blocked = true;
            }
        }
        Some(InterruptEvent {
            source,
            return_pc: pc,
            saved_flags: flags::architectural(live_flags, context),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EventState {
    #[default]
    Ready,
    Pending(ExceptionEvent),
    Shutdown,
    Halted,
    PendingInterrupt(InterruptEvent),
}

impl InterruptEvent {
    pub fn validate(
        self,
        rip: u64,
        live_flags: u64,
        context: &ExecutionContext,
    ) -> Result<(), &'static str> {
        if self.return_pc != rip {
            return Err("pending interrupt return pc differs from rip");
        }
        if live_flags & !flags::IMPORTED_MASK != 0
            || live_flags & 2 == 0
            || self.saved_flags != flags::architectural(live_flags, context)
        {
            return Err("pending interrupt flags differ from live flags");
        }
        Ok(())
    }

    pub fn escalate(self, vector: u8, error_code: u16) -> EventState {
        assert!(matches!(vector, 10..=13), "selected entry protection fault");
        self.secondary(vector, error_code, None)
    }

    pub fn escalate_page_fault(self, error_code: u16, address: u64) -> EventState {
        self.secondary(14, error_code, Some(address))
    }

    pub fn escalate_alignment_check(self) -> EventState {
        self.secondary(17, 1, None)
    }

    fn secondary(self, vector: u8, error_code: u16, page_fault_address: Option<u64>) -> EventState {
        EventState::Pending(ExceptionEvent {
            numeric: None,
            debug: None,
            vector,
            instruction_pc: self.return_pc,
            return_pc: self.return_pc,
            saved_flags: self.saved_flags | flags::RF,
            error_code: Some(error_code),
            page_fault_address,
        })
    }
}

impl ExceptionEvent {
    pub fn validate(
        self,
        rip: u64,
        live_flags: u64,
        context: &ExecutionContext,
    ) -> Result<(), &'static str> {
        if let Some(source) = self.numeric {
            source.validate()?;
            if source.vector() != self.vector
                || source.instruction_pc != self.instruction_pc
                || self.instruction_pc != self.return_pc
                || self.debug.is_some()
                || self.page_fault_address.is_some()
            {
                return Err("numeric source differs from pending fault");
            }
        }
        let valid_code = match self.vector {
            0 | 1 | 6 | 7 | 16 | 19 => self.error_code.is_none(),
            8 => self.error_code == Some(0),
            10..=13 => self.error_code.is_some(),
            14 => self.error_code.is_some_and(|code| {
                code <= 63
                    && (code & 8 == 0 || code & 1 != 0)
                    && code & 0x12 != 0x12
                    && (code & 32 == 0 || code & 0x19 == 1)
            }),
            17 => matches!(self.error_code, Some(0 | 1)),
            _ => false,
        };
        if !valid_code {
            return Err("invalid exception vector or error code");
        }
        let valid_address = if self.vector == 14 {
            self.page_fault_address
                .is_some_and(crate::memory::canonical48)
        } else {
            self.page_fault_address.is_none()
        };
        if !valid_address {
            return Err("invalid page fault address source");
        }
        if self.return_pc != rip {
            return Err("pending exception return pc differs from rip");
        }
        if self.saved_flags & !(flags::IMPORTED_MASK | flags::VM) != 0
            || self.saved_flags & 2 == 0
            || live_flags & !flags::IMPORTED_MASK != 0
            || live_flags & 2 == 0
        {
            return Err("invalid pending exception flags");
        }
        let architectural = flags::architectural(live_flags, context);
        if (self.saved_flags ^ architectural) & !flags::RF != 0 {
            return Err("pending exception flags differ from live flags");
        }
        if self.vector != 1 && self.saved_flags & flags::RF == 0 {
            return Err("fault image requires rf");
        }
        if let Some(source) = self.debug {
            if self.vector != 1 || self.error_code.is_some() {
                return Err("debug source requires vector one without an error slot");
            }
            if matches!(source, super::debug::Source::TaskSwitch(_))
                && context.mode() != Mode::Protected
            {
                return Err("task-switch debug source requires protected mode");
            }
            source.validate(self, architectural)?;
        }
        Ok(())
    }

    pub fn fault(
        vector: u8,
        pc: u64,
        live_flags: u64,
        context: &ExecutionContext,
        error_code: Option<u16>,
    ) -> Self {
        Self {
            numeric: None,
            debug: None,
            vector,
            instruction_pc: pc,
            return_pc: pc,
            saved_flags: flags::architectural(live_flags, context) | flags::RF,
            error_code,
            page_fault_address: None,
        }
    }

    pub fn page_fault(
        pc: u64,
        live_flags: u64,
        context: &ExecutionContext,
        error_code: u16,
        address: u64,
    ) -> Self {
        Self {
            page_fault_address: Some(address),
            ..Self::fault(14, pc, live_flags, context, Some(error_code))
        }
    }

    pub fn escalate(self, vector: u8, error_code: u16) -> EventState {
        assert!(matches!(vector, 10..=13), "selected entry protection fault");
        self.secondary(vector, error_code, None)
    }

    pub fn escalate_page_fault(self, error_code: u16, address: u64) -> EventState {
        self.secondary(14, error_code, Some(address))
    }

    pub fn escalate_alignment_check(self) -> EventState {
        self.secondary(17, 1, None)
    }

    fn secondary(self, vector: u8, error_code: u16, page_fault_address: Option<u64>) -> EventState {
        entry_secondary(
            EventState::Pending(self),
            Self {
                numeric: None,
                debug: None,
                vector,
                error_code: Some(error_code),
                page_fault_address,
                saved_flags: self.saved_flags | flags::RF,
                ..self
            },
        )
    }
}

/// Classify an entry fault using the accepted source while retaining the
/// processor image at the point of failure, which can belong to a new task.
pub fn entry_secondary(source: EventState, fault: ExceptionEvent) -> EventState {
    let previous = match source {
        EventState::Pending(exception) => exception.vector,
        EventState::PendingInterrupt(_) => return EventState::Pending(fault),
        _ => unreachable!("entry requires an accepted event"),
    };
    if previous == 8 {
        return EventState::Shutdown;
    }
    if fault.vector != 17
        && (previous == 14 || fault.vector != 14 && matches!(previous, 0 | 10..=13))
    {
        EventState::Pending(ExceptionEvent {
            vector: 8,
            error_code: Some(0),
            page_fault_address: None,
            debug: None,
            numeric: None,
            ..fault
        })
    } else {
        EventState::Pending(fault)
    }
}

impl EventState {
    pub fn halt(context: &ExecutionContext) -> Result<Self, ProtectionFault> {
        if context.mode() != Mode::Real && context.cpl() != 0 {
            return Err(ProtectionFault {
                vector: 13,
                error_code: 0,
                phase: ProtectionPhase::Privilege,
                selector: None,
            });
        }
        Ok(Self::Halted)
    }

    pub fn validate(
        self,
        rip: u64,
        live_flags: u64,
        context: &ExecutionContext,
        shadow: u32,
        interrupts: InterruptState,
    ) -> Result<(), &'static str> {
        match self {
            Self::Pending(event) => {
                event.validate(rip, live_flags, context)?;
                if let Some(source) = event.debug {
                    source.validate_shadow(shadow)?;
                }
                Ok(())
            }
            Self::PendingInterrupt(event) => {
                event.validate(rip, live_flags, context)?;
                if shadow == 2 {
                    return Err("pending interrupt retains ss inhibition");
                }
                match event.source {
                    InterruptSource::Irq(_) if live_flags & flags::IF == 0 || shadow != 0 => {
                        Err("pending irq has incompatible flags or inhibition")
                    }
                    InterruptSource::Nmi if !interrupts.nmi_blocked => {
                        Err("pending nmi requires nmi blocking")
                    }
                    _ => Ok(()),
                }
            }
            Self::Halted if shadow != 0 => Err("halted state retains inhibition"),
            Self::Halted => Ok(()),
            Self::Ready | Self::Shutdown => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests;
