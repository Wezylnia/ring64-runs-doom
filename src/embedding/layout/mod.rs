pub mod debug;
mod event;
pub mod numeric;
pub mod paging;
pub use event::{RawEventState, RawInterruptState};

use crate::x86::exception::EventState;
use crate::x86::processor::TimeState;
use crate::x86::protection_keys::State as ProtectionKeysState;
use crate::x86::state::State;
use crate::x86::x87::State as X87State;

// Private returns from a compiled unit or its cold task-switch continuation.
pub const TASK_CAPTURED: i32 = -2;
pub const TASK_EVENT_BASE: i32 = -64;

// Private tags of the guarded dword-pair read-span ABI.
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const PAGED_PAIR_START: u32 = 6;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const PAGED_PAIR_CONTINUE: u32 = 7;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const PAGED_PAIR_WINDOW: u32 = 8;

// Private width encoding on the existing paged scalar store import.
#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) const PAGED_STACK_QWORD: u32 = 40;
// Private width encoding on the paged scalar load import.
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const PAGED_RETURN_QWORD: u32 = 40;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const QUIET_RETURN_READ: u32 = 2;

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Default)]
#[repr(C)]
pub(crate) struct ReturnRead {
    pub(crate) value: u64,
    pub(crate) status: u32,
}

pub const DISPATCH: i32 = 0;
pub const BUDGET: i32 = 1;
pub const STOP: i32 = 2;
pub const UNSUPPORTED: i32 = 3;
pub const FETCH: i32 = 4;
pub const TARGET: i32 = 5;
pub const DATA: i32 = 6;
pub const INVALID: i32 = 7;
pub const RESOURCE: i32 = 8;
pub const DIVIDE_ERROR: i32 = 9;
pub const SEGMENT: i32 = 10;
pub const REPEAT_BUDGET: i32 = 11;
pub const ALIGNMENT: i32 = 12;
pub const SIMD_EXCEPTION: i32 = 13;
pub const GENERAL_PROTECTION: i32 = 14;
pub const X87_EXCEPTION: i32 = 15;
pub const PROTECTION: i32 = 16;
pub const SYSTEM_MEMORY: i32 = 17;
pub const DEBUG_EXCEPTION: i32 = 18;
pub const EVENT_BUDGET: i32 = 19;
pub const PENDING_EXCEPTION: i32 = 20;
pub const SHUTDOWN: i32 = 21;
pub const PENDING_INTERRUPT: i32 = 22;
pub const HALTED: i32 = 23;
pub const PAGE_FAULT: i32 = 24;
pub const PHYSICAL_MEMORY: i32 = 25;
pub const DEVICE_NOT_AVAILABLE: i32 = 27;
pub const ALIGNMENT_CHECK: i32 = 28;
pub const SCALAR_STATE_SIZE: usize = 176;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const COPY_SCALAR: u32 = 1 << 31;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const COPY_NO_SPLIT: u32 = 1 << 30;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const COPY_FILL_INDEX: u32 = 1 << 29;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const INDEXED_WRITE_UNSUPPORTED_LEAF: u32 = 1;

pub const REASONS: [&str; 29] = [
    "dispatch",
    "budget",
    "stop",
    "unsupported",
    "fetch-fault",
    "target-fault",
    "data-fault",
    "invalid-encoding",
    "resource-limit",
    "divide-error",
    "segment-fault",
    "repeat-budget",
    "alignment-fault",
    "simd-exception",
    "general-protection",
    "x87-exception",
    "protection-fault",
    "system-memory-fault",
    "debug-exception",
    "event-budget",
    "pending-exception",
    "shutdown",
    "pending-interrupt",
    "halted",
    "page-fault",
    "physical-memory-fault",
    "paging-restart",
    "device-not-available",
    "alignment-check",
];

#[repr(C)]
pub struct RunState {
    pub registers: [u64; 16],
    pub flags: u64,
    pub rip: u64,
    pub remaining: u64,
    pub target: u64,
    pub fault_segment: u32,
    pub interruptibility: u32,
    pub repeat_remaining: u64,
    pub xmm: [[u8; 16]; 16],
    pub vector_scratch: [u8; 16],
    pub mxcsr: u32,
    pub x87: X87State,
    pub event: RawEventState,
    pub delivery_enabled: u32,
    pub interrupts: RawInterruptState,
    pub debug: debug::DebugState,
    pub paging_source: paging::RawPagingSource,
    pub numeric_source: numeric::RawNumericSource,
    pub time: TimeState,
    pub protection_keys: ProtectionKeysState,
    pub xcr0: u64,
}

// Share the existing state pointer with compiler-private caches without
// putting their backing pointers in the SDK-visible RunState layout.
#[cfg(any(test, target_arch = "wasm32"))]
#[repr(C)]
pub(crate) struct StateStorage {
    run: std::cell::UnsafeCell<RunState>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) flat_reads: [crate::memory::DirectSpan; 2],
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) flat_data_write: crate::memory::DirectSpan,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) flat_stack_pointer_read: crate::memory::DirectSpan,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) indexed_paged_reads: u32,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) loop_read_table: std::cell::Cell<u32>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) loop_read_generation: std::cell::Cell<u64>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    pub(crate) loop_read_range: std::cell::Cell<paging::LoopReadRange>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) newest_fetch: paging::NewestFetch,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) flat_stack_write: crate::memory::DirectSpan,
    // Flat32 generated code truncates its cached base to 32 bits.
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) x87_long64_reads: [crate::memory::DirectSpan; 2],
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(crate) x87_long64_writes: [crate::memory::DirectSpan; 2],
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) const FLAT_STACK_POINTER_READ: i32 = 0x100 | crate::x86::context::Segment::Ss as i32;

#[cfg(any(test, target_arch = "wasm32"))]
impl StateStorage {
    pub(crate) fn new(run: RunState) -> Self {
        Self {
            run: std::cell::UnsafeCell::new(run),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            flat_reads: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            flat_data_write: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            flat_stack_pointer_read: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            indexed_paged_reads: 0,
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            loop_read_table: Default::default(),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            loop_read_generation: Default::default(),
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            loop_read_range: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            newest_fetch: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            flat_stack_write: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            x87_long64_reads: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            x87_long64_writes: Default::default(),
        }
    }

    pub(crate) fn get(&self) -> *mut RunState {
        self.run.get()
    }
}

impl Default for RunState {
    fn default() -> Self {
        Self {
            registers: [0; 16],
            flags: 2,
            rip: 0,
            remaining: 0,
            target: 0,
            fault_segment: 0,
            interruptibility: 0,
            repeat_remaining: 0,
            xmm: [[0; 16]; 16],
            vector_scratch: [0; 16],
            mxcsr: 0x1f80,
            x87: X87State::default(),
            event: RawEventState::default(),
            delivery_enabled: 0,
            interrupts: RawInterruptState::default(),
            debug: debug::DebugState::default(),
            paging_source: paging::RawPagingSource::default(),
            numeric_source: numeric::RawNumericSource::default(),
            time: TimeState::default(),
            protection_keys: ProtectionKeysState::ZERO,
            xcr0: crate::x86::xstate::RESET_XCR0,
        }
    }
}

impl RunState {
    pub fn reset() -> Self {
        let mut state = Self {
            rip: 0xfff0,
            ..Self::default()
        };
        state.registers[2] = 0x600;
        state.x87.control_word = 0x40;
        state.x87.tag_word = 0x5555;
        state
    }
    pub fn import(&mut self, state: State) {
        self.registers = state.registers;
        self.flags = state.flags;
        self.rip = state.rip;
        self.remaining = 0;
        self.target = 0;
        self.fault_segment = 0;
        self.interruptibility = 0;
        self.repeat_remaining = 0;
        self.event = RawEventState::default();
        self.interrupts = RawInterruptState::default();
        self.debug.source = debug::DebugSource::default();
        self.debug.continuation = debug::Continuation::default();
        self.paging_source = paging::RawPagingSource::default();
        self.numeric_source = numeric::RawNumericSource::default();
    }

    pub fn event_state(&self) -> Result<EventState, &'static str> {
        let mut event = self.event.decode()?;
        let source = self.debug.source.decode()?;
        if let EventState::Pending(ref mut pending) = event {
            if source.is_some() && pending.vector != 1 {
                return Err("debug source on non-debug exception");
            }
            pending.debug = source;
        } else if source.is_some() {
            return Err("debug source without pending exception");
        }
        self.paging_source.attach(&mut event)?;
        self.numeric_source.attach(&mut event)?;
        Ok(event)
    }
    pub fn set_event(&mut self, event: EventState) {
        self.debug.source = debug::DebugSource::encode(if let EventState::Pending(e) = event {
            e.debug
        } else {
            None
        });
        self.numeric_source =
            numeric::RawNumericSource::encode(if let EventState::Pending(e) = event {
                e.numeric
            } else {
                None
            });
        self.event = RawEventState::encode(event);
        self.paging_source =
            paging::RawPagingSource::encode(if let EventState::Pending(e) = event {
                e.page_fault_address
            } else {
                None
            });
    }
    pub fn descriptor() -> serde_json::Value {
        let mut numeric_source = numeric::RawNumericSource::descriptor();
        numeric_source["offset"] = serde_json::json!(std::mem::offset_of!(Self, numeric_source));
        let mut paging_source = paging::RawPagingSource::descriptor();
        paging_source["offset"] = serde_json::json!(std::mem::offset_of!(Self, paging_source));
        serde_json::json!({
            "xcr0": std::mem::offset_of!(Self, xcr0),
            "protectionKeys": {
                "offset": std::mem::offset_of!(Self, protection_keys),
                "size": size_of::<ProtectionKeysState>(),
                "pkru": std::mem::offset_of!(ProtectionKeysState, pkru),
                "pkrs": std::mem::offset_of!(ProtectionKeysState, pkrs),
            },
            "time": {
                "offset": std::mem::offset_of!(Self, time),
                "size": size_of::<TimeState>(),
                "tsc": std::mem::offset_of!(TimeState, tsc),
                "tscAux": std::mem::offset_of!(TimeState, tsc_aux),
            },
            "pagingSource": paging_source,
            "numericSource": numeric_source,
            "debug": { "offset": std::mem::offset_of!(Self,debug), "layout": debug::DebugState::descriptor() },
            "size": size_of::<Self>(),
            "scalarSize": SCALAR_STATE_SIZE,
            "xmm": std::mem::offset_of!(Self, xmm),
            "xmmBytes": size_of::<[[u8; 16]; 16]>(),
            "vectorScratch": std::mem::offset_of!(Self, vector_scratch),
            "mxcsr": std::mem::offset_of!(Self, mxcsr),
            "mxcsrBytes": size_of::<u32>(),
            "x87": {
                "offset": std::mem::offset_of!(Self, x87),
                "bytes": size_of::<X87State>(),
                "registers": std::mem::offset_of!(X87State, registers),
                "registerBytes": 10,
                "controlWord": std::mem::offset_of!(X87State, control_word),
                "statusWord": std::mem::offset_of!(X87State, status_word),
                "tagWord": std::mem::offset_of!(X87State, tag_word),
                "opcode": std::mem::offset_of!(X87State, opcode),
                "instructionPointer": std::mem::offset_of!(X87State, instruction_pointer),
                "dataPointer": std::mem::offset_of!(X87State, data_pointer),
            },
            "event": {
                "offset": std::mem::offset_of!(Self, event),
                "bytes": size_of::<RawEventState>(),
                "kind": std::mem::offset_of!(RawEventState, kind),
                "vector": std::mem::offset_of!(RawEventState, vector),
                "instructionPc": std::mem::offset_of!(RawEventState, instruction_pc),
                "returnPc": std::mem::offset_of!(RawEventState, return_pc),
                "savedFlags": std::mem::offset_of!(RawEventState, saved_flags),
                "errorCode": std::mem::offset_of!(RawEventState, error_code),
                "hasErrorCode": std::mem::offset_of!(RawEventState, has_error_code),
            },
            "deliveryEnabled": std::mem::offset_of!(Self, delivery_enabled),
            "interrupts": {
                "offset": std::mem::offset_of!(Self, interrupts),
                "bytes": size_of::<RawInterruptState>(),
                "irq": std::mem::offset_of!(RawInterruptState, irq),
                "nmiPending": std::mem::offset_of!(RawInterruptState, nmi_pending),
                "nmiBlocked": std::mem::offset_of!(RawInterruptState, nmi_blocked),
            },
            "registers": std::mem::offset_of!(Self, registers),
            "flags": std::mem::offset_of!(Self, flags),
            "rip": std::mem::offset_of!(Self, rip),
            "remaining": std::mem::offset_of!(Self, remaining),
            "repeatRemaining": std::mem::offset_of!(Self, repeat_remaining),
            "faultSegment": std::mem::offset_of!(Self, fault_segment),
            "interruptibility": std::mem::offset_of!(Self, interruptibility),
            "interruptibilityBytes": size_of::<u32>(),
            "reasons": REASONS,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interruptibility_uses_scalar_padding_without_moving_any_numeric_state() {
        assert_eq!(std::mem::offset_of!(RunState, interruptibility), 164);
        assert_eq!(std::mem::offset_of!(RunState, repeat_remaining), 168);
        assert_eq!(std::mem::offset_of!(RunState, xmm), 176);
        assert_eq!(size_of::<RunState>(), 800);
        assert_eq!(REASONS[18], "debug-exception");
        assert_eq!(RunState::descriptor()["interruptibilityBytes"], 4);
        let mut state = RunState {
            interruptibility: 2,
            ..RunState::default()
        };
        state.import(State::default());
        assert_eq!(state.interruptibility, 0);
    }

    #[test]
    fn x87_projection_appends_after_mxcsr_and_scalar_import_preserves_pending_and_ghosts() {
        let mut transport = RunState::default();
        assert_eq!(std::mem::offset_of!(RunState, mxcsr), 448);
        assert_eq!(std::mem::offset_of!(RunState, x87), 456);
        assert_eq!(size_of::<RunState>(), 800);
        let projection = &RunState::descriptor()["x87"];
        assert_eq!(projection["bytes"], 104);
        assert_eq!(projection["controlWord"], 80);
        assert_eq!(projection["instructionPointer"], 88);
        transport.x87.registers[7] = [0xa5; 10];
        transport.x87.control_word = 0x37b;
        transport.x87.status_word = 0xa884;
        let before = transport.x87;
        transport.import(State::default());
        assert_eq!(transport.x87, before);
        assert_eq!(REASONS[X87_EXCEPTION as usize], "x87-exception");
    }

    #[test]
    fn xmm_banks_append_without_changing_scalar_import_or_legacy_offsets() {
        let mut state = RunState::default();
        assert_eq!(std::mem::offset_of!(RunState, xmm), 176);
        assert_eq!(std::mem::offset_of!(RunState, vector_scratch), 432);
        assert_eq!(size_of::<RunState>(), 800);
        assert_eq!(RunState::descriptor()["scalarSize"], 176);
        assert_eq!(RunState::descriptor()["xmmBytes"], 256);
        assert_eq!(state.xmm, [[0; 16]; 16]);
        state.xmm[15] = [0xff; 16];
        state.import(State::default());
        assert_eq!(state.xmm[15], [0xff; 16]);
    }

    #[test]
    fn repeated_work_extends_transport_after_existing_fields() {
        let descriptor = RunState::descriptor();
        assert_eq!(descriptor["size"], 800);
        assert_eq!(descriptor["repeatRemaining"], 168);
        assert_eq!(descriptor["reasons"][11], "repeat-budget");
    }

    #[test]
    fn divide_error_extends_the_controlled_reasons_without_moving_state() {
        assert_eq!(DIVIDE_ERROR, 9);
        assert_eq!(REASONS[DIVIDE_ERROR as usize], "divide-error");
        assert_eq!(
            &REASONS[..9],
            &[
                "dispatch",
                "budget",
                "stop",
                "unsupported",
                "fetch-fault",
                "target-fault",
                "data-fault",
                "invalid-encoding",
                "resource-limit",
            ]
        );
        assert_eq!(RunState::descriptor()["reasons"][9], "divide-error");
        assert_eq!(size_of::<RunState>(), 800);
    }

    #[test]
    fn runtime_target_transport_keeps_existing_state_offsets() {
        assert_eq!(std::mem::offset_of!(RunState, registers), 0);
        assert_eq!(std::mem::offset_of!(RunState, flags), 128);
        assert_eq!(std::mem::offset_of!(RunState, rip), 136);
        assert_eq!(std::mem::offset_of!(RunState, remaining), 144);
        assert_eq!(std::mem::offset_of!(RunState, target), 152);
        assert_eq!(std::mem::offset_of!(RunState, fault_segment), 160);
        assert_eq!(std::mem::offset_of!(RunState, repeat_remaining), 168);
        assert_eq!(size_of::<RunState>(), 800);
        assert_eq!(RunState::descriptor()["size"], 800);
    }

    #[test]
    fn architectural_import_resets_private_execution_transport() {
        let mut transport = RunState {
            remaining: 17,
            repeat_remaining: 23,
            target: 0x8000_0000_0008,
            ..RunState::default()
        };
        let registers = std::array::from_fn(|i| 0x1122_3344_0000_0000 + i as u64);
        transport.import(State {
            registers,
            flags: 0x8d7,
            rip: 0x1000,
        });
        assert_eq!(transport.registers, registers);
        assert_eq!(transport.flags, 0x8d7);
        assert_eq!(transport.rip, 0x1000);
        assert_eq!(transport.remaining, 0);
        assert_eq!(transport.repeat_remaining, 0);
        assert_eq!(transport.target, 0);
        assert_eq!(RunState::default().target, 0);
    }
}

#[cfg(test)]
mod protection_keys_append_tests {
    use super::*;

    #[test]
    fn protection_keys_append_after_time_and_survive_scalar_import() {
        assert_eq!(std::mem::offset_of!(RunState, time), 768);
        assert_eq!(std::mem::offset_of!(RunState, protection_keys), 784);
        assert_eq!(size_of::<ProtectionKeysState>(), 8);
        assert_eq!(std::mem::offset_of!(RunState, xcr0), 792);
        assert_eq!(RunState::default().xcr0, 1);
        assert_eq!(RunState::reset().xcr0, 1);
        assert_eq!(size_of::<RunState>(), 800);
        let descriptor = RunState::descriptor();
        assert_eq!(descriptor["protectionKeys"]["offset"], 784);
        assert_eq!(descriptor["protectionKeys"]["size"], 8);
        assert_eq!(descriptor["protectionKeys"]["pkru"], 0);
        assert_eq!(descriptor["protectionKeys"]["pkrs"], 4);
        assert_eq!(descriptor["scalarSize"], 176);
        assert_eq!(REASONS.len(), 29);
        assert_eq!(
            RunState::default().protection_keys,
            ProtectionKeysState::ZERO
        );
        assert_eq!(RunState::reset().protection_keys, ProtectionKeysState::ZERO);
        let keys = ProtectionKeysState {
            pkru: 0x8000_0001,
            pkrs: u32::MAX,
        };
        let mut state = RunState {
            protection_keys: keys,
            ..RunState::default()
        };
        state.import(State::default());
        assert_eq!(state.protection_keys, keys);
    }
}

#[cfg(test)]
mod debug_append_tests {
    use super::*;
    #[test]
    fn debug_state_appends_without_moving_instruction_or_numeric_offsets() {
        assert_eq!(size_of::<RunState>(), 800);
        assert_eq!(RunState::descriptor()["debug"]["offset"], 616);
        let mut state = RunState::default();
        assert_eq!(RunState::descriptor()["xmm"], 176);
        assert_eq!(RunState::descriptor()["x87"]["offset"], 456);
        state.import(State::default());
        assert_eq!(state.debug.registers.dr6, 0xffff0ff0);
    }
}

#[cfg(test)]
mod paging_append_tests {
    use super::*;
    use crate::x86::context::ExecutionContext;
    use crate::x86::exception::ExceptionEvent;

    #[test]
    fn paging_source_appends_without_moving_existing_state_and_round_trips_frozen_faults() {
        assert_eq!(std::mem::offset_of!(RunState, paging_source), 712);
        assert_eq!(std::mem::offset_of!(RunState, debug), 616);
        assert_eq!(std::mem::offset_of!(RunState, event), 560);
        assert_eq!(size_of::<RunState>(), 800);
        let descriptor = RunState::descriptor();
        assert_eq!(descriptor["pagingSource"]["offset"], 712);
        assert_eq!(descriptor["pagingSource"]["bytes"], 16);
        assert_eq!(descriptor["pagingSource"]["address"], 0);
        assert_eq!(descriptor["pagingSource"]["hasAddress"], 8);
        assert_eq!(descriptor["pagingSource"]["reserved"], 12);
        assert_eq!(REASONS[PAGE_FAULT as usize], "page-fault");
        assert_eq!(REASONS[PHYSICAL_MEMORY as usize], "physical-memory-fault");
        let event = EventState::Pending(ExceptionEvent::page_fault(
            0x1000,
            0x202,
            &ExecutionContext::default(),
            6,
            0x501000,
        ));
        let mut state = RunState::default();
        state.set_event(event);
        assert_eq!(state.event_state(), Ok(event));
        assert_eq!(state.paging_source.address, 0x501000);
        assert_eq!(state.paging_source.has_address, 1);
        state.import(State::default());
        assert_eq!(state.paging_source, paging::RawPagingSource::default());
        state.set_event(event);
        state.set_event(EventState::Shutdown);
        assert_eq!(state.paging_source, paging::RawPagingSource::default());
    }

    #[test]
    fn page_fault_source_requires_matching_pending_vector_and_canonical_transport() {
        let pf = EventState::Pending(ExceptionEvent::page_fault(
            0x1000,
            2,
            &ExecutionContext::default(),
            0,
            0,
        ));
        let mut state = RunState {
            event: RawEventState::encode(pf),
            ..RunState::default()
        };
        assert!(state.event_state().is_err());
        state.set_event(pf);
        assert_eq!(state.event_state(), Ok(pf));
        state.debug.source =
            debug::DebugSource::encode(Some(crate::x86::debug::Source::GeneralDetect));
        assert!(state.event_state().is_err());
        for event in [
            EventState::Ready,
            EventState::Halted,
            EventState::Shutdown,
            EventState::Pending(ExceptionEvent::fault(
                13,
                0x1000,
                2,
                &ExecutionContext::default(),
                Some(0),
            )),
        ] {
            state.set_event(event);
            state.paging_source = paging::RawPagingSource {
                address: 0,
                has_address: 1,
                reserved: 0,
            };
            assert!(state.event_state().is_err(), "{event:?}");
        }
        for raw in [
            paging::RawPagingSource {
                address: 1,
                has_address: 0,
                reserved: 0,
            },
            paging::RawPagingSource {
                address: 0,
                has_address: 2,
                reserved: 0,
            },
            paging::RawPagingSource {
                address: 0,
                has_address: 1,
                reserved: 1,
            },
            paging::RawPagingSource {
                address: 0x0000_8000_0000_0000,
                has_address: 1,
                reserved: 0,
            },
        ] {
            assert!(raw.decode().is_err(), "{raw:?}");
        }
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&0xffff_8000_1234_5678u64.to_le_bytes());
        bytes[8] = 1;
        let raw = paging::RawPagingSource::from_bytes(&bytes).unwrap();
        assert_eq!(raw.decode(), Ok(Some(0xffff_8000_1234_5678)));
        assert!(paging::RawPagingSource::from_bytes(&bytes[..15]).is_err());
        bytes[12] = 1;
        assert!(paging::RawPagingSource::from_bytes(&bytes).is_err());
    }
}
