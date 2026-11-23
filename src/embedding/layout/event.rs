use crate::x86::exception::{
    EventState, ExceptionEvent, InterruptEvent, InterruptSource, InterruptState,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct RawEventState {
    pub kind: u32,
    pub vector: u32,
    pub instruction_pc: u64,
    pub return_pc: u64,
    pub saved_flags: u64,
    pub error_code: u32,
    pub has_error_code: u32,
}

impl RawEventState {
    pub fn decode(self) -> Result<EventState, &'static str> {
        match self.kind {
            0 if self == Self::default() => Ok(EventState::Ready),
            2 if self
                == (Self {
                    kind: 2,
                    ..Self::default()
                }) =>
            {
                Ok(EventState::Shutdown)
            }
            3 if self
                == (Self {
                    kind: 3,
                    ..Self::default()
                }) =>
            {
                Ok(EventState::Halted)
            }
            4 | 5
                if self.vector <= u8::MAX.into()
                    && (self.kind == 4 || self.vector == 2)
                    && self.instruction_pc == 0
                    && self.error_code == 0
                    && self.has_error_code == 0 =>
            {
                Ok(EventState::PendingInterrupt(InterruptEvent {
                    source: if self.kind == 4 {
                        InterruptSource::Irq(self.vector as u8)
                    } else {
                        InterruptSource::Nmi
                    },
                    return_pc: self.return_pc,
                    saved_flags: self.saved_flags,
                }))
            }
            1 if self.vector <= u8::MAX.into()
                && self.error_code <= u16::MAX.into()
                && self.has_error_code <= 1
                && (self.has_error_code != 0 || self.error_code == 0) =>
            {
                Ok(EventState::Pending(ExceptionEvent {
                    numeric: None,
                    page_fault_address: None,
                    debug: None,
                    vector: self.vector as u8,
                    instruction_pc: self.instruction_pc,
                    return_pc: self.return_pc,
                    saved_flags: self.saved_flags,
                    error_code: (self.has_error_code != 0).then_some(self.error_code as u16),
                }))
            }
            _ => Err("invalid event state transport"),
        }
    }

    pub fn encode(event: EventState) -> Self {
        match event {
            EventState::Ready => Self::default(),
            EventState::Shutdown => Self {
                kind: 2,
                ..Self::default()
            },
            EventState::Halted => Self {
                kind: 3,
                ..Self::default()
            },
            EventState::PendingInterrupt(event) => Self {
                kind: if matches!(event.source, InterruptSource::Irq(_)) {
                    4
                } else {
                    5
                },
                vector: event.source.vector().into(),
                return_pc: event.return_pc,
                saved_flags: event.saved_flags,
                ..Self::default()
            },
            EventState::Pending(event) => Self {
                kind: 1,
                vector: event.vector.into(),
                instruction_pc: event.instruction_pc,
                return_pc: event.return_pc,
                saved_flags: event.saved_flags,
                error_code: event.error_code.unwrap_or(0).into(),
                has_error_code: event.error_code.is_some().into(),
            },
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != size_of::<Self>() {
            return Err("event input length differs");
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let wide = |offset| u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
        Ok(Self {
            kind: word(0),
            vector: word(4),
            instruction_pc: wide(8),
            return_pc: wide(16),
            saved_flags: wide(24),
            error_code: word(32),
            has_error_code: word(36),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C)]
pub struct RawInterruptState {
    pub irq: u32,
    pub nmi_pending: u32,
    pub nmi_blocked: u32,
}

impl RawInterruptState {
    pub fn decode(self) -> Result<InterruptState, &'static str> {
        if self.irq > 256 || self.nmi_pending > 1 || self.nmi_blocked > 1 {
            return Err("invalid interrupt state transport");
        }
        Ok(InterruptState {
            irq: self.irq.checked_sub(1).map(|vector| vector as u8),
            nmi_pending: self.nmi_pending != 0,
            nmi_blocked: self.nmi_blocked != 0,
        })
    }

    pub fn encode(state: InterruptState) -> Self {
        Self {
            irq: state.irq.map_or(0, |vector| u32::from(vector) + 1),
            nmi_pending: state.nmi_pending.into(),
            nmi_blocked: state.nmi_blocked.into(),
        }
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() != size_of::<Self>() {
            return Err("interrupt input length differs");
        }
        let word = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        Ok(Self {
            irq: word(0),
            nmi_pending: word(4),
            nmi_blocked: word(8),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_transport_keeps_source_identity_and_rejects_noncanonical_payloads() {
        let irq = RawEventState {
            kind: 4,
            vector: 13,
            return_pc: 0x1000,
            saved_flags: 0x202,
            ..RawEventState::default()
        };
        assert_eq!(
            irq.decode().unwrap(),
            EventState::PendingInterrupt(InterruptEvent {
                source: InterruptSource::Irq(13),
                return_pc: 0x1000,
                saved_flags: 0x202,
            })
        );
        for raw in [
            RawEventState {
                kind: 5,
                vector: 2,
                ..irq
            },
            RawEventState {
                kind: 3,
                ..RawEventState::default()
            },
        ] {
            assert_eq!(RawEventState::encode(raw.decode().unwrap()), raw);
        }
        for raw in [
            RawEventState {
                instruction_pc: 1,
                ..irq
            },
            RawEventState {
                has_error_code: 1,
                ..irq
            },
            RawEventState {
                error_code: 1,
                ..irq
            },
            RawEventState { vector: 256, ..irq },
            RawEventState { kind: 5, ..irq },
            RawEventState { kind: 3, ..irq },
        ] {
            assert!(raw.decode().is_err(), "{raw:?}");
        }
        let interrupts = RawInterruptState {
            irq: 256,
            nmi_pending: 1,
            nmi_blocked: 1,
        };
        assert_eq!(
            interrupts.decode().unwrap(),
            InterruptState {
                irq: Some(255),
                nmi_pending: true,
                nmi_blocked: true
            }
        );
        assert_eq!(
            RawInterruptState::encode(interrupts.decode().unwrap()),
            interrupts
        );
        assert!(
            RawInterruptState {
                irq: 257,
                ..interrupts
            }
            .decode()
            .is_err()
        );
        assert!(
            RawInterruptState {
                nmi_pending: 2,
                ..interrupts
            }
            .decode()
            .is_err()
        );
        assert!(
            RawInterruptState {
                nmi_blocked: 2,
                ..interrupts
            }
            .decode()
            .is_err()
        );
    }
}
