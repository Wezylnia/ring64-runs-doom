use super::BRIDGE;
use crate::embedding::layout::{
    RawEventState, RawInterruptState, RunState, SCALAR_STATE_SIZE, debug, numeric, paging,
};
use crate::x86::state::State;

#[unsafe(no_mangle)]
pub extern "C" fn core_state_ptr() -> u32 {
    BRIDGE.with_borrow(|bridge| bridge.core.as_ref().unwrap().state.get() as u32)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_import_state() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let event_end = SCALAR_STATE_SIZE + size_of::<RawEventState>();
        let interrupt_end = event_end + size_of::<RawInterruptState>();
        let debug_source_end = interrupt_end + size_of::<debug::DebugSource>();
        let debug_end = debug_source_end + size_of::<debug::Continuation>();
        let paging_end = debug_end + size_of::<paging::RawPagingSource>();
        let numeric_end = paging_end + size_of::<numeric::RawNumericSource>();
        let length = bridge.input.len();
        if ![
            SCALAR_STATE_SIZE,
            event_end,
            interrupt_end,
            debug_end,
            paging_end,
            numeric_end,
        ]
        .contains(&length)
        {
            return bridge.error("state input length differs");
        }
        let word =
            |offset| u64::from_le_bytes(bridge.input[offset..offset + 8].try_into().unwrap());
        let state = State {
            registers: std::array::from_fn(|i| {
                word(std::mem::offset_of!(RunState, registers) + i * 8)
            }),
            flags: word(std::mem::offset_of!(RunState, flags)),
            rip: word(std::mem::offset_of!(RunState, rip)),
        };
        let offset = std::mem::offset_of!(RunState, interruptibility);
        let interruptibility =
            u32::from_le_bytes(bridge.input[offset..offset + 4].try_into().unwrap());
        if let Err(message) = state.validate() {
            return bridge.error(message);
        }
        if !crate::x86::flags::valid_interruptibility(state.flags, interruptibility) {
            return bridge.error("invalid interruptibility for imported flags");
        }
        let event = if length == SCALAR_STATE_SIZE {
            RawEventState::default()
        } else {
            match RawEventState::from_bytes(&bridge.input[SCALAR_STATE_SIZE..event_end]) {
                Ok(raw) => raw,
                Err(message) => return bridge.error(message),
            }
        };
        let interrupts = if length > event_end {
            match RawInterruptState::from_bytes(&bridge.input[event_end..interrupt_end]) {
                Ok(raw) => raw,
                Err(message) => return bridge.error(message),
            }
        } else {
            RawInterruptState::default()
        };
        let decoded_interrupts = match interrupts.decode() {
            Ok(state) => state,
            Err(message) => return bridge.error(message),
        };
        let (source, continuation) = if length >= debug_end {
            let source = match debug::DebugSource::from_bytes(
                &bridge.input[interrupt_end..debug_source_end],
            ) {
                Ok(s) => s,
                Err(e) => return bridge.error(e),
            };
            let continuation =
                match debug::Continuation::from_bytes(&bridge.input[debug_source_end..debug_end]) {
                    Ok(s) => s,
                    Err(e) => return bridge.error(e),
                };
            (source, continuation)
        } else {
            (
                debug::DebugSource::default(),
                debug::Continuation::default(),
            )
        };
        let mut decoded = match event.decode() {
            Ok(event) => event,
            Err(message) => return bridge.error(message),
        };
        if let crate::x86::exception::EventState::Pending(ref mut pending) = decoded {
            pending.debug = source.decode().unwrap();
        } else if source != debug::DebugSource::default() {
            return bridge.error("debug source requires pending exception");
        }
        let paging_source = if length >= paging_end {
            match paging::RawPagingSource::from_bytes(&bridge.input[debug_end..paging_end]) {
                Ok(source) => source,
                Err(message) => return bridge.error(message),
            }
        } else {
            paging::RawPagingSource::default()
        };
        let numeric_source = if length == numeric_end {
            match numeric::RawNumericSource::from_bytes(&bridge.input[paging_end..numeric_end]) {
                Ok(source) => source,
                Err(message) => return bridge.error(message),
            }
        } else {
            Default::default()
        };
        if let Err(message) = numeric_source.attach(&mut decoded) {
            return bridge.error(message);
        }
        if let Err(message) = paging_source.attach(&mut decoded) {
            return bridge.error(message);
        }
        if matches!(decoded, crate::x86::exception::EventState::Pending(event) if event.vector==14)
            && bridge.core.as_ref().unwrap().paging.is_none()
        {
            return bridge.error("page fault source requires paged execution");
        }
        if let Err(message) = decoded.validate(
            state.rip,
            state.flags,
            &bridge.core.as_ref().unwrap().context,
            interruptibility,
            decoded_interrupts,
        ) {
            return bridge.error(message);
        }
        if let Err(message) = continuation.validate(
            interruptibility,
            decoded == crate::x86::exception::EventState::Ready,
        ) {
            return bridge.error(message);
        }
        let core = bridge.core.as_mut().unwrap();
        // generated execution is suspended for every export call. no reference to
        // this foreign-mutable buffer survives the call back into generated wasm.
        core.task_work = None;
        unsafe {
            let target = &mut *core.state.get();
            target.import(state);
            target.interruptibility = interruptibility;
            target.set_event(decoded);
            target.debug.continuation = continuation;
            target.interrupts = interrupts;
        };
        core.clear_instruction_faults();
        core.numeric_form = None;
        core.debug_exception = None;
        core.decode_fault = None;
        0
    })
}
