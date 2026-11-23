use std::cell::RefCell;

use super::layout::{
    DATA, FETCH, INVALID, RESOURCE, RawInterruptState, RunState, SEGMENT, StateStorage, TARGET,
    UNSUPPORTED,
};
use crate::dbt::{self, CompileError, Limits};
use crate::memory::{Access, Cause, Fault, Image};
use crate::x86::context::{AccessFailure, ExecutionContext, Segment, SegmentCause, SegmentFault};
use crate::x86::decode::{self, DecodeError};

macro_rules! guest_access {
    ($core:expr, $paged:expr, read($address:expr, $width:expr)) => {
        guest_access!($core, $paged, read_aligned($address, $width, crate::x86::alignment::Intent::Natural($width)))
    };
    ($core:expr, $paged:expr, read_for_write($address:expr, $width:expr)) => {
        guest_access!($core, $paged, read_for_write_aligned($address, $width, crate::x86::alignment::Intent::Natural($width)))
    };
    ($core:expr, $paged:expr, write($address:expr, $width:expr, $value:expr)) => {
        guest_access!($core, $paged, write_aligned($address, $width, $value, crate::x86::alignment::Intent::Natural($width)))
    };
    ($core:expr, $paged:expr, $method:ident($($argument:expr),* $(,)?)) => {{
        if $paged {
            let cpl = $core.context.cpl();
            let flags = unsafe { (*$core.state.get()).flags };
            let keys = unsafe { &(*$core.state.get()).protection_keys };
            let mut memory = crate::x86::paging::access::Memory::new(
                &mut $core.image, $core.paging.as_ref().expect("paged helper profile"),
                cpl, flags, &mut $core.paging_work, keys,
            ).with_cache(&$core.translation_cache);
            crate::x86::access::GuestMemory::$method(&mut memory, $($argument),*)
        } else {
            crate::x86::access::GuestMemory::$method(&mut $core.image, $($argument),*)
        }
    }};
}

macro_rules! guest_semantics {
    ($core:expr,$paged:expr,$operation:path,$($argument:expr),* $(,)?) => {{
        if $paged {
            let cpl=$core.context.cpl();
            let flags=unsafe { (*$core.state.get()).flags };
            let keys=unsafe { &(*$core.state.get()).protection_keys };
            let mut memory=crate::x86::paging::access::Memory::new(&mut $core.image,$core.paging.as_ref().expect("paged helper profile"),cpl,flags,&mut $core.paging_work,keys).with_cache(&$core.translation_cache);
            $operation(&mut $core.context,&mut memory,$($argument),*)
        } else {$operation(&mut $core.context,&mut $core.image,$($argument),*)}
    }};
}

#[path = "exports/execution/admission.rs"]
mod admission;
#[path = "exports/system/compound_stack.rs"]
mod compound_stack;
#[path = "exports/system/context.rs"]
mod context;
#[path = "exports/events/debug.rs"]
mod debug;
#[path = "exports/events/exception_delivery.rs"]
mod exception_delivery;
#[path = "exports/events/external_interrupts.rs"]
mod external_interrupts;
#[path = "exports/system/far_control.rs"]
mod far_control;
#[path = "exports/system/fast_system.rs"]
mod fast_system;
#[path = "exports/execution/invalidation.rs"]
mod invalidation;
#[path = "exports/system/io.rs"]
mod io;
#[path = "exports/memory/mapped.rs"]
mod mapped;
#[cfg(not(feature = "parallel"))]
#[path = "exports/memory/mmio.rs"]
mod mmio;
#[path = "exports/numeric/mmx.rs"]
mod mmx;
#[path = "exports/numeric/numeric.rs"]
mod numeric;
#[path = "exports/numeric/numeric_controls.rs"]
mod numeric_controls;
#[path = "exports/numeric/numeric_image/mod.rs"]
mod numeric_image;
#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[path = "exports/execution/paged_region.rs"]
mod paged_region;
#[path = "exports/memory/paging.rs"]
mod paging;
#[path = "exports/system/processor.rs"]
mod processor;
#[path = "exports/system/protection.rs"]
mod protection;
#[path = "exports/memory/protection_keys.rs"]
mod protection_keys;
#[path = "exports/execution/reset.rs"]
mod reset;
#[path = "exports/memory/scalar.rs"]
mod scalar;
#[path = "exports/events/software_events.rs"]
mod software_events;
#[path = "exports/execution/state.rs"]
mod state;
#[path = "exports/memory/streaming.rs"]
mod streaming;
#[path = "exports/events/system_flags.rs"]
mod system_flags;
#[path = "exports/system/task_switch.rs"]
mod task_switch;
#[path = "exports/execution/translation.rs"]
mod translation;
#[path = "exports/numeric/vector.rs"]
mod vector;
#[path = "exports/numeric/x87.rs"]
mod x87;
#[path = "exports/numeric/x87_state.rs"]
mod x87_state;

struct Core {
    task_work: Option<task_switch::Work>,
    io_present: bool,
    io_configured: bool,
    io_effect: bool,
    #[cfg(not(feature = "parallel"))]
    mmio_effect: bool,
    image: Image,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    direct_span: crate::memory::DirectSpan,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    return_read: crate::embedding::layout::ReturnRead,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    guarded_dword_pair: Option<paging::GuardedDwordPair>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_dword_write: crate::memory::IndexedDwordWrite,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_write_memo: crate::x86::paging::cache::IndexedWriteMemo,
    fast_system: crate::x86::fast_system::State,
    paging: Option<crate::x86::paging::State>,
    physical_controls: bool,
    paging_work: crate::x86::paging::access::AccessState,
    translation_cache: crate::x86::paging::cache::Cache,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_paged_read_root: Option<u64>,
    active_owner: Option<u64>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    paged_region_trace: Option<Box<paged_region::Trace>>,
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    guarded_walk: RefCell<paging::GuardedReads>,
    paging_failure: Option<crate::x86::paging::walk::Failure>,
    pdpte_load_fault: Option<(u8, u8, Fault)>,
    paging_system: Option<crate::x86::protection::SystemPagingFault>,
    state: Box<StateStorage>,
    fault: Option<Fault>,
    divide_failed: bool,
    decode_fault: Option<decode::DecodeFault>,
    context: ExecutionContext,
    context_version: u64,
    segment_fault: Option<SegmentFault>,
    alignment_fault: Option<vector::AlignmentFault>,
    alignment_check: Option<crate::x86::alignment::Fault>,
    simd_exception: Option<numeric::SimdException>,
    numeric_fault: Option<crate::x86::numeric_controls::Fault>,
    numeric_form: Option<(crate::x86::numeric_controls::Family, u64)>,
    general_protection: Option<numeric::GeneralProtectionFault>,
    protection_fault: Option<crate::x86::protection::ProtectionFault>,
    system_memory_fault: Option<crate::x86::protection::SystemMemoryFault>,
    debug_exception: Option<system_flags::DebugException>,
    translation_owners: dbt::Owners,
    debug_candidate: u8,
}

struct InitialImage {
    image: Image,
    paging: Option<crate::x86::paging::State>,
    fast_system: crate::x86::fast_system::State,
    context: ExecutionContext,
    physical_controls: bool,
}

impl InitialImage {
    fn linear(image: Image) -> Self {
        Self {
            image,
            paging: None,
            fast_system: Default::default(),
            context: Default::default(),
            physical_controls: false,
        }
    }
}

#[derive(Default)]
struct Bridge {
    core: Option<Core>,
    x87_operations: x87::DecodedOperations,
    input: Vec<u8>,
    output: Vec<u8>,
    info: Vec<u8>,
}

impl Bridge {
    fn install_core(&mut self, entry: u64, initial: InitialImage) -> i32 {
        let InitialImage {
            image,
            paging,
            fast_system,
            context,
            physical_controls,
        } = initial;
        self.core = Some(Core {
            task_work: None,
            io_present: false,
            io_configured: false,
            io_effect: false,
            #[cfg(not(feature = "parallel"))]
            mmio_effect: false,
            image,
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            direct_span: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            return_read: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            guarded_dword_pair: None,
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            indexed_dword_write: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            indexed_write_memo: Default::default(),
            fast_system,
            paging,
            physical_controls,
            paging_work: Default::default(),
            translation_cache: Default::default(),
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            indexed_paged_read_root: None,
            active_owner: None,
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            paged_region_trace: None,
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            guarded_walk: Default::default(),
            paging_failure: None,
            pdpte_load_fault: None,
            paging_system: None,
            state: Box::new(StateStorage::new(RunState {
                rip: entry,
                ..RunState::default()
            })),
            fault: None,
            divide_failed: false,
            decode_fault: None,
            context,
            context_version: 0,
            segment_fault: None,
            alignment_fault: None,
            alignment_check: None,
            simd_exception: None,
            numeric_fault: None,
            numeric_form: None,
            general_protection: None,
            protection_fault: None,
            system_memory_fault: None,
            debug_exception: None,
            translation_owners: Default::default(),
            debug_candidate: 0,
        });
        self.info = serde_json::to_vec(&RunState::descriptor()).unwrap();
        0
    }

    fn error(&mut self, message: &str) -> i32 {
        self.info = serde_json::to_vec(&serde_json::json!({ "error": message })).unwrap();
        1
    }
}

thread_local! {
    static BRIDGE: RefCell<Bridge> = RefCell::default();
}

fn fault_json(fault: &Fault) -> serde_json::Value {
    serde_json::json!({
        "address": format!("0x{:x}", fault.address),
        "width": fault.width,
        "access": match fault.access { Access::Read => "read", Access::Write => "write", Access::Execute => "execute" },
        "cause": match fault.cause { Cause::AddressRange => "address-range", Cause::Noncanonical => "noncanonical", Cause::Unmapped => "unmapped", Cause::Permission => "permission" },
        "failingAddress": format!("0x{:x}", fault.failing),
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_input(length: u32) -> u32 {
    BRIDGE.with_borrow_mut(|bridge| {
        bridge.input.resize(length as usize, 0);
        bridge.input.as_mut_ptr() as u32
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_init(code_base: u64, code_len: u32, ram_base: u64, ram_len: u32) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let Some(total) = code_len.checked_add(ram_len) else {
            return bridge.error("combined image exceeds wasm32 input size");
        };
        if bridge.input.len() != total as usize {
            return bridge.error("image input length differs");
        }
        let code = bridge.input[..code_len as usize].to_vec();
        let ram = bridge.input[code_len as usize..].to_vec();
        let image = match Image::new(code_base, code, ram_base, ram) {
            Ok(image) => image,
            Err(message) => return bridge.error(message),
        };
        bridge.install_core(code_base, InitialImage::linear(image))
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_clear_fault() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.paging_failure = None;
        core.pdpte_load_fault = None;
        core.paging_system = None;
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
        core.divide_failed = false;
        core.decode_fault = None;
        core.segment_fault = None;
        core.debug_exception = None;
        unsafe {
            (*core.state.get()).target = 0;
            (*core.state.get()).fault_segment = Segment::Cs as u32;
        };
        i32::from(core.boundary_observed())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_multiply_high(left: u64, right: u64, signed: i32) -> u64 {
    super::arithmetic::multiply_high(left, right, signed != 0)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_divide(high: u64, low: u64, divisor: u64, signed: i32) -> u64 {
    let result = super::arithmetic::divide(high, low, divisor, signed != 0);
    BRIDGE.with_borrow_mut(|bridge| {
        bridge.core.as_mut().unwrap().divide_failed = result.is_none();
    });
    result.unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_divide_failed() -> i32 {
    BRIDGE.with_borrow(|bridge| i32::from(bridge.core.as_ref().unwrap().divide_failed))
}

#[unsafe(no_mangle)]
pub extern "C" fn core_fault_info(reason: i32) {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        let fault = match reason {
            super::layout::PAGE_FAULT | super::layout::PHYSICAL_MEMORY => {
                core.paging_failure_json()
            }
            super::layout::DEBUG_EXCEPTION => core
                .debug_exception
                .as_ref()
                .map(system_flags::DebugException::json),
            super::layout::X87_EXCEPTION => unsafe {
                x87_state::exception_json(&(*core.state.get()).x87)
            },
            super::layout::SIMD_EXCEPTION => core
                .simd_exception
                .as_ref()
                .map(numeric::SimdException::json),
            super::layout::GENERAL_PROTECTION => core
                .general_protection
                .as_ref()
                .map(numeric::GeneralProtectionFault::json),
            super::layout::ALIGNMENT => core
                .alignment_fault
                .as_ref()
                .map(vector::AlignmentFault::json),
            super::layout::ALIGNMENT_CHECK => core.alignment_check.map(|fault| {
                let state=unsafe { &*core.state.get() };
                let segment=Segment::from_id(state.fault_segment);
                let offset=fault.address.wrapping_sub(core.context.segment_base(segment));
                let offset=if core.context.is_long64() { offset } else { u64::from(offset as u32) };
                let code=match state.event_state() { Ok(crate::x86::exception::EventState::Pending(event)) if event.vector==17 => event.error_code.unwrap_or(0), _=>0 };
                serde_json::json!({ "segment":segment.name(), "offset":format!("0x{offset:x}"), "address":format!("0x{:x}",fault.address), "width":fault.width,
                    "access":if fault.access==Access::Write { "write" } else { "read" }, "alignment":fault.alignment, "vector":17,"errorCode":code })
            }),
            super::layout::PROTECTION => core.protection_fault.as_ref().map(protection::fault_json),
            super::layout::SYSTEM_MEMORY => core
                .system_memory_fault
                .as_ref()
                .map(protection::system_fault_json),
            SEGMENT => core.segment_fault.as_ref().map(context::segment_fault_json),
            DATA | FETCH => core.fault.as_ref().map(fault_json),
            TARGET => {
                let target = unsafe { (*core.state.get()).target };
                Some(fault_json(&Fault {
                    address: target,
                    width: 0,
                    access: Access::Execute,
                    cause: Cause::Noncanonical,
                    failing: u128::from(target),
                }))
            }
            _ => None,
        };
        let fault = fault.map(|mut value| {
            if matches!(reason, DATA | FETCH | TARGET) {
                let segment = if reason == TARGET {
                    Segment::Cs
                } else {
                    Segment::from_id(unsafe { (*core.state.get()).fault_segment })
                };
                value["segment"] = serde_json::json!(segment.name());
            }
            value
        });
        bridge.info = serde_json::to_vec(&fault).unwrap();
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_output_ptr() -> u32 {
    BRIDGE.with_borrow(|bridge| bridge.output.as_ptr() as u32)
}
#[unsafe(no_mangle)]
pub extern "C" fn core_output_len() -> u32 {
    BRIDGE.with_borrow(|bridge| bridge.output.len() as u32)
}
#[unsafe(no_mangle)]
pub extern "C" fn core_info_ptr() -> u32 {
    BRIDGE.with_borrow(|bridge| bridge.info.as_ptr() as u32)
}
#[unsafe(no_mangle)]
pub extern "C" fn core_info_len() -> u32 {
    BRIDGE.with_borrow(|bridge| bridge.info.len() as u32)
}
#[unsafe(no_mangle)]
pub extern "C" fn core_dispose() {
    BRIDGE.with_borrow_mut(|bridge| *bridge = Bridge::default());
}

#[cfg(test)]
mod tests;

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
#[path = "exports/memory/parallel.rs"]
mod parallel;
