use super::{BRIDGE, Core, paging};
use crate::dbt::EntrySelection;
use crate::embedding::layout::{RESOURCE, debug::CONTINUE_ACTIVE};
use crate::x86::{exception::EventState, flags, paging::State};

const PAGE_MASK: u64 = !4095;
const MAX_OBSERVATIONS: u16 = 2048;

pub(super) struct Trace {
    page: u64,
    paging: State,
    context_version: u64,
    samples: u16,
    counts: [u16; 4096],
}

fn trace_profile(core: &Core) -> Option<State> {
    core.paging.filter(|paging| {
        paging.cr0 & (1 << 31) != 0
            && core.context.is_long64()
            && !core.io_present
            && !core.image.has_mmio()
    })
}

fn traceable(core: &Core) -> Option<State> {
    let paging = trace_profile(core)?;
    let state = unsafe { &*core.state.get() };
    (!core.image.active_changed()
        && state.delivery_enabled == 0
        && state.interrupts.irq == 0
        && state.interrupts.nmi_pending == 0
        && state.interruptibility == 0
        && state.flags & (flags::TF | flags::RF) == 0
        && !state.debug.registers.active()
        && state.debug.continuation.slots == 0
        && matches!(state.event_state(), Ok(EventState::Ready)))
    .then_some(paging)
}

#[unsafe(no_mangle)]
pub extern "C" fn core_supports_paged_region_trace() -> i32 {
    BRIDGE.with_borrow(|bridge| i32::from(bridge.core.as_ref().and_then(trace_profile).is_some()))
}

#[unsafe(no_mangle)]
pub extern "C" fn core_begin_paged_region_trace(pc: u64) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        let Some(core) = bridge.core.as_mut() else {
            return 0;
        };
        if core.paged_region_trace.is_some() || !crate::memory::canonical48(pc) {
            return 0;
        }
        let Some(paging) = traceable(core) else {
            return 0;
        };
        core.paged_region_trace = Some(Box::new(Trace {
            page: pc & PAGE_MASK,
            paging,
            context_version: core.context_version,
            samples: 0,
            counts: [0; 4096],
        }));
        1
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_finish_paged_region_trace(max_points: u32) -> i32 {
    BRIDGE.with_borrow_mut(|bridge| {
        bridge.output.clear();
        let Some(core) = bridge.core.as_mut() else {
            return 0;
        };
        let Some(trace) = core.paged_region_trace.take() else {
            return 0;
        };
        if max_points == 0
            || Some(trace.paging) != traceable(core)
            || trace.context_version != core.context_version
        {
            return 0;
        }
        let mut points: Vec<_> = trace
            .counts
            .iter()
            .copied()
            .enumerate()
            .filter(|&(_, count)| count != 0)
            .collect();
        points.sort_unstable_by_key(|&(offset, count)| (std::cmp::Reverse(count), offset));
        for (offset, _) in points.into_iter().take(max_points as usize) {
            bridge
                .output
                .extend((trace.page | offset as u64).to_le_bytes());
        }
        i32::from(trace.samples)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_region_debug_finish(
    pc: u64,
    resume: u64,
    completed: u32,
    flags: u64,
    decision: u32,
) -> i32 {
    let reason = paging::core_paged_debug_finish(pc, resume, completed, flags, decision);
    BRIDGE.with_borrow_mut(|bridge| {
        let core = bridge.core.as_mut().unwrap();
        let Some(trace) = core.paged_region_trace.as_mut() else {
            return;
        };
        if reason != CONTINUE_ACTIVE || trace.page != pc & PAGE_MASK {
            core.paged_region_trace = None;
        } else if completed != 0 && trace.samples < MAX_OBSERVATIONS {
            trace.samples += 1;
            trace.counts[(pc & !PAGE_MASK) as usize] += 1;
        }
    });
    reason
}

#[unsafe(no_mangle)]
pub extern "C" fn core_paged_region_checked_guard(
    _pc: u64,
    _bytes: u32,
    _flags: u64,
    _shadow: u32,
) -> i32 {
    // The guarded copy already handles this checked retry without retiring.
    2
}

#[unsafe(no_mangle)]
pub extern "C" fn core_compile_with_paged_region_points(
    pc: u64,
    max_instructions: u32,
    max_source_bytes: u32,
    max_artifact_bytes: u32,
) -> i32 {
    let Some(points) = BRIDGE.with_borrow(|bridge| {
        traceable(bridge.core.as_ref()?)?;
        let count = bridge.input.len() / 8;
        if !bridge.input.len().is_multiple_of(8)
            || count == 0
            || count > max_instructions as usize
            || count > usize::from(MAX_OBSERVATIONS)
        {
            return None;
        }
        let points: Vec<_> = bridge
            .input
            .chunks_exact(8)
            .map(|bytes| u64::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        if !points.contains(&pc) && count == max_instructions as usize
            || points
                .iter()
                .any(|&point| point & PAGE_MASK != pc & PAGE_MASK)
        {
            return None;
        }
        Some(points)
    }) else {
        return RESOURCE;
    };
    super::translation::compile_artifact(
        pc,
        max_instructions,
        max_source_bytes,
        max_artifact_bytes,
        true,
        true,
        EntrySelection::Sampled(&points),
    )
}
