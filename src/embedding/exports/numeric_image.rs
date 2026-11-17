use super::*;

mod extended;
use crate::embedding::layout::{ALIGNMENT, GENERAL_PROTECTION};
use crate::x86::numeric_images::{self, ImageOp};

#[unsafe(no_mangle)]
pub extern "C" fn core_numeric_image(operation: u32, offset: u64, segment: u32) -> i32 {
    core_numeric_image_impl::<false, false>(operation, offset, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_numeric_image_observed(operation: u32, offset: u64, segment: u32) -> i32 {
    core_numeric_image_impl::<true, false>(operation, offset, segment)
}

fn core_numeric_image_impl<const OBSERVED: bool, const PAGED: bool>(
    operation: u32,
    offset: u64,
    segment: u32,
) -> i32 {
    let Some(operation) = ImageOp::try_decode(operation) else {
        return -1;
    };
    let Some(segment) = Segment::ALL.get(segment as usize).copied() else {
        return -1;
    };
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        core.fault = None;
        core.segment_fault = None;
        core.alignment_fault = None;
        core.alignment_check = None;
        core.simd_exception = None;
        core.numeric_fault = None;
        core.general_protection = None;
        core.protection_fault = None;
        core.system_memory_fault = None;
        core.divide_failed = false;
        unsafe {
            let state = &mut *core.state.get();
            state.target = 0;
            state.fault_segment = Segment::Cs as u32;
        }
        if operation.is_xstate() {
            return extended::execute::<OBSERVED, PAGED>(core, operation, offset, segment);
        }
        if operation.is_waiting() && unsafe { (*core.state.get()).x87.pending() } != 0 {
            return core.pending_x87_reason();
        }
        let width = operation.width();
        let access = if operation.is_store() {
            Access::Write
        } else {
            Access::Read
        };
        let Some(address) = core.segment_address(offset, width, segment, access, false) else {
            return if core.segment_fault.is_some() {
                SEGMENT
            } else {
                DATA
            };
        };
        let alignment = operation.alignment();
        if address & u64::from(alignment - 1) != 0 {
            core.alignment_fault = Some(vector::AlignmentFault {
                segment,
                offset,
                address,
                width,
                access,
                alignment,
            });
            return ALIGNMENT;
        }
        if operation.is_store() {
            let saved = unsafe {
                let state = &*core.state.get();
                numeric_images::save(operation, &state.x87, &state.xmm, state.mxcsr)
            };
            if let Err(fault) = guest_access!(
                core,
                PAGED,
                write_fields_aligned(
                    address,
                    width,
                    operation.spans(),
                    &saved.bytes,
                    operation.alignment_intent()
                )
            ) {
                core.access_failure(fault);
                return core.memory_failure_reason();
            }
            unsafe {
                (*core.state.get()).x87 = saved.x87;
            }
            if OBSERVED {
                for &(offset, length) in operation.spans() {
                    core.debug_operand(address + u64::from(offset), length, Access::Write);
                }
            }
            if core.image.active_changed() { 2 } else { 0 }
        } else {
            let mut bytes = [0; 512];
            if let Err(fault) = guest_access!(
                core,
                PAGED,
                read_fields_aligned(
                    address,
                    width,
                    operation.spans(),
                    &mut bytes,
                    operation.alignment_intent()
                )
            ) {
                core.access_failure(fault);
                return core.memory_failure_reason();
            }
            let state = unsafe { &mut *core.state.get() };
            if let Err(loaded) = numeric_images::restore(
                operation,
                &bytes,
                &mut state.x87,
                &mut state.xmm,
                &mut state.mxcsr,
            ) {
                core.general_protection = Some(numeric::GeneralProtectionFault { loaded });
                return GENERAL_PROTECTION;
            }
            if OBSERVED {
                for &(offset, length) in operation.spans() {
                    core.debug_operand(address + u64::from(offset), length, Access::Read);
                }
            }
            0
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_numeric_image(operation: u32, offset: u64, segment: u32) -> i32 {
    core_numeric_image_impl::<false, true>(operation, offset, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_numeric_image_observed(
    operation: u32,
    offset: u64,
    segment: u32,
) -> i32 {
    core_numeric_image_impl::<true, true>(operation, offset, segment)
}
