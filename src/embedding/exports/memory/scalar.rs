use super::BRIDGE;
use crate::embedding::layout::{ALIGNMENT, ALIGNMENT_CHECK, DATA, SEGMENT};
use crate::memory::Access;
use crate::x86::context::Segment;

#[unsafe(no_mangle)]
pub extern "C" fn core_load(address: u64, width: u32) -> u64 {
    core_load_impl::<false, false>(address, width)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_read_span(address: u64, width: u32, segment: i32) -> u32 {
    core_direct_span(address, width, segment, Access::Read)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[unsafe(no_mangle)]
pub extern "C" fn core_write_span(address: u64, width: u32, segment: i32) -> u32 {
    core_direct_span(address, width, segment, Access::Write)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn core_direct_span(address: u64, width: u32, segment: i32, access: Access) -> u32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        if segment >= 0
            && core
                .context
                .translate(
                    address,
                    width,
                    Segment::from_id(segment as u32),
                    access,
                    false,
                )
                .ok()
                != Some(address)
        {
            return 0;
        }
        let span = if access == Access::Write {
            core.image.direct_write_span(address, width)
        } else {
            core.image.direct_read_span(address, width)
        };
        let Some(mut span) = span else {
            return 0;
        };
        if segment >= 0 && !core.context.is_long64() {
            const END32: u64 = 1u64 << 32;
            let available = END32.saturating_sub(span.base);
            span.length = u64::from(span.length).min(available) as u32;
        }
        if access == Access::Read
            && core.context.mode() == crate::x86::context::Mode::Protected
            && core.context.code32()
            && (segment == Segment::Ds as i32 || segment == Segment::Ss as i32)
        {
            let slot = &mut core.state.flat_reads[usize::from(segment == Segment::Ss as i32)];
            *slot = span;
            return slot as *const crate::memory::DirectSpan as u32;
        }
        core.direct_span = span;
        &raw const core.direct_span as u32
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_load_observed(address: u64, width: u32) -> u64 {
    core_load_impl::<true, false>(address, width)
}

fn core_load_impl<const OBSERVED: bool, const PAGED: bool>(address: u64, width: u32) -> u64 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.fault = None;
        if PAGED {
            core.alignment_check = None;
        }
        match guest_access!(core, PAGED, read(address, width)) {
            Ok(value) => {
                if OBSERVED {
                    core.debug_operand(address, width, Access::Read);
                }
                value
            }
            Err(fault) => {
                core.access_failure(fault);
                unsafe {
                    (*core.state.get()).fault_segment = Segment::Ds as u32;
                }
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_load_for_write(address: u64, width: u32) -> u64 {
    core_load_for_write_impl::<false, false>(address, width)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_load_for_write_observed(address: u64, width: u32) -> u64 {
    core_load_for_write_impl::<true, false>(address, width)
}

fn core_load_for_write_impl<const OBSERVED: bool, const PAGED: bool>(
    address: u64,
    width: u32,
) -> u64 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.fault = None;
        if PAGED {
            core.alignment_check = None;
        }
        match guest_access!(core, PAGED, read_for_write(address, width)) {
            Ok(value) => {
                if OBSERVED {
                    core.debug_operand(address, width, Access::Read);
                }
                value
            }
            Err(fault) => {
                core.access_failure(fault);
                unsafe {
                    (*core.state.get()).fault_segment = Segment::Ds as u32;
                }
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_store(address: u64, width: u32, value: u64) -> i32 {
    core_store_impl::<false, false>(address, width, value)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_store_observed(address: u64, width: u32, value: u64) -> i32 {
    core_store_impl::<true, false>(address, width, value)
}

fn core_store_impl<const OBSERVED: bool, const PAGED: bool>(
    address: u64,
    width: u32,
    value: u64,
) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.fault = None;
        if PAGED {
            core.alignment_check = None;
        }
        match guest_access!(core, PAGED, write(address, width, value)) {
            Ok(()) => {
                if OBSERVED {
                    core.debug_operand(address, width, Access::Write);
                }
                if core.image.active_changed() { 2 } else { 1 }
            }
            Err(fault) => {
                core.access_failure(fault);
                unsafe {
                    (*core.state.get()).fault_segment = Segment::Ds as u32;
                }
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_access_failed() -> i32 {
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        i32::from(core.fault.is_some())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_segmented_access_failed() -> i32 {
    BRIDGE.with_borrow(|bridge| {
        let core = bridge.core.as_ref().unwrap();
        i32::from(core.fault.is_some() || core.segment_fault.is_some())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_load(address: u64, width: u32) -> u64 {
    core_load_impl::<false, true>(address, width)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_load_observed(address: u64, width: u32) -> u64 {
    core_load_impl::<true, true>(address, width)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_load_for_write(address: u64, width: u32) -> u64 {
    core_load_for_write_impl::<false, true>(address, width)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_load_for_write_observed(address: u64, width: u32) -> u64 {
    core_load_for_write_impl::<true, true>(address, width)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_store(address: u64, width: u32, value: u64) -> i32 {
    core_store_impl::<false, true>(address, width, value)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_store_observed(address: u64, width: u32, value: u64) -> i32 {
    core_store_impl::<true, true>(address, width, value)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_segmented_load(offset: u64, width: u32, segment: u32) -> u64 {
    core_segmented_load_impl::<false, false>(offset, width, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_segmented_load_observed(offset: u64, width: u32, segment: u32) -> u64 {
    core_segmented_load_impl::<true, false>(offset, width, segment)
}

fn core_segmented_load_impl<const OBSERVED: bool, const PAGED: bool>(
    offset: u64,
    width: u32,
    segment: u32,
) -> u64 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(address) = core.segment_address(
            offset,
            width,
            Segment::from_id(segment),
            Access::Read,
            false,
        ) else {
            return 0;
        };
        match guest_access!(core, PAGED, read(address, width)) {
            Ok(value) => {
                if OBSERVED {
                    core.debug_operand(address, width, Access::Read);
                }
                value
            }
            Err(fault) => {
                core.access_failure(fault);
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_segmented_load_for_write(offset: u64, width: u32, segment: u32) -> u64 {
    core_segmented_load_for_write_impl::<false, false>(offset, width, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_segmented_load_for_write_observed(
    offset: u64,
    width: u32,
    segment: u32,
) -> u64 {
    core_segmented_load_for_write_impl::<true, false>(offset, width, segment)
}

fn core_segmented_load_for_write_impl<const OBSERVED: bool, const PAGED: bool>(
    offset: u64,
    width: u32,
    segment: u32,
) -> u64 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(address) = core.segment_address(
            offset,
            width,
            Segment::from_id(segment),
            Access::Write,
            true,
        ) else {
            return 0;
        };
        match guest_access!(core, PAGED, read_for_write(address, width)) {
            Ok(value) => {
                if OBSERVED {
                    core.debug_operand(address, width, Access::Read);
                }
                value
            }
            Err(fault) => {
                core.access_failure(fault);
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_segmented_store(offset: u64, width: u32, value: u64, segment: u32) -> i32 {
    core_segmented_store_impl::<false, false>(offset, width, value, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_segmented_store_observed(
    offset: u64,
    width: u32,
    value: u64,
    segment: u32,
) -> i32 {
    core_segmented_store_impl::<true, false>(offset, width, value, segment)
}

fn core_segmented_store_impl<const OBSERVED: bool, const PAGED: bool>(
    offset: u64,
    width: u32,
    value: u64,
    segment: u32,
) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(address) = core.segment_address(
            offset,
            width,
            Segment::from_id(segment),
            Access::Write,
            false,
        ) else {
            return 0;
        };
        match guest_access!(core, PAGED, write(address, width, value)) {
            Ok(()) => {
                if OBSERVED {
                    core.debug_operand(address, width, Access::Write);
                }
                if core.image.active_changed() { 2 } else { 1 }
            }
            Err(fault) => {
                core.access_failure(fault);
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_access_reason() -> i32 {
    BRIDGE.with_borrow(|bridge| {
        if bridge.core.as_ref().unwrap().alignment_check.is_some() {
            ALIGNMENT_CHECK
        } else if bridge.core.as_ref().unwrap().alignment_fault.is_some() {
            ALIGNMENT
        } else if bridge.core.as_ref().unwrap().segment_fault.is_some() {
            SEGMENT
        } else {
            DATA
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_segmented_load(offset: u64, width: u32, segment: u32) -> u64 {
    core_segmented_load_impl::<false, true>(offset, width, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_segmented_load_observed(offset: u64, width: u32, segment: u32) -> u64 {
    core_segmented_load_impl::<true, true>(offset, width, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_segmented_load_for_write(
    offset: u64,
    width: u32,
    segment: u32,
) -> u64 {
    core_segmented_load_for_write_impl::<false, true>(offset, width, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_segmented_load_for_write_observed(
    offset: u64,
    width: u32,
    segment: u32,
) -> u64 {
    core_segmented_load_for_write_impl::<true, true>(offset, width, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_segmented_store(
    offset: u64,
    width: u32,
    value: u64,
    segment: u32,
) -> i32 {
    core_segmented_store_impl::<false, true>(offset, width, value, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_segmented_store_observed(
    offset: u64,
    width: u32,
    value: u64,
    segment: u32,
) -> i32 {
    core_segmented_store_impl::<true, true>(offset, width, value, segment)
}
