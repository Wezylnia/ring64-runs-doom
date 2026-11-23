use crate::embedding::exports::*;
use crate::embedding::layout::X87_EXCEPTION;
use crate::x86::context::Mode;
use crate::x86::x87::{self, Op};

const DECODED_OPERATION_CAPACITY: usize = 128;

pub(in crate::embedding::exports) struct DecodedOperations(
    [Option<(u32, Op)>; DECODED_OPERATION_CAPACITY],
);

impl Default for DecodedOperations {
    fn default() -> Self {
        Self([None; DECODED_OPERATION_CAPACITY])
    }
}

impl DecodedOperations {
    fn index(word: u32) -> usize {
        (word.wrapping_mul(0x9e37_79b9) >> (32 - DECODED_OPERATION_CAPACITY.ilog2())) as usize
    }

    fn get(&mut self, word: u32) -> Option<Op> {
        let entry = &mut self.0[Self::index(word)];
        if let Some((cached, operation)) = *entry
            && cached == word
        {
            return Some(operation);
        }
        let operation = Op::try_decode(word)?;
        *entry = Some((word, operation));
        Some(operation)
    }
}

fn saved_pointer(context: &ExecutionContext, offset: u64, segment: Segment) -> u64 {
    match context.mode() {
        Mode::Long64 => offset,
        Mode::Real | Mode::Virtual8086 => {
            context.segment_base(segment).wrapping_add(offset) as u32 as u64
        }
        Mode::Protected | Mode::Compatibility => offset as u32 as u64,
    }
}

fn x87_environment(
    context: &ExecutionContext,
    state: &mut x87::State,
    operation: Op,
    pc: u64,
    data: Option<(u64, Segment)>,
) {
    if operation.updates_environment() {
        state.instruction_pointer = saved_pointer(context, pc, Segment::Cs);
        if let Some((offset, segment)) = data {
            state.data_pointer = saved_pointer(context, offset, segment);
        }
        if state.pending() != 0 {
            state.opcode = operation.encoding;
        }
    }
}

fn clear_x87_diagnostics(core: &mut Core) {
    core.clear_instruction_faults();
    unsafe {
        let state = &mut *core.state.get();
        state.target = 0;
        state.fault_segment = Segment::Cs as u32;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn core_x87(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    #[cfg(not(feature = "parallel"))]
    if segment == x87::REGISTER_PAIR_SEGMENT {
        return core_x87_register_pair(operation, offset, instruction_pc, rax, flags);
    }
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    if segment & x87::MEMORY_PAIR_SEGMENT != 0 {
        return core_x87_load_memory_pair(
            operation,
            flags,
            offset,
            segment ^ x87::MEMORY_PAIR_SEGMENT,
            rax,
        );
    }
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    if segment & x87::MEMORY_FIRST_PAIR_SEGMENT != 0 {
        return core_x87_memory_first_pair(
            operation,
            rax,
            segment ^ x87::MEMORY_FIRST_PAIR_SEGMENT,
            [offset, flags],
            instruction_pc,
        );
    }
    core_x87_impl::<false, false>(operation, offset, segment, instruction_pc, flags, rax)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline(always)]
fn flat_pair_segment(context: &ExecutionContext, segment: Segment) -> bool {
    use crate::x86::context::SegmentType;
    // This helper dereferences the retained address as a private Rust pointer.
    if cfg!(guest_memory64) || !matches!(segment, Segment::Ds | Segment::Ss) {
        return false;
    }
    match context.mode() {
        // Long64 ignores both segment bases and limits for these two segments.
        Mode::Long64 => return true,
        Mode::Protected => {}
        _ => return false,
    }
    let loaded = context.segment(segment);
    loaded.base == 0
        && loaded.limit == u32::MAX
        && loaded.usable
        && loaded.kind == SegmentType::ReadWrite
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline(always)]
fn flat_pair_pointer(
    span: &mut crate::memory::DirectSpan,
    offset: u64,
    width: u32,
    long64: bool,
    refill: impl FnOnce() -> Option<crate::memory::DirectSpan>,
) -> Option<u32> {
    const END32: u64 = 1 << 32;
    if !long64 && offset > END32 - u64::from(width) {
        return None;
    }
    // The installed mapping is canonical over its entire range. Containment
    // proves all operand bytes lie in it, including at the top of Long64.
    let relative = offset.wrapping_sub(span.base);
    if width <= span.length && relative <= u64::from(span.length - width) {
        return Some(span.host + relative as u32);
    }
    let mut next = refill()?;
    if !long64 {
        next.length = u64::from(next.length).min(END32 - next.base) as u32;
    }
    *span = next;
    Some(next.host + (offset - next.base) as u32)
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[inline(always)]
fn read_pair_operand(core: &mut Core, offset: u64, width: u32, segment: Segment) -> Option<u64> {
    #[cfg(target_arch = "wasm32")]
    if flat_pair_segment(&core.context, segment) {
        let long64 = core.context.is_long64();
        let spans = if long64 {
            &mut core.state.x87_long64_reads
        } else {
            &mut core.state.flat_reads
        };
        let slot = usize::from(segment == Segment::Ss);
        if let Some(host) = flat_pair_pointer(&mut spans[slot], offset, width, long64, || {
            core.image.direct_read_span(offset, width)
        }) {
            // The retained span proves every byte in the current image mapping.
            return Some(unsafe {
                if width == 8 {
                    std::ptr::read_unaligned(host as *const u64)
                } else {
                    u64::from(std::ptr::read_unaligned(host as *const u32))
                }
            });
        }
    }
    let linear = core
        .context
        .translate(offset, width, segment, Access::Read, false)
        .ok()?;
    core.image.read(linear, width).ok()
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[inline(never)]
fn core_x87_memory_first_pair(
    first: u32,
    second: u64,
    segments: u32,
    offsets: [u64; 2],
    pc: u64,
) -> i32 {
    let (Ok(second), Some(&head_segment), Some(&tail_segment)) = (
        u32::try_from(second),
        Segment::ALL.get((segments & 7) as usize),
        Segment::ALL.get((segments >> 3) as usize),
    ) else {
        return -1;
    };
    BRIDGE.with_borrow_mut(|bridge| {
        let (Some(first), Some(second)) = (
            bridge.x87_operations.get(first),
            bridge.x87_operations.get(second),
        ) else {
            return -1;
        };
        if !x87::memory_first_pair(first, second) {
            return -1;
        }
        let core = bridge.core.as_mut().unwrap();
        if core.paging.is_some() || unsafe { (*core.state.get()).x87.pending() } != 0 {
            return -1;
        }
        // Both operands must be available before publishing any instruction effects.
        let width = first.memory_bytes();
        let Some(source) = read_pair_operand(core, offsets[0], width, head_segment) else {
            return -1;
        };
        let run = unsafe { &mut *core.state.get() };
        let (data, diagnostic_segment) = if second.is_store() {
            let width = second.memory_bytes();
            #[cfg(target_arch = "wasm32")]
            let direct_write = if flat_pair_segment(&core.context, tail_segment) {
                let long64 = core.context.is_long64();
                let stack = tail_segment == Segment::Ss;
                let slot = if long64 {
                    &mut core.state.x87_long64_writes[usize::from(stack)]
                } else if stack {
                    &mut core.state.flat_stack_write
                } else {
                    &mut core.state.flat_data_write
                };
                flat_pair_pointer(slot, offsets[1], width, long64, || {
                    core.image.direct_write_span(offsets[1], width)
                })
            } else {
                None
            };
            #[cfg(not(target_arch = "wasm32"))]
            let direct_write: Option<u32> = None;
            let write = if direct_write.is_none() {
                let Ok(tail) =
                    core.context
                        .translate(offsets[1], width, tail_segment, Access::Write, false)
                else {
                    return -1;
                };
                let Ok(write) = core.image.prepare_linear_write(tail, width) else {
                    return -1;
                };
                Some(write)
            } else {
                None
            };
            let Some(value) = run.x87.memory_arithmetic_store(first, source, second) else {
                return -1;
            };
            if let Some(value) = value {
                if let Some(host) = direct_write {
                    // Retained writable spans exclude executable aliases and
                    // are cleared when source dependencies or mappings change.
                    unsafe {
                        if width == 8 {
                            std::ptr::write_unaligned(host as *mut u64, value);
                        } else {
                            std::ptr::write_unaligned(host as *mut u32, value as u32);
                        }
                    }
                } else {
                    write
                        .expect("admitted x87 write")
                        .publish(&value.to_le_bytes()[..width as usize]);
                }
            }
            ((offsets[1], tail_segment), tail_segment)
        } else {
            if !run
                .x87
                .load_memory_register(first.format, source, second.index)
            {
                return -1;
            }
            ((offsets[0], head_segment), Segment::Cs)
        };
        // The first instruction cannot leave a pending exception in an admitted pair.
        x87_environment(&core.context, &mut run.x87, second, pc, Some(data));
        clear_x87_diagnostics(core);
        unsafe { (*core.state.get()).fault_segment = diagnostic_segment as u32 };
        if second.is_store() && core.image.active_changed() {
            2
        } else {
            0
        }
    })
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
#[inline(never)]
fn core_x87_load_memory_pair(first: u32, second: u64, offset: u64, segment: u32, pc: u64) -> i32 {
    let (Ok(second), Some(&segment)) = (u32::try_from(second), Segment::ALL.get(segment as usize))
    else {
        return -1;
    };
    BRIDGE.with_borrow_mut(|bridge| {
        let (Some(first), Some(second)) = (
            bridge.x87_operations.get(first),
            bridge.x87_operations.get(second),
        ) else {
            return -1;
        };
        if !x87::register_load_memory_arithmetic(first, second) {
            return -1;
        }
        let core = bridge.core.as_mut().unwrap();
        let state = unsafe { &(*core.state.get()).x87 };
        if core.paging.is_some() || !state.can_load_register(first.index) {
            return -1;
        }
        // Capture the second operand without publishing diagnostics or changing x87.
        // On refusal, the ordinary entries execute the load then expose the fault.
        let Some(bits) = read_pair_operand(core, offset, second.memory_bytes(), segment) else {
            return -1;
        };
        clear_x87_diagnostics(core);
        let run = unsafe { &mut *core.state.get() };
        run.fault_segment = segment as u32;
        run.x87.load_memory_arithmetic(first.index, second, bits);
        x87_environment(
            &core.context,
            &mut run.x87,
            second,
            pc,
            Some((offset, segment)),
        );
        0
    })
}

#[cfg(not(feature = "parallel"))]
fn core_x87_register_pair(
    first: u32,
    second: u64,
    first_pc: u64,
    second_pc: u64,
    flags: u64,
) -> i32 {
    let Ok(second) = u32::try_from(second) else {
        return -1;
    };
    BRIDGE.with_borrow_mut(|bridge| {
        let Some(first) = bridge.x87_operations.get(first) else {
            return -1;
        };
        let Some(second) = bridge.x87_operations.get(second) else {
            return -1;
        };
        let compare = x87::register_exchange_compare(first, second);
        let pop = x87::register_store_pop(first) && x87::register_store_pop(second);
        if !compare && (flags != 0 || !(pop || x87::register_load_arithmetic(first, second))) {
            return -1;
        }
        let core = bridge.core.as_mut().unwrap();
        let state = unsafe { &(*core.state.get()).x87 };
        let can_start = if compare {
            state.can_exchange_register(first.index)
        } else if pop {
            state.can_store_register_pop_pair(first.index)
        } else {
            state.can_load_register(first.index)
        };
        if core.paging.is_some() || !can_start {
            return -1;
        }
        clear_x87_diagnostics(core);
        let run = unsafe { &mut *core.state.get() };
        if pop {
            run.x87.store_register_pop(first.index);
            run.x87.store_register_pop(second.index);
            // Neither admitted pop raises an exception. Only the final FIP survives.
            x87_environment(&core.context, &mut run.x87, second, second_pc, None);
            return 0;
        }
        if compare
            && let Some(flags) =
                x87::admitted_exchange_compare(&mut run.x87, first.index, second, flags)
        {
            run.flags = flags;
            x87_environment(&core.context, &mut run.x87, second, second_pc, None);
            return 0;
        }
        // An admitted register move cannot raise a new exception. The second
        // numeric operation still produces its original exception response.
        x87::eval(&mut run.x87, first, 0, 0);
        x87_environment(&core.context, &mut run.x87, first, first_pc, None);
        let effect = x87::eval(&mut run.x87, second, 0, flags);
        x87_environment(&core.context, &mut run.x87, second, second_pc, None);
        if compare {
            run.flags = effect.flags;
        }
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_x87_observed(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    core_x87_impl::<true, false>(operation, offset, segment, instruction_pc, flags, rax)
}

fn core_x87_impl<const OBSERVED: bool, const PAGED: bool>(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    let Some(segment) = Segment::ALL.get(segment as usize).copied() else {
        return -1;
    };
    BRIDGE.with_borrow_mut(|bridge| {
        let Some(operation) = bridge.x87_operations.get(operation) else {
            return -1;
        };
        let core = bridge.core.as_mut().unwrap();
        clear_x87_diagnostics(core);
        let pending = unsafe { (*core.state.get()).x87.pending() };
        if operation.is_waiting() && pending != 0 {
            return core.pending_x87_reason();
        }
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        if !OBSERVED && !PAGED && operation.format == x87::Format::Register {
            let state = unsafe { &mut (*core.state.get()).x87 };
            let handled = match operation.kind {
                x87::Kind::Store if operation.pop == 1 => state.store_register_pop(operation.index),
                x87::Kind::Sub if !operation.dest_index && operation.pop == 0 => {
                    x87::subtract_register(state, operation.index)
                }
                _ => false,
            };
            if handled {
                x87_environment(&core.context, state, operation, instruction_pc, None);
                return 0;
            }
        }
        let store = operation.is_store();
        let private_linear = !PAGED && !cfg!(feature = "parallel");
        let width = operation.memory_bytes();
        let mut address = None;
        let mut linear_write = None;
        let source = if width == 0 {
            0
        } else {
            let access = if store { Access::Write } else { Access::Read };
            let Some(linear) = core.segment_address(offset, width, segment, access, false) else {
                return if core.segment_fault.is_some() {
                    SEGMENT
                } else {
                    DATA
                };
            };
            address = Some(linear);
            if store {
                if private_linear {
                    match core.image.prepare_linear_write(linear, width) {
                        Ok(write) => linear_write = Some(write),
                        Err(fault) => {
                            core.access_failure(AccessFailure::Linear(fault));
                            return core.memory_failure_reason();
                        }
                    }
                } else if let Err(fault) = guest_access!(
                    core,
                    PAGED,
                    check_write_aligned(linear, width, operation.alignment_intent())
                ) {
                    core.access_failure(fault);
                    return core.memory_failure_reason();
                }
                0
            } else {
                match guest_access!(
                    core,
                    PAGED,
                    read_vector_aligned(linear, width, operation.alignment_intent())
                ) {
                    Ok(bytes) => u128::from_le_bytes(bytes),
                    Err(fault) => {
                        core.access_failure(fault);
                        return core.memory_failure_reason();
                    }
                }
            }
        };
        // A prepared private write holds the complete admission through publication.
        let mut staged = (store && !private_linear).then(|| unsafe { (*core.state.get()).x87 });
        let effect = {
            let candidate = match staged.as_mut() {
                Some(candidate) => candidate,
                None => unsafe { &mut (*core.state.get()).x87 },
            };
            let effect = x87::eval(candidate, operation, source, flags);
            x87_environment(
                &core.context,
                candidate,
                operation,
                instruction_pc,
                address.map(|_| (offset, segment)),
            );
            effect
        };
        match (effect.store, private_linear, linear_write) {
            (true, true, write) => write
                .expect("prepared linear x87 store")
                .publish(&effect.value.to_le_bytes()[..width as usize]),
            (true, false, _) => {
                let linear = address.expect("x87 memory store address");
                if let Err(fault) = guest_access!(
                    core,
                    PAGED,
                    write_bytes_aligned(
                        linear,
                        &effect.value.to_le_bytes()[..width as usize],
                        operation.alignment_intent()
                    )
                ) {
                    core.access_failure(fault);
                    return core.memory_failure_reason();
                }
            }
            _ => {}
        }
        unsafe {
            let state = &mut *core.state.get();
            if let Some(candidate) = staged {
                state.x87 = candidate;
            }
            if operation.writes_flags() {
                state.flags = effect.flags;
            }
            if effect.ax {
                state.registers[0] = (rax & !0xffff) | (effect.value as u64 & 0xffff);
            }
        }
        if OBSERVED && let Some(address) = address {
            if !store {
                core.debug_operand(address, width, Access::Read);
            }
            if effect.store {
                core.debug_operand(address, width, Access::Write);
            }
        }
        if address.is_some() && core.image.active_changed() {
            2
        } else {
            0
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_x87(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    core_x87_impl::<false, true>(operation, offset, segment, instruction_pc, flags, rax)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline(always)]
fn quiet_paged_x87_pair(core: &Core, state: &RunState, pc: u64, flags: u64) -> bool {
    use crate::x86::flags::{RF, TF};
    core.context.is_long64()
        && core.numeric_admission_unrestricted()
        && state.x87.pending() == 0
        && state.event.kind == 0
        && state.interrupts == Default::default()
        && flags & (RF | TF) == 0
        && core.debug_completion_quiet()
        && core.paged_region_trace.is_none()
        && core.warm_guarded_code_page(pc, false)
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn paged_x87_operand(image: &Image, physical: u64, width: u32) -> Option<u64> {
    if width == 8 {
        image.physical_read_mapped_qword(physical)
    } else {
        image.physical_read_mapped_dword(physical).map(u64::from)
    }
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline(never)]
fn core_paged_x87_load_pair(
    operation: u32,
    address: u64,
    segment: u32,
    pc: u64,
    flags: u64,
    next: u64,
) -> i32 {
    use crate::x86::flags::AC;
    use crate::x86::paging::walk::{AccessClass, Request};
    let (Ok(next), Some(segment @ (Segment::Ds | Segment::Ss))) = (
        u32::try_from(next),
        Segment::ALL.get(segment as usize).copied(),
    ) else {
        return -1;
    };
    BRIDGE.with_borrow_mut(|bridge| {
        let (Some(first), Some(second)) = (
            bridge.x87_operations.get(operation),
            bridge.x87_operations.get(next),
        ) else {
            return -1;
        };
        if first.kind != x87::Kind::Load
            || second.is_store()
            || !x87::memory_first_pair(first, second)
        {
            return -1;
        }
        let core = bridge.core.as_mut().unwrap();
        let state = unsafe { &mut *core.state.get() };
        let width = first.memory_bytes();
        if address & u64::from(width - 1) != 0
            || !crate::memory::canonical48(address)
            || !quiet_paged_x87_pair(core, state, pc, flags)
        {
            return -1;
        }
        // Both starts share the admitted fetch page. Only a retained data
        // hit may precede the successor fetch: a walk could change A/D bits.
        let request = Request {
            linear: address,
            access: Access::Read,
            class: AccessClass::operand(core.context.cpl(), flags & AC != 0),
        };
        let paired = core.translation_cache.data_then_fetch::<false, _>(
            core.paging.as_ref().expect("paged x87 pair"),
            request,
            &state.protection_keys,
            width,
            false,
            |physical| {
                let bits = paged_x87_operand(&core.image, physical, width)?;
                state
                    .x87
                    .load_memory_register(first.format, bits, second.index)
                    .then_some(())
            },
        );
        if paired.is_none() {
            return -1;
        }
        x87_environment(
            &core.context,
            &mut state.x87,
            second,
            pc,
            Some((address, segment)),
        );
        clear_x87_diagnostics(core);
        0
    })
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[inline(never)]
fn core_paged_x87_store_pair(
    operation: u32,
    source: u64,
    segments: u32,
    pc: u64,
    destination: u64,
    next: u64,
) -> i32 {
    use crate::x86::flags::AC;
    use crate::x86::paging::walk::{AccessClass, Request};
    let (Some(Segment::Ds | Segment::Ss), Some(&segment @ (Segment::Ds | Segment::Ss))) = (
        Segment::ALL.get((segments & 7) as usize).copied(),
        Segment::ALL.get((segments >> 3) as usize),
    ) else {
        return -1;
    };
    let flags = next >> 32;
    BRIDGE.with_borrow_mut(|bridge| {
        let (Some(first), Some(second)) = (
            bridge.x87_operations.get(operation),
            bridge.x87_operations.get(next as u32),
        ) else {
            return -1;
        };
        if !second.is_store() || !x87::memory_first_pair(first, second) {
            return -1;
        }
        let core = bridge.core.as_mut().unwrap();
        let state = unsafe { &mut *core.state.get() };
        let read_width = first.memory_bytes();
        let write_width = second.memory_bytes();
        if source & u64::from(read_width - 1) != 0
            || destination & u64::from(write_width - 1) != 0
            || !crate::memory::canonical48(source)
            || !crate::memory::canonical48(destination)
            || !quiet_paged_x87_pair(core, state, pc, flags)
        {
            return -1;
        }
        let paging = core.paging.as_ref().expect("paged x87 pair");
        let class = AccessClass::operand(core.context.cpl(), flags & AC != 0);
        let read = Request {
            linear: source,
            class,
            access: Access::Read,
        };
        let write = Request {
            linear: destination,
            class,
            access: Access::Write,
        };
        let Some(bits) = core.translation_cache.data_then_fetch::<false, _>(
            paging,
            read,
            &state.protection_keys,
            read_width,
            false,
            |physical| paged_x87_operand(&core.image, physical, read_width),
        ) else {
            return -1;
        };

        // Admission is deliberately retained-only and cannot update page
        // tables. Stage the numeric bank until the destination also qualifies.
        let mut x87 = state.x87;
        let Some(value) = x87.memory_arithmetic_store(first, bits, second) else {
            return -1;
        };
        if !core.translation_cache.write_retained_span(
            paging,
            write,
            &state.protection_keys,
            write_width,
            |physical| match value {
                Some(value) if write_width == 8 => core
                    .image
                    .physical_direct_write(physical, value.to_le_bytes()),
                Some(value) => core
                    .image
                    .physical_direct_write(physical, (value as u32).to_le_bytes()),
                None => core
                    .image
                    .physical_direct_write_ready(physical, write_width)
                    .then_some(()),
            },
        ) {
            return -1;
        }
        x87_environment(
            &core.context,
            &mut x87,
            second,
            pc,
            Some((destination, segment)),
        );
        state.x87 = x87;
        clear_x87_diagnostics(core);
        unsafe { (*core.state.get()).fault_segment = segment as u32 };
        0
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_x87_observed(
    operation: u32,
    offset: u64,
    segment: u32,
    instruction_pc: u64,
    flags: u64,
    rax: u64,
) -> i32 {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if segment & x87::PAGED_LOAD_PAIR_SEGMENT != 0 {
        return core_paged_x87_load_pair(
            operation,
            offset,
            segment ^ x87::PAGED_LOAD_PAIR_SEGMENT,
            instruction_pc,
            flags,
            rax,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if segment & x87::PAGED_STORE_PAIR_SEGMENT != 0 {
        return core_paged_x87_store_pair(
            operation,
            offset,
            segment ^ x87::PAGED_STORE_PAIR_SEGMENT,
            instruction_pc,
            flags,
            rax,
        );
    }
    core_x87_impl::<true, true>(operation, offset, segment, instruction_pc, flags, rax)
}

impl Core {
    pub(in crate::embedding::exports) fn pending_x87_reason(&self) -> i32 {
        if self.paging.is_some_and(|bank| bank.cr0 & 0x20 == 0) {
            UNSUPPORTED
        } else {
            X87_EXCEPTION
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_operations_distinguish_colliding_opcodes_and_malformed_words() {
        let first = Op {
            kind: x87::Kind::Load,
            format: x87::Format::F32,
            ..Op::default()
        }
        .encode();
        let index = DecodedOperations::index(first);
        let collision = |bits| {
            (1..0x800)
                .map(|encoding| first | bits | (encoding << 19))
                .find(|&word| DecodedOperations::index(word) == index)
                .unwrap()
        };
        let second = collision(0);
        let malformed = collision(1 << 30);
        let mut cache = DecodedOperations::default();
        for word in [first, first, malformed, second, second, first] {
            assert_eq!(cache.get(word), Op::try_decode(word));
        }
    }
}
