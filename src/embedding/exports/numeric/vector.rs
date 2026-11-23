use crate::embedding::exports::*;

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn indexed_vector_width<const OBSERVED: bool, const PAGED: bool>(width: u32) -> (u32, bool) {
    use crate::embedding::layout::paging::INDEXED_VECTOR_ACCESS;
    let indexed = OBSERVED && PAGED && width & INDEXED_VECTOR_ACCESS != 0;
    (
        if indexed {
            width & !INDEXED_VECTOR_ACCESS
        } else {
            width
        },
        indexed,
    )
}

pub(in crate::embedding::exports) struct AlignmentFault {
    pub(in crate::embedding::exports) segment: Segment,
    pub(in crate::embedding::exports) offset: u64,
    pub(in crate::embedding::exports) address: u64,
    pub(in crate::embedding::exports) width: u32,
    pub(in crate::embedding::exports) access: Access,
    pub(in crate::embedding::exports) alignment: u32,
}

impl AlignmentFault {
    pub(in crate::embedding::exports) fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "segment":self.segment.name(),
            "offset":format!("0x{:x}",self.offset),
            "address":format!("0x{:x}",self.address),
            "width":self.width,
            "access":match self.access {Access::Read=>"read",Access::Write=>"write",Access::Execute=>unreachable!()},
            "alignment":self.alignment,
        })
    }
}

impl Core {
    pub(in crate::embedding::exports) fn vector_address(
        &mut self,
        offset: u64,
        width: u32,
        segment: u32,
        alignment: u32,
        access: Access,
    ) -> Option<u64> {
        assert!(matches!(width, 2 | 4 | 8 | 16));
        assert!(alignment == 1 || alignment == 16 && width == 16);
        self.alignment_fault = None;
        self.alignment_check = None;
        let segment = Segment::from_id(segment);
        let address = self.segment_address(offset, width, segment, access, false)?;
        if address & u64::from(alignment - 1) != 0 {
            self.alignment_fault = Some(AlignmentFault {
                segment,
                offset,
                address,
                width,
                access,
                alignment,
            });
            None
        } else {
            Some(address)
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_import_xmm() -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        if bridge.input.len() != 256 {
            return bridge.error("XMM state input length differs");
        }
        let bank = std::array::from_fn(|index| {
            bridge.input[index * 16..index * 16 + 16]
                .try_into()
                .unwrap()
        });
        let core = bridge.core.as_mut().unwrap();
        unsafe {
            let state = &mut *core.state.get();
            state.xmm = bank;
            state.target = 0;
            state.fault_segment = Segment::Cs as u32;
        }
        core.clear_instruction_faults();
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_vector_load(offset: u64, width: u32, segment: u32, alignment: u32) -> u32 {
    core_vector_load_impl::<false, false>(offset, width, segment, alignment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_vector_load_observed(
    offset: u64,
    width: u32,
    segment: u32,
    alignment: u32,
) -> u32 {
    core_vector_load_impl::<true, false>(offset, width, segment, alignment)
}

fn core_vector_load_impl<const OBSERVED: bool, const PAGED: bool>(
    offset: u64,
    width: u32,
    segment: u32,
    alignment: u32,
) -> u32 {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let (width, indexed) = indexed_vector_width::<OBSERVED, PAGED>(width);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(address) = core.vector_address(offset, width, segment, alignment, Access::Read)
        else {
            return 0;
        };
        // This helper returns a pointer in the private memory containing vector_scratch.
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32", not(guest_memory64)))]
        if !OBSERVED
            && !PAGED
            && width == 16
            && alignment == 1
            && let Some(span) = core.image.direct_read_span(address, width)
        {
            // The generated instruction consumes all 16 bytes before any other helper.
            return span.host + (address - span.base) as u32;
        }
        match guest_access!(
            core,
            PAGED,
            read_vector_aligned(address, width, crate::x86::alignment::Intent::vector(width))
        ) {
            Ok(bytes) => unsafe {
                if OBSERVED {
                    core.debug_operand(address, width, Access::Read);
                }
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if indexed {
                    core.remember_indexed_scalar_read(address, width);
                }
                let state = &mut *core.state.get();
                state.vector_scratch = bytes;
                state.vector_scratch.as_ptr() as u32
            },
            Err(fault) => {
                core.access_failure(fault);
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_block16_for_write(offset: u64, segment: u32) -> u32 {
    block16_for_write::<false>(offset, segment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_block16_for_write(offset: u64, segment: u32) -> u32 {
    block16_for_write::<true>(offset, segment)
}

fn block16_for_write<const PAGED: bool>(offset: u64, segment: u32) -> u32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(address) = core.vector_address(offset, 16, segment, 16, Access::Write) else {
            return 0;
        };
        let mut bytes = [0; 16];
        match guest_access!(
            core,
            PAGED,
            read_block16_for_write_aligned(address, &mut bytes)
        ) {
            Ok(()) => unsafe {
                // Both comparison outcomes publish an observed write to this
                // entire span. It covers both write and read/write breakpoints.
                let state = &mut *core.state.get();
                state.vector_scratch = bytes;
                state.vector_scratch.as_ptr() as u32
            },
            Err(fault) => {
                core.access_failure(fault);
                0
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_vector_store(
    offset: u64,
    width: u32,
    segment: u32,
    alignment: u32,
    value_ptr: u32,
) -> i32 {
    core_vector_store_impl::<false, false>(offset, width, segment, alignment, value_ptr)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_vector_store_observed(
    offset: u64,
    width: u32,
    segment: u32,
    alignment: u32,
    value_ptr: u32,
) -> i32 {
    core_vector_store_impl::<true, false>(offset, width, segment, alignment, value_ptr)
}

fn core_vector_store_impl<const OBSERVED: bool, const PAGED: bool>(
    offset: u64,
    width: u32,
    segment: u32,
    alignment: u32,
    value_ptr: u32,
) -> i32 {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let (width, indexed) = indexed_vector_width::<OBSERVED, PAGED>(width);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let bytes = unsafe {
            let state = &*core.state.get();
            assert_eq!(
                state.vector_scratch.as_ptr() as u32,
                value_ptr,
                "invalid vector scratch pointer"
            );
            state.vector_scratch
        };
        let Some(address) = core.vector_address(offset, width, segment, alignment, Access::Write)
        else {
            return 0;
        };
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if !OBSERVED
            && !PAGED
            && width == 16
            && alignment == 1
            && core.image.direct_write_vector(address, &bytes)
        {
            return if core.image.active_changed() { 2 } else { 1 };
        }
        match guest_access!(
            core,
            PAGED,
            write_bytes_aligned(
                address,
                &bytes[..width as usize],
                crate::x86::alignment::Intent::vector(width)
            )
        ) {
            Ok(()) => {
                if OBSERVED {
                    core.debug_operand(address, width, Access::Write);
                }
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if indexed {
                    core.remember_indexed_scalar_write(address, width);
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
pub extern "C" fn core_paged_vector_load(
    offset: u64,
    width: u32,
    segment: u32,
    alignment: u32,
) -> u32 {
    core_vector_load_impl::<false, true>(offset, width, segment, alignment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_vector_load_observed(
    offset: u64,
    width: u32,
    segment: u32,
    alignment: u32,
) -> u32 {
    core_vector_load_impl::<true, true>(offset, width, segment, alignment)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_vector_store(
    offset: u64,
    width: u32,
    segment: u32,
    alignment: u32,
    value_ptr: u32,
) -> i32 {
    core_vector_store_impl::<false, true>(offset, width, segment, alignment, value_ptr)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_vector_store_observed(
    offset: u64,
    width: u32,
    segment: u32,
    alignment: u32,
    value_ptr: u32,
) -> i32 {
    core_vector_store_impl::<true, true>(offset, width, segment, alignment, value_ptr)
}
