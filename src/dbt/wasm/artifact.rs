#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
use super::parallel;
use super::{
    ENTRY, ENTRY_INDEX, Emitter, REASON, StructuredLoop, Unit, boundary, control, entry_lookup,
    mmx, state, vector,
};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::{
    float_dyadic, float_loop, loop_cfg, memory,
    paged_copy::{CopyGroup, ScalarCopyGroup},
    read_loop, stack_slots,
};
use crate::embedding::layout::DISPATCH;
use crate::x86::context::Segment;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::x86::instruction::Instruction;
use crate::x86::instruction::{Location, MemoryAluOperands, Operation, Repeat};
use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction as Op, MemoryType, Module, Section, TypeSection, ValType,
};

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(super) fn register_prefix_operation(operation: Operation) -> bool {
    super::structured_loop::inside_register_body(operation)
        || matches!(
            operation,
            Operation::Extend {
                src: Location::Register(_),
                ..
            }
        )
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn flat_integer_groups(unit: &Unit, structured: Option<StructuredLoop>) -> Vec<Option<usize>> {
    use crate::x86::instruction::{Operand, Shift, Width};

    let instructions = &unit.instructions;
    let mut groups = vec![None; instructions.len()];
    let mut first = 0;
    while first < instructions.len() {
        let mut end = first;
        while let Some(instruction) = instructions.get(end)
            && end - first < 20
            && (end == first || instructions[end - 1].next_pc == instruction.pc)
            && structured.is_none_or(|body| body.order().position(end as u32).is_none())
        {
            let operation = instruction.operation;
            if let Operation::Jcc { target, .. } = operation {
                // A failing target must see the ordinary Jcc retirement boundary.
                if end > first && target <= unit.context.code_limit() {
                    end += 1;
                }
                break;
            }
            if matches!(
                operation,
                Operation::Call {
                    target: Operand::Register(_) | Operand::Memory(_),
                    width: Width::W32,
                } | Operation::Return {
                    width: Width::W32,
                    ..
                }
            ) {
                // All decoded near targets pass CS. The stack and indirect target
                // operands still use the checked memory exits.
                if end > first && unit.context.code_limit() == u64::from(u32::MAX) {
                    end += 1;
                }
                break;
            }
            let memory = matches!(
                operation,
                Operation::MemoryAlu { .. }
                    | Operation::Load { .. }
                    | Operation::Store { .. }
                    | Operation::Extend {
                        src: Location::Memory(_),
                        ..
                    }
                    | Operation::Imul {
                        src: Location::Memory(_),
                        width: Width::W32,
                        ..
                    }
                    | Operation::Pop {
                        width: Width::W32,
                        ..
                    }
                    | Operation::Push {
                        width: Width::W32,
                        ..
                    }
                    | Operation::Shift {
                        kind: Shift::Rol | Shift::Ror,
                        dst: Location::Memory(_),
                        width: Width::W32,
                        ..
                    }
            );
            if !register_prefix_operation(operation)
                && !memory
                && !matches!(
                    operation,
                    Operation::Cmov {
                        src: Location::Register(_),
                        ..
                    }
                )
            {
                break;
            }
            end += 1;
        }
        if end - first >= 3 {
            groups[first] = Some(end);
        }
        first = end.max(first + 1);
    }
    groups
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn flat_call_order(instructions: &[Instruction]) -> Option<Vec<usize>> {
    use crate::x86::instruction::Operand;

    let mut backedges =
        instructions
            .iter()
            .enumerate()
            .filter_map(|(index, i)| match i.operation {
                Operation::Jcc { target, .. } if target < i.pc => Some((index, target)),
                _ => None,
            });
    let (latch, target) = backedges.next()?;
    if backedges.next().is_some() {
        return None;
    }
    let head = instructions.binary_search_by_key(&target, |i| i.pc).ok()?;
    let loop_body = &instructions[head..=latch];
    if loop_body
        .windows(2)
        .any(|pair| pair[0].next_pc != pair[1].pc)
    {
        return None;
    }
    let mut transfers = instructions[head..latch]
        .iter()
        .enumerate()
        .filter(|(_, i)| {
            matches!(
                i.operation,
                Operation::Call { .. }
                    | Operation::Return { .. }
                    | Operation::Jump { .. }
                    | Operation::Jcc { .. }
                    | Operation::CountBranch { .. }
                    | Operation::JumpRegister { .. }
                    | Operation::JumpMemory { .. }
            )
        });
    let (call, instruction) = transfers.next()?;
    if transfers.next().is_some()
        || !matches!(
            instruction.operation,
            Operation::Call {
                target: Operand::Register(_) | Operand::Memory(_),
                ..
            }
        )
    {
        return None;
    }
    // The call's return continuation runs first in layout, so the latch can
    // branch directly to the next call without reentering the dense table.
    let mut order = (0..instructions.len()).collect::<Vec<_>>();
    order[head..=latch].rotate_left(call + 1);
    Some(order)
}

// A clone can touch the executable leaf once when every admitted instruction
// stays on that leaf and can neither access memory nor change the controls.
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
fn paged_register_prefixes(
    unit: &Unit,
    loops: [Option<StructuredLoop>; 2],
    short_indexed_reads: bool,
) -> Vec<Option<usize>> {
    let instructions = &unit.instructions;
    let long64 = unit.context.is_long64();
    let short_register_prefixes = long64 && unit.sampled;
    let maximum = if long64 { 12 } else { 8 };
    let mut targets = vec![false; instructions.len()];
    if long64 {
        for instruction in instructions {
            let target = match instruction.operation {
                Operation::Jump { target }
                | Operation::Jcc { target, .. }
                | Operation::CountBranch { target, .. } => target,
                _ => continue,
            };
            if let Ok(index) = instructions.binary_search_by_key(&target, |i| i.pc) {
                targets[index] = true;
            }
        }
    }
    let local_backedge = |instruction: &Instruction| {
        matches!(instruction.operation, Operation::Jump { target }
            if target < instruction.pc
                && target >> 12 == instruction.pc >> 12
                && instructions.binary_search_by_key(&target, |i| i.pc).is_ok())
    };
    let terminal_branch = |instruction: &Instruction, has_read: bool, wide_read: bool| {
        long64
            && match instruction.operation {
                Operation::Jcc { target, .. } => {
                    crate::memory::canonical48(target)
                        && ((short_register_prefixes
                            && (!has_read || target >> 12 == instruction.pc >> 12))
                            || wide_read
                            || instructions.binary_search_by_key(&target, |i| i.pc).is_ok())
                }
                Operation::Jump { target } => {
                    short_register_prefixes
                        && !has_read
                        && target >> 12 == instruction.pc >> 12
                        && instructions.binary_search_by_key(&target, |i| i.pc).is_ok()
                }
                _ => false,
            }
    };
    let mut prefixes = vec![None; instructions.len()];
    let mut first = 0;
    while first < instructions.len() {
        let pc = instructions[first].pc;
        let mut end = first;
        let mut has_read = false;
        let mut has_wide_read = false;
        let mut has_branch = false;
        while let Some(instruction) = instructions.get(end)
            && end - first < maximum
            && (end == first || !targets[end])
            && !loops
                .into_iter()
                .flatten()
                .any(|body| body.order().position(end as u32).is_some())
            && (register_prefix_operation(instruction.operation)
                || (if has_read {
                    has_wide_read || end - first >= 2
                } else {
                    short_register_prefixes && (end > first || local_backedge(instruction))
                } && terminal_branch(instruction, has_read, has_wide_read))
                || (short_register_prefixes
                    && !has_read
                    && super::instruction::paged_prefix_wide_read(*instruction))
                || (long64
                    && !has_wide_read
                    && if end == first {
                        super::instruction::paged_prefix_read(*instruction).is_some()
                    } else {
                        super::instruction::paged_prefix_dword_read(*instruction)
                            || has_read
                                && super::instruction::paged_prefix_dword_store(*instruction)
                    }))
            && instruction.next_pc > instruction.pc
            && instruction.next_pc.checked_sub(pc).is_some_and(|bytes| {
                bytes <= 4096 && unit.context.single_page_code_span(pc, bytes as u32)
            })
            && (end == first || instructions[end - 1].next_pc == instruction.pc)
        {
            // Keep an already useful register-only prefix before a checked store.
            has_wide_read |= super::instruction::paged_prefix_wide_read(*instruction);
            has_read |= has_wide_read
                || if end == first {
                    super::instruction::paged_prefix_read(*instruction).is_some()
                } else {
                    super::instruction::paged_prefix_dword_read(*instruction)
                };
            end += 1;
            if terminal_branch(instruction, has_read, has_wide_read) {
                has_branch = true;
                break;
            }
        }
        // Each successful operand accounts for the next fetch. Leave a following
        // in-prefix instruction so a stop or exhausted budget cannot omit it.
        while end > first + 1
            && (super::instruction::paged_prefix_dword_read(instructions[end - 1])
                || super::instruction::paged_prefix_dword_store(instructions[end - 1])
                || super::instruction::paged_prefix_wide_read(instructions[end - 1]))
        {
            end -= 1;
        }
        let minimum = if has_branch && local_backedge(&instructions[first]) {
            1
        } else if short_register_prefixes && (!has_read || has_wide_read && has_branch) {
            2
        } else if has_branch
            || short_indexed_reads
                && instructions[first..end]
                    .iter()
                    .copied()
                    .any(super::instruction::paged_prefix_dword_read)
        {
            3
        } else {
            4
        };
        if end - first >= minimum {
            prefixes[first] = Some(end);
        }
        first = end.max(first + 1);
    }
    prefixes
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[derive(Clone, Copy, Default)]
struct PrefixFetchEdge {
    receives: bool,
    destination: Option<usize>,
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
// The admitted read-only clone leaves its executable translation newest and
// cannot change code or controls. A same-page successor needs no new touch.
fn paged_prefix_handoffs(
    unit: &Unit,
    prefixes: &[Option<usize>],
    earlier_clones: [Option<usize>; 3],
) -> Option<Vec<PrefixFetchEdge>> {
    if !unit.context.is_long64() {
        return None;
    }
    let instructions = &unit.instructions;
    let mut edges = None;
    for (start, &end) in prefixes.iter().enumerate() {
        let Some(end) = end else { continue };
        let Operation::Jcc { target, .. } = instructions[end - 1].operation else {
            continue;
        };
        if target & !4095 != instructions[start].pc & !4095
            || instructions[start..end]
                .iter()
                .copied()
                .any(super::instruction::paged_prefix_dword_store)
        {
            continue;
        }
        if let Ok(destination) = instructions.binary_search_by_key(&target, |i| i.pc)
            && prefixes[destination].is_some()
            && !earlier_clones.contains(&Some(destination))
        {
            let edges =
                edges.get_or_insert_with(|| vec![PrefixFetchEdge::default(); prefixes.len()]);
            edges[destination].receives = true;
            edges[start].destination = Some(destination);
        }
    }
    edges
}

fn add_function_type<const PARAMS: usize, const RESULTS: usize>(
    types: &mut TypeSection,
    params: [ValType; PARAMS],
    results: [ValType; RESULTS],
) -> u32 {
    let index = types.len();
    types.ty().function(params, results);
    index
}

pub(crate) fn emit_profile(
    unit: &Unit,
    observed: bool,
    paged: bool,
    max_artifact_bytes: usize,
) -> Vec<u8> {
    let observed = observed || paged;
    #[cfg(not(feature = "parallel"))]
    if paged && let Some(bytes) = emit_paged_profile(unit, max_artifact_bytes, false) {
        return bytes;
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if !observed
        && let Some(body) = StructuredLoop::select_shared_float(&unit.instructions)
            .or_else(|| StructuredLoop::select_shared_rotated_reads(unit))
            .or_else(|| StructuredLoop::select_shared_natural_memory(&unit.instructions))
    {
        // Every shared placement retains its exact per-instruction signal check.
        let bytes = emit_placement(
            unit,
            observed,
            (paged, false),
            Some(body),
            false,
            max_artifact_bytes,
        );
        if bytes.len() <= max_artifact_bytes {
            return bytes;
        }
    }
    if !observed && let Some(loop_body) = StructuredLoop::select(unit) {
        let bytes = emit_placement(
            unit,
            observed,
            (paged, false),
            Some(loop_body),
            true,
            max_artifact_bytes,
        );
        if bytes.len() <= max_artifact_bytes {
            return bytes;
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        if loop_body.dword_writes(unit) != 0 {
            let bytes = emit_placement(
                unit,
                observed,
                (paged, false),
                Some(loop_body.without_dword_projection()),
                true,
                max_artifact_bytes,
            );
            if bytes.len() <= max_artifact_bytes {
                return bytes;
            }
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if loop_body.read_proof(unit).is_some()
            || loop_body.stack_proof(unit).is_some()
            || loop_body.integer_read_proof(unit).is_some()
        {
            // Keep the checked placement when only the extra guarded body
            // would exhaust the user's generated artifact limit.
            let bytes = emit_placement(
                unit,
                observed,
                (paged, false),
                Some(loop_body),
                false,
                max_artifact_bytes,
            );
            if bytes.len() <= max_artifact_bytes {
                return bytes;
            }
        }
    }
    emit_placement(
        unit,
        observed,
        (paged, false),
        None,
        false,
        max_artifact_bytes,
    )
}

#[cfg(not(feature = "parallel"))]
// Keep paged selection out of the Wasm body compiled on the first linear translation.
#[cfg_attr(target_arch = "wasm32", inline(never))]
fn emit_paged_profile(
    unit: &Unit,
    max_artifact_bytes: usize,
    copy_prefix: bool,
) -> Option<Vec<u8>> {
    let placement = |body| {
        let bytes = emit_placement_with_copy(
            unit,
            true,
            (true, false),
            Some(body),
            false,
            max_artifact_bytes,
            (None, copy_prefix, false),
        );
        (bytes.len() <= max_artifact_bytes).then_some(bytes)
    };
    if let Some(body) = StructuredLoop::select_paged_registers(&unit.instructions) {
        if let Some(guarded) = body.within_paged_fetch(unit)
            && let Some(bytes) = placement(guarded)
        {
            return Some(bytes);
        }
        if let Some(bytes) = placement(body) {
            return Some(bytes);
        }
    }
    if let Some(body) = StructuredLoop::select_paged_reads(unit) {
        #[cfg(target_arch = "wasm32")]
        if let Some(window) = body.with_paged_dword_window(unit)
            && let Some(bytes) = placement(window)
        {
            return Some(bytes);
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(span) = body.with_paged_read_span(unit)
            && let Some(bytes) = placement(span)
        {
            return Some(bytes);
        }
        if let Some(bytes) = placement(body) {
            return Some(bytes);
        }
    }
    if let Some(body) = StructuredLoop::select_paged_scalar_f32(unit) {
        #[cfg(target_arch = "wasm32")]
        if let Some(span) = body.with_paged_f32_read_span(unit)
            && let Some(bytes) = placement(span)
        {
            return Some(bytes);
        }
        if let Some(bytes) = placement(body) {
            return Some(bytes);
        }
    }
    #[cfg(target_arch = "wasm32")]
    if let Some(body) = StructuredLoop::select_sse_write(unit, true)
        && let Some(bytes) = placement(body)
    {
        return Some(bytes);
    }
    #[cfg(target_arch = "wasm32")]
    if max_artifact_bytes >= 256 * 1024
        && let Some(body) = StructuredLoop::select_paged_scalar_writes(unit)
    {
        if let Some(window) = body.with_paged_dword_window(unit)
            && let Some(bytes) = placement(window)
        {
            return Some(bytes);
        }
        if let Some(bytes) = placement(body) {
            return Some(bytes);
        }
    }
    None
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) fn emit_paged_vector_copy(
    unit: &Unit,
    max_artifact_bytes: usize,
    copy_spans: bool,
) -> Option<Vec<u8>> {
    let body = StructuredLoop::select_paged_vector_copy(unit);
    let windowed = copy_spans
        .then(|| body.and_then(|body| body.with_paged_copy_spans(unit)))
        .flatten();
    let additional = copy_spans
        .then(|| body.and_then(|body| StructuredLoop::select_additional_paged_copy(unit, body)))
        .flatten();
    let copy_prefix = copy_spans
        && (CopyGroup::finite_prefix(unit, [body, additional]).is_some()
            || CopyGroup::preloaded_prefix(unit, [body, additional]).is_some()
            || unit.context.is_long64()
                && ScalarCopyGroup::finite_prefix(unit, [body, additional]).is_some());
    if copy_prefix
        && body.is_none()
        && let Some(bytes) = emit_paged_profile(unit, max_artifact_bytes, true)
    {
        return Some(bytes);
    }
    if windowed.is_some() || additional.is_some() || copy_prefix {
        let bytes = emit_placement_with_copy(
            unit,
            true,
            (true, false),
            windowed.or(body),
            false,
            max_artifact_bytes,
            (additional, copy_prefix, false),
        );
        if bytes.len() <= max_artifact_bytes {
            return Some(bytes);
        }
    }
    let body = body?;
    let bytes = emit_placement(
        unit,
        true,
        (true, false),
        Some(body),
        false,
        max_artifact_bytes,
    );
    (bytes.len() <= max_artifact_bytes).then_some(bytes)
}

#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) fn emit_mmio_profile(
    unit: &Unit,
    observed: bool,
    paged: bool,
    max_artifact_bytes: usize,
) -> Vec<u8> {
    if !unit.instructions.iter().any(|instruction| {
        matches!(
            instruction.operation,
            Operation::Load { .. } | Operation::Store { .. }
        )
    }) {
        return emit_profile(unit, observed, paged, max_artifact_bytes);
    }
    emit_placement(
        unit,
        observed || paged,
        (paged, true),
        None,
        false,
        max_artifact_bytes,
    )
}

#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) fn emit_flat_trace(unit: &Unit, max_artifact_bytes: usize) -> Vec<u8> {
    emit_placement_with_copy(
        unit,
        false,
        (false, false),
        None,
        false,
        max_artifact_bytes,
        (None, false, true),
    )
}

fn emit_placement(
    unit: &Unit,
    observed: bool,
    (paged, mmio): (bool, bool),
    loop_body: Option<StructuredLoop>,
    guard_reads: bool,
    max_artifact_bytes: usize,
) -> Vec<u8> {
    emit_placement_with_copy(
        unit,
        observed,
        (paged, mmio),
        loop_body,
        guard_reads,
        max_artifact_bytes,
        (None, false, false),
    )
}

fn emit_placement_with_copy(
    unit: &Unit,
    observed: bool,
    (paged, mmio): (bool, bool),
    loop_body: Option<StructuredLoop>,
    guard_reads: bool,
    max_artifact_bytes: usize,
    (_additional_copy, _copy_prefix, flat_trace): (Option<StructuredLoop>, bool, bool),
) -> Vec<u8> {
    let reads = if cfg!(all(feature = "parallel", target_arch = "wasm32")) {
        let indexed = !observed
        && !paged
        && (unit.context.is_long64()
            || unit.context.mode() == crate::x86::context::Mode::Protected
            && unit.context.code32()
            && unit.context.translate(0, 4, Segment::Ds, crate::memory::Access::Read, false).ok() == Some(0))
        && unit.instructions.iter().any(|i| {
            matches!(i.operation, Operation::Jcc { target, .. } | Operation::Jump { target } if target < i.pc)
        });
        GuardedReads::Shared {
            indexed: if indexed {
                SharedIndexedReads::IncludeBaseBytes
            } else {
                SharedIndexedReads::Disabled
            },
            compact: false,
            #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
            byte_stores: indexed
                && (unit.context.is_long64()
                    || unit
                        .context
                        .translate(0, 1, Segment::Ds, crate::memory::Access::Write, false)
                        .ok()
                        == Some(0)),
        }
    } else if !guard_reads {
        GuardedReads::Checked
    } else if cfg!(all(not(feature = "parallel"), target_arch = "wasm32")) {
        GuardedReads::Outlined {
            sticky: true,
            dyadic: true,
            diamond: true,
            indexed_read: true,
        }
    } else {
        GuardedReads::Inline
    };
    let optional = OptionalLowering {
        flat_trace,
        extra_spans: true,
        inline_f32_compares: true,
        inline_f32_sub: true,
        inline_f32_packed: true,
        inline_cmpnleps: true,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_native_lock_memo: max_artifact_bytes >= 256 * 1024,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        x87_register_pairs: !flat_trace,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_register_prefixes: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        flat_integer_groups: !flat_trace,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_quiet_completion: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_quiet_fetch: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        indexed_scalar_read: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        indexed_dword_prefixes: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        additional_copy: _additional_copy,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        copy_prefix: _copy_prefix,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        scalar_copy_prefix: !flat_trace && (_copy_prefix || (!paged && !observed && !mmio)),
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        linear_recurrence: max_artifact_bytes >= 64 * 1024,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        direct_vector_loads: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        direct_vector_stores: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_float_dyadic: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_float_sticky: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_f32_read_pair: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_f32_split_pair: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_dense_read_span: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        direct_loop_reads: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_read_rearm: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_byte_write_pair: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_dword_read_pair: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_indexed_write: true,
        // Smaller caller budgets immediately use the original paired lowering.
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_byte_write_pointer: max_artifact_bytes >= 256 * 1024,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_split_vectors: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_vector_window: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_vector_retouch: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_vector_rearm: max_artifact_bytes >= 256 * 1024,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_read_diamond: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_preamble: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_preamble_latch: true,
    };
    let (bytes, extra_spans) = emit_body(
        unit,
        observed,
        (paged, mmio),
        loop_body,
        reads,
        optional,
        max_artifact_bytes,
    );
    if extra_spans && bytes.len() > max_artifact_bytes {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if guard_reads
            && loop_body.is_some_and(|body| {
                body.stack_proof(unit).is_some() || body.integer_read_proof(unit).is_some()
            })
        {
            // Drop the extra loop before reconsidering the original span caches.
            return bytes;
        }
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            reads,
            OptionalLowering {
                extra_spans: false,
                ..optional
            },
            max_artifact_bytes,
        )
        .0;
    }
    bytes
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SharedIndexedReads {
    Disabled,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    DwordsOnly,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    IncludeBytes,
    IncludeBaseBytes,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GuardedReads {
    Checked,
    Inline,
    Shared {
        indexed: SharedIndexedReads,
        compact: bool,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        byte_stores: bool,
    },
    Outlined {
        sticky: bool,
        dyadic: bool,
        diamond: bool,
        indexed_read: bool,
    },
}

#[derive(Clone, Copy)]
struct OptionalLowering {
    flat_trace: bool,
    extra_spans: bool,
    inline_f32_compares: bool,
    inline_f32_sub: bool,
    inline_f32_packed: bool,
    inline_cmpnleps: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_native_lock_memo: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    x87_register_pairs: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_register_prefixes: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    flat_integer_groups: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_quiet_completion: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_quiet_fetch: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_scalar_read: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_dword_prefixes: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    additional_copy: Option<StructuredLoop>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    copy_prefix: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    scalar_copy_prefix: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    linear_recurrence: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    direct_vector_loads: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    direct_vector_stores: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_float_dyadic: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_float_sticky: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_f32_read_pair: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_f32_split_pair: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_dense_read_span: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    direct_loop_reads: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_read_rearm: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_byte_write_pair: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_dword_read_pair: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_indexed_write: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_byte_write_pointer: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_split_vectors: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_vector_window: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_vector_retouch: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_vector_rearm: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_read_diamond: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_preamble: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_preamble_latch: bool,
}

fn emit_body(
    unit: &Unit,
    observed: bool,
    (paged, mmio): (bool, bool),
    loop_body: Option<StructuredLoop>,
    _guard_reads: GuardedReads,
    optional: OptionalLowering,
    max_artifact_bytes: usize,
) -> (Vec<u8>, bool) {
    let _extra_spans = optional.extra_spans;
    let vector_banks = vector::Banks::for_unit(unit);
    let has_repeat = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::String { repeat, .. } if repeat != Repeat::None));
    let dynamic_targets = unit.instructions.iter().any(|i| {
        matches!(
            i.operation,
            Operation::Return { .. }
                | Operation::JumpRegister { .. }
                | Operation::JumpMemory { .. }
                | Operation::Call {
                    target: crate::x86::instruction::Operand::Register(_)
                        | crate::x86::instruction::Operand::Memory(_),
                    ..
                }
        )
    });
    let segmented = !unit.context.is_long64();
    let target_type = dynamic_targets || segmented;
    let mut types = TypeSection::new();
    types.ty().function(
        if segmented {
            vec![ValType::I64, ValType::I32, ValType::I32]
        } else {
            vec![ValType::I64, ValType::I32]
        },
        [ValType::I64],
    );
    types.ty().function(
        if segmented {
            vec![ValType::I64, ValType::I32, ValType::I64, ValType::I32]
        } else {
            vec![ValType::I64, ValType::I32, ValType::I64]
        },
        [ValType::I32],
    );
    types.ty().function([], [ValType::I32]);
    types.ty().function(
        [
            ValType::I32,
            ValType::I64,
            ValType::I64,
            ValType::I32,
            ValType::I32,
        ],
        [ValType::I32],
    );
    if target_type {
        types.ty().function([ValType::I64], [ValType::I32]);
    }
    let wide_product = unit.instructions.iter().any(|i| {
        matches!(
            i.operation,
            Operation::Multiply {
                width: crate::x86::instruction::Width::W64,
                ..
            } | Operation::Imul {
                width: crate::x86::instruction::Width::W64,
                ..
            }
        )
    });
    let wide_divide = unit.instructions.iter().any(|i| {
        matches!(
            i.operation,
            Operation::Divide {
                width: crate::x86::instruction::Width::W64,
                ..
            }
        )
    });
    let multiply_type = wide_product.then(|| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I64, ValType::I32],
            [ValType::I64],
        )
    });
    let divide_type = wide_divide.then(|| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I64, ValType::I64, ValType::I32],
            [ValType::I64],
        )
    });
    let vector_load_type = vector_banks.loads.then(|| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I32, ValType::I32, ValType::I32],
            [ValType::I32],
        )
    });
    let wide_compare = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::CompareExchange16b { .. }));
    let block16_for_write_type = wide_compare
        .then(|| add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I32]));
    let mmio_type = mmio.then(|| {
        add_function_type(
            &mut types,
            [ValType::I32, ValType::I64, ValType::I64, ValType::I32],
            [ValType::I32],
        )
    });
    let vector_store_type = (vector_banks.stores || wide_compare).then(|| {
        add_function_type(
            &mut types,
            [
                ValType::I64,
                ValType::I32,
                ValType::I32,
                ValType::I32,
                ValType::I32,
            ],
            [ValType::I32],
        )
    });
    let has_float = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Float(_)));
    let load_mxcsr_used = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Mxcsr { store: false, .. }));
    let store_mxcsr_used = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Mxcsr { store: true, .. }));
    let has_x87 = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::X87(_)));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let x87_register_pairs = optional.x87_register_pairs
        && has_x87
        && !mmio
        && if paged {
            unit.context.is_long64()
                && unit
                    .instructions
                    .windows(2)
                    .any(|pair| super::x87::paged_x87_memory_pair(pair[0], pair[1]))
        } else {
            !observed
                && (unit.instructions.iter().any(|instruction| {
                    matches!(instruction.operation, Operation::X87(x87) if super::x87::inline_x87_bank_operation(x87.op))
                }) || unit.instructions.windows(2)
                    .any(|pair| super::x87::x87_pair(pair[0], pair[1]).is_some()))
        };
    let has_mmx = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Mmx { op, .. } if !mmx::direct(op)));
    let has_mmx_vector = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Mmx { op, .. } if mmx::vector_temps(op)));
    let has_numeric_image = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::NumericImage { .. }));
    let has_masked_store = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::MaskedStore { .. }));
    let has_clflush = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Clflush { .. }));
    let has_protection = unit.instructions.iter().any(|i| {
        matches!(
            i.operation,
            Operation::Protection(_) | Operation::PopSs { .. }
        )
    });
    let has_far = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Far(_)));
    let has_compound_stack = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::CompoundStack(_)));
    let has_debug_register = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::DebugRegister { .. }));
    let has_paging = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Paging(operation) if !matches!(operation, crate::x86::instruction::PagingInstruction::InvalidatePcid { .. })));
    let has_invpcid = unit.instructions.iter().any(|i| {
        matches!(
            i.operation,
            Operation::Paging(crate::x86::instruction::PagingInstruction::InvalidatePcid { .. })
        )
    });
    let has_fast_system = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::FastSystem(_)));
    let has_processor = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Processor(_)));
    let has_protection_key = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::ProtectionKey(_)));
    let has_system_flags = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::SystemFlags(_)));
    let has_software_events = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::SoftwareEvent(_)));
    let has_halt = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::Halt));
    let has_boundary = has_fast_system
        || observed
        || has_system_flags
        || has_software_events
        || has_halt
        || unit
            .instructions
            .iter()
            .any(|i| boundary::ss_producer(i.operation));
    let float_type = has_float.then(|| {
        add_function_type(
            &mut types,
            [
                ValType::I32,
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I64,
            ],
            [ValType::I32],
        )
    });
    let mxcsr_type = (load_mxcsr_used || store_mxcsr_used)
        .then(|| add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I32]));
    let x87_type = has_x87.then(|| {
        add_function_type(
            &mut types,
            [
                ValType::I32,
                ValType::I64,
                ValType::I32,
                ValType::I64,
                ValType::I64,
                ValType::I64,
            ],
            [ValType::I32],
        )
    });
    let masked_store_type = has_masked_store.then(|| {
        add_function_type(
            &mut types,
            [
                ValType::I64,
                ValType::I32,
                ValType::I32,
                ValType::I32,
                ValType::I32,
            ],
            [ValType::I32],
        )
    });
    let clflush_type = has_clflush
        .then(|| add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I32]));
    let protection_type = has_protection.then(|| {
        add_function_type(
            &mut types,
            [ValType::I32, ValType::I64, ValType::I64, ValType::I32],
            [ValType::I32],
        )
    });
    let compound_stack_type =
        has_compound_stack.then(|| add_function_type(&mut types, [ValType::I32], [ValType::I32]));
    let far_type = has_far.then(|| {
        add_function_type(
            &mut types,
            [
                ValType::I32,
                ValType::I64,
                ValType::I32,
                ValType::I64,
                ValType::I64,
                ValType::I32,
            ],
            [ValType::I32],
        )
    });
    let software_event_type = has_software_events.then(|| {
        add_function_type(
            &mut types,
            [ValType::I32, ValType::I64, ValType::I64, ValType::I64],
            [ValType::I32],
        )
    });
    let system_flags_type = has_system_flags.then(|| {
        add_function_type(
            &mut types,
            [ValType::I32, ValType::I64, ValType::I64],
            [ValType::I32],
        )
    });
    let debug_register_type = has_debug_register.then(|| {
        add_function_type(
            &mut types,
            [ValType::I32, ValType::I64, ValType::I64, ValType::I64],
            [ValType::I32],
        )
    });
    let paging_type = has_paging.then(|| {
        add_function_type(
            &mut types,
            [
                ValType::I32,
                ValType::I32,
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I64,
            ],
            [ValType::I32],
        )
    });
    let invpcid_type = has_invpcid.then(|| {
        add_function_type(
            &mut types,
            [ValType::I32, ValType::I64, ValType::I64, ValType::I64],
            [ValType::I32],
        )
    });
    let processor_type = has_processor.then(|| {
        add_function_type(
            &mut types,
            [ValType::I32, ValType::I32, ValType::I32, ValType::I64],
            [ValType::I32],
        )
    });
    let protection_key_type = has_protection_key
        .then(|| add_function_type(&mut types, [ValType::I32; 4], [ValType::I32]));
    let fast_system_type = has_fast_system.then(|| {
        add_function_type(
            &mut types,
            [
                ValType::I32,
                ValType::I32,
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I64,
                ValType::I64,
            ],
            [ValType::I32],
        )
    });
    let write_segment_base_type = unit
        .instructions
        .iter()
        .any(|i| matches!(i.operation, Operation::SegmentBase { write: true, .. }))
        .then(|| add_function_type(&mut types, [ValType::I32, ValType::I64], [ValType::I32]));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let indexed_paging =
        paged && observed && !mmio && unit.context.is_long64() && unit.context.cpl() == 0;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let retouch_prefix_fetch = indexed_paging
        && !unit.sampled
        && optional.indexed_scalar_read
        && unit.instructions.iter().any(|instruction| {
            matches!(
                instruction.operation,
                Operation::Call {
                    width: crate::x86::instruction::Width::W64,
                    target: crate::x86::instruction::Operand::Register(_),
                }
            )
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let register_prefixes = (paged
        && !mmio
        && loop_body.is_none_or(StructuredLoop::paged_vector_copy)
        && optional.paged_register_prefixes)
        .then(|| {
            paged_register_prefixes(
                unit,
                [loop_body, optional.additional_copy],
                retouch_prefix_fetch && optional.indexed_dword_prefixes,
            )
        })
        .filter(|prefixes| prefixes.iter().any(Option::is_some));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let register_prefix_guard_type = register_prefixes.as_ref().map(|_| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I32, ValType::I64, ValType::I32],
            [ValType::I32],
        )
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let register_prefix_read_type = register_prefixes
        .as_ref()
        .filter(|prefixes| {
            prefixes.iter().enumerate().any(|(first, end)| {
                end.is_some_and(|end| {
                    super::instruction::paged_prefix_read(unit.instructions[first]).is_some()
                        || unit.instructions[first..end].iter().copied().any(|i| {
                            super::instruction::paged_prefix_dword_read(i)
                                || super::instruction::paged_prefix_wide_read(i)
                        })
                })
            })
        })
        .map(|_| {
            add_function_type(
                &mut types,
                [ValType::I64, ValType::I32, ValType::I32],
                [ValType::I32],
            )
        });
    let paged_loop_guard_type = loop_body.and_then(|body| body.paged_fetch_bytes).map(|_| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I32, ValType::I64, ValType::I32],
            [ValType::I32],
        )
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_preamble = loop_body
        .filter(|_| optional.paged_preamble && !mmio && paged_loop_guard_type.is_some())
        .and_then(|body| body.paged_preamble(unit));
    #[cfg(not(feature = "parallel"))]
    let [paged_read_byte, paged_read_dword] =
        loop_body.map_or([false; 2], |body| body.paged_read_widths(unit));
    #[cfg(feature = "parallel")]
    let [paged_read_byte, paged_read_dword] = [false; 2];
    let paged_loop_read_type = (paged_read_byte || paged_read_dword)
        .then(|| add_function_type(&mut types, [ValType::I64], [ValType::I64]));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_sse_write = loop_body.is_some_and(StructuredLoop::paged_sse_write);
    #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
    let paged_sse_write = false;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_vector_copy = loop_body.is_some_and(StructuredLoop::paged_vector_copy);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let copy_group = loop_body.and_then(|body| body.paged_copy_group(unit));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let additional_copy_group = optional
        .additional_copy
        .and_then(|body| body.paged_copy_group(unit));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let copy_prefix = (paged && !mmio && optional.copy_prefix)
        .then(|| CopyGroup::finite_prefix(unit, [loop_body, optional.additional_copy]))
        .flatten();
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let preloaded_copy_prefix = (paged && !mmio && optional.copy_prefix)
        .then(|| CopyGroup::preloaded_prefix(unit, [loop_body, optional.additional_copy]))
        .flatten();
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let preloaded_tail = preloaded_copy_prefix.and_then(|(head, group)| {
        group.preloaded_tail(unit.instructions.get(head..head + 7)?, unit)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let scalar_copy_prefix =
        (paged && !mmio && optional.scalar_copy_prefix && unit.context.is_long64())
            .then(|| ScalarCopyGroup::finite_prefix(unit, [loop_body, optional.additional_copy]))
            .flatten();
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let has_copy_prefix =
        copy_prefix.is_some() || scalar_copy_prefix.is_some() || preloaded_copy_prefix.is_some();
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let copy_prefix_guard_type = has_copy_prefix.then(|| {
        paged_loop_guard_type.unwrap_or_else(|| {
            add_function_type(
                &mut types,
                [ValType::I64, ValType::I32, ValType::I64, ValType::I32],
                [ValType::I32],
            )
        })
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_copy_resolver =
        (copy_group.is_some() || additional_copy_group.is_some() || has_copy_prefix).then(|| {
            add_function_type(
                &mut types,
                [ValType::I64, ValType::I64, ValType::I32],
                [ValType::I32],
            )
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_scalar_writes = loop_body.is_some_and(StructuredLoop::paged_scalar_writes);
    #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
    let paged_scalar_writes = false;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_split_vectors =
        (paged_sse_write || paged_vector_copy) && optional.paged_split_vectors;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_loop_read_block_type = (paged_sse_write || paged_vector_copy).then(|| {
        if paged_split_vectors {
            add_function_type(&mut types, [ValType::I64], [ValType::I32])
        } else {
            add_function_type(
                &mut types,
                [ValType::I64, ValType::I32, ValType::I32],
                [ValType::I32],
            )
        }
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_vector_window_type = (paged_split_vectors
        && optional.paged_vector_window
        && !mmio
        && !paged_vector_copy
        && loop_body.is_some_and(|body| body.has_single_paged_vector_load(unit)))
    .then(|| add_function_type(&mut types, [ValType::I64], [ValType::I64]));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dword_window_type = loop_body
        .is_some_and(|body| body.paged_dword_window && (!mmio || !body.paged_scalar_writes()))
        .then(|| add_function_type(&mut types, [ValType::I64], [ValType::I32]));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dword_read_pair = loop_body
        .filter(|_| optional.paged_dword_read_pair && paged_scalar_writes && !mmio)
        .and_then(|body| body.paged_dword_read_pair(unit));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dword_read_pair_type = paged_dword_read_pair.map(|_| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I32, ValType::I32],
            [ValType::I32],
        )
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_indexed_write = loop_body
        .filter(|body| {
            optional.paged_indexed_write
                && paged_scalar_writes
                && !mmio
                && body.paged_source_is_4k(unit)
        })
        .and_then(|body| super::paged_indexed_write::IndexedWrite::recognize(body, unit))
        .map(|shape| {
            let signature = add_function_type(
                &mut types,
                [ValType::I64, ValType::I64, ValType::I64, ValType::I32],
                [ValType::I32],
            );
            (shape, signature)
        });
    #[cfg(not(feature = "parallel"))]
    let paged_write_byte = loop_body.is_some_and(|body| body.has_paged_byte_store(unit));
    #[cfg(feature = "parallel")]
    let paged_write_byte = false;
    let paged_write_dword = paged_sse_write
        || paged_scalar_writes
        || loop_body.is_some_and(|body| body.paged_stack_writes);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let prefix_write_dword = register_prefixes.as_ref().is_some_and(|prefixes| {
        prefixes.iter().enumerate().any(|(first, end)| {
            end.is_some_and(|end| {
                unit.instructions[first + 1..end]
                    .iter()
                    .copied()
                    .any(super::instruction::paged_prefix_dword_store)
            })
        })
    });
    #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
    let prefix_write_dword = false;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_vector_write_type = paged_vector_copy
        .then(|| add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I32]));
    let paged_loop_write_type = (paged_write_byte || paged_write_dword || prefix_write_dword)
        .then(|| add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I32]));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_byte_write_pair = loop_body
        .filter(|_| optional.paged_byte_write_pair && !mmio)
        .and_then(|body| body.paged_byte_write_pair(unit));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_byte_write_pair_type = paged_byte_write_pair.map(|_| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I32, ValType::I32],
            [ValType::I32],
        )
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dense_bytes = loop_body.is_some_and(|body| {
        body.paged_read_span.is_some_and(|proof| {
            proof.dense_ascending_bytes(&unit.instructions[body.head as usize..=body.tail as usize])
        })
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_read_rearm_type = loop_body
        .filter(|body| {
            let dword_replay = paged_read_dword
                && (body.forward_edges()
                    || body.paged_sse() && body.paged_read_span.is_some()
                    || !paged_read_byte && body.guarded_dword_reads(unit));
            let read_only_replay =
                !paged_write_byte && !paged_write_dword && (dword_replay || paged_dense_bytes);
            let writable_byte_replay = paged_write_byte && paged_read_byte;
            optional.paged_read_rearm
                && !mmio
                && body.paged_fetch_bytes.is_some()
                && (read_only_replay
                    || writable_byte_replay
                    || paged_sse_write
                    || paged_vector_copy
                    || paged_scalar_writes)
        })
        .map(|body| {
            (
                add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I32]),
                body.order(),
            )
        });
    let debug_begin_type = observed.then(|| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I64, ValType::I32],
            [ValType::I32],
        )
    });
    let debug_type = observed.then(|| {
        add_function_type(
            &mut types,
            [
                ValType::I64,
                ValType::I64,
                ValType::I32,
                ValType::I64,
                ValType::I32,
            ],
            [ValType::I32],
        )
    });
    let external_step_type = has_boundary.then(|| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I64, ValType::I32],
            [ValType::I32],
        )
    });
    let halt_type = has_halt.then(|| add_function_type(&mut types, [ValType::I64], [ValType::I32]));
    let io_type = unit
        .instructions
        .iter()
        .any(|i| {
            matches!(
                i.operation,
                Operation::Io { .. }
                    | Operation::String {
                        kind: crate::x86::instruction::StringKind::Ins
                            | crate::x86::instruction::StringKind::Outs,
                        ..
                    }
            )
        })
        .then(|| {
            add_function_type(
                &mut types,
                [
                    ValType::I32,
                    ValType::I32,
                    ValType::I64,
                    ValType::I64,
                    ValType::I32,
                    ValType::I64,
                ],
                [ValType::I32],
            )
        });
    let numeric_state_type = (has_mmx || has_numeric_image).then(|| {
        add_function_type(
            &mut types,
            [ValType::I32, ValType::I64, ValType::I32],
            [ValType::I32],
        )
    });
    let mut imports = ImportSection::new();
    imports.import(
        "env",
        "memory",
        EntityType::Memory(MemoryType {
            minimum: 1,
            maximum: None,
            memory64: false,
            shared: false,
            page_size_log2: None,
        }),
    );
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    {
        imports.import(
            "env",
            "shared_ram",
            EntityType::Memory(MemoryType {
                minimum: 1,
                maximum: Some(65536),
                memory64: false,
                shared: true,
                page_size_log2: None,
            }),
        );
        imports.import(
            "env",
            "parallel_signal",
            EntityType::Global(wasm_encoder::GlobalType {
                val_type: ValType::I32,
                mutable: false,
                shared: false,
            }),
        );
    }
    imports.import(
        "env",
        if segmented { "segmented_load" } else { "load" },
        EntityType::Function(0),
    );
    imports.import(
        "env",
        if segmented {
            "segmented_store"
        } else {
            "store"
        },
        EntityType::Function(1),
    );
    imports.import(
        "env",
        if segmented {
            "segmented_access_failed"
        } else {
            "access_failed"
        },
        EntityType::Function(2),
    );
    let write_intent = unit.instructions.iter().any(|instruction| {
        matches!(instruction.operation,
            Operation::MemoryAlu { kind, operands: MemoryAluOperands::MemoryFirst(_), .. }
                if kind.writes_result())
            || matches!(instruction.operation,
                Operation::Bit { kind, dst:Location::Memory(_), .. } if kind.writes_result())
            || matches!(
                instruction.operation,
                Operation::Unary {
                    dst: Location::Memory(_),
                    ..
                } | Operation::Shift {
                    dst: Location::Memory(_),
                    ..
                } | Operation::DoubleShift {
                    dst: Location::Memory(_),
                    ..
                } | Operation::Exchange {
                    dst: Location::Memory(_),
                    ..
                } | Operation::Xadd {
                    dst: Location::Memory(_),
                    ..
                } | Operation::CompareExchange {
                    dst: Location::Memory(_),
                    ..
                } | Operation::CompareExchange8b { .. }
            )
    });
    if write_intent {
        imports.import(
            "env",
            if segmented {
                "segmented_load_for_write"
            } else {
                "load_for_write"
            },
            EntityType::Function(0),
        );
    }
    let mut run_index = 3 + u32::from(write_intent);
    let mut import_function = |name, signature| {
        imports.import("env", name, EntityType::Function(signature));
        let index = run_index;
        run_index += 1;
        index
    };
    let multiply_high = multiply_type.map(|index| import_function("multiply_high", index));
    let divide = divide_type.map(|index| import_function("divide", index));
    let divide_failed = wide_divide.then(|| import_function("divide_failed", 2));
    let access_reason = (segmented || paged).then(|| import_function("access_reason", 2));
    let mmio_operand = mmio_type.map(|index| import_function("mmio_operand", index));
    let target_check = segmented.then(|| import_function("target_check", 4));
    let vector_load = vector_load_type.map(|index| import_function("vector_load", index));
    let block16_for_write =
        block16_for_write_type.map(|index| import_function("block16_for_write", index));
    let vector_store = vector_store_type.map(|index| import_function("vector_store", index));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let indexed_store_fill = (paged
        && observed
        && !mmio
        && unit.context.is_long64()
        && unit.context.cpl() == 0
        && optional.indexed_scalar_read
        && unit.instructions.iter().any(|instruction| {
            super::instruction::indexed_scalar_write(instruction.operation, unit.sampled)
        }))
    .then(|| {
        let signature = add_function_type(&mut types, [ValType::I64, ValType::I32], []);
        import_function("paged_store_fill", signature)
    });
    let float = float_type.map(|index| import_function("float", index));
    let load_mxcsr = load_mxcsr_used.then(|| import_function("load_mxcsr", mxcsr_type.unwrap()));
    let store_mxcsr = store_mxcsr_used.then(|| import_function("store_mxcsr", mxcsr_type.unwrap()));
    let io = io_type.map(|index| import_function("io", index));
    let x87 = x87_type.map(|index| import_function("x87", index));
    let mmx = has_mmx.then(|| import_function("mmx", numeric_state_type.unwrap()));
    let numeric_image =
        has_numeric_image.then(|| import_function("numeric_image", numeric_state_type.unwrap()));
    let masked_store = masked_store_type.map(|index| import_function("masked_store", index));
    let clflush = clflush_type.map(|index| import_function("clflush", index));
    let protection = protection_type.map(|index| import_function("protection", index));
    let compound_stack = compound_stack_type.map(|index| import_function("compound_stack", index));
    let far_control = far_type.map(|index| import_function("far_control", index));
    let software_event = software_event_type.map(|index| import_function("software_event", index));
    let system_flags = system_flags_type.map(|index| import_function("flags", index));
    let debug_register = debug_register_type.map(|index| import_function("debug_register", index));
    let paging_control = paging_type.map(|index| import_function("paging_control", index));
    let invpcid = invpcid_type.map(|index| import_function("invpcid", index));
    let fast_system = fast_system_type.map(|index| import_function("fast_system", index));
    let write_segment_base =
        write_segment_base_type.map(|index| import_function("write_segment_base", index));
    let processor = processor_type.map(|index| import_function("processor", index));
    let protection_key = protection_key_type.map(|index| import_function("protection_key", index));
    #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
    let paged_dword_walk = !mmio
        && !paged_read_byte
        && paged_read_dword
        && loop_body.is_some_and(|body| body.guarded_dword_reads(unit));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let register_prefix_guard =
        register_prefix_guard_type.map(|index| import_function("paged_loop_guard", index));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let register_prefix_read =
        register_prefix_read_type.map(|index| import_function("paged_loop_read_span", index));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let preloaded_tail_read = preloaded_tail.map(|_| {
        register_prefix_read.unwrap_or_else(|| {
            let signature = add_function_type(
                &mut types,
                [ValType::I64, ValType::I32, ValType::I32],
                [ValType::I32],
            );
            import_function("paged_loop_read_span", signature)
        })
    });
    let paged_loop_guard = paged_loop_guard_type.map(|index| {
        #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
        if paged_dword_walk {
            const _: () = assert!("paged_walk_guard".len() == "paged_loop_guard".len());
            const _: () =
                assert!("paged_walk_loop_guard32".len() == "paged_read_loop_guard32".len());
            let name = if unit.context.is_long64() {
                "paged_walk_guard"
            } else {
                "paged_walk_loop_guard32"
            };
            return import_function(name, index);
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if paged_sse_write && !unit.context.is_long64() {
            return import_function("paged_sse_write_loop_guard32", index);
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if paged_vector_copy {
            return import_function("paged_sse_loop_guard", index);
        }
        #[cfg(not(feature = "parallel"))]
        if paged_sse_write || loop_body.is_some_and(StructuredLoop::paged_sse) {
            return import_function("paged_sse_loop_guard", index);
        }
        let name = if loop_body.is_some_and(|body| body.paged_stack_writes) {
            "paged_write_loop_guard32"
        } else if !unit.context.is_long64() && (paged_read_byte || paged_read_dword) {
            "paged_read_loop_guard32"
        } else {
            "paged_loop_guard"
        };
        import_function(name, index)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_preamble_stack_guard = paged_preamble
        .filter(|body| body.stack_reads)
        .and(paged_loop_guard_type)
        .map(|index| import_function("paged_write_loop_guard32", index));
    let paged_loop_read_byte = paged_loop_read_type
        .filter(|_| paged_read_byte)
        .map(|index| import_function("paged_loop_read_byte", index));
    let paged_loop_read_dword = paged_loop_read_type
        .filter(|_| paged_read_dword)
        .map(|index| {
            #[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
            if paged_dword_walk {
                // Keep the old artifact size and its size-limit fallbacks.
                const _: () =
                    assert!("paged_loop_walk_dword".len() == "paged_loop_read_dword".len());
                return import_function("paged_loop_walk_dword", index);
            }
            import_function("paged_loop_read_dword", index)
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_loop_read_block = paged_loop_read_block_type.map(|index| {
        let name = if paged_split_vectors {
            "paged_loop_block_host"
        } else {
            "paged_loop_read_span"
        };
        (import_function(name, index), paged_split_vectors)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_loop_write_block =
        paged_vector_write_type.map(|index| import_function("paged_loop_write_block", index));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_copy_resolver =
        paged_copy_resolver.map(|signature| import_function("paged_loop_copy_window", signature));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let copy_prefix_guard = if paged_vector_copy {
        paged_loop_guard
    } else {
        copy_prefix_guard_type.map(|signature| import_function("paged_sse_loop_guard", signature))
    };
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_read_rearm = paged_read_rearm_type.map(|(index, order)| {
        let name = if paged_split_vectors {
            "paged_loop_split_read_ready"
        } else {
            "paged_loop_read_ready"
        };
        (import_function(name, index), order)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_write_ready = paged_read_rearm_type
        .filter(|_| paged_sse_write || paged_scalar_writes)
        .map(|(index, _)| import_function("paged_loop_write_ready", index));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_vector_retouch = paged_vector_window_type
        .filter(|_| optional.paged_vector_retouch && unit.context.is_long64())
        .zip(paged_loop_read_block_type)
        .zip(loop_body.and_then(|body| body.paged_vector_alignment(unit)))
        .map(|((resolve, touch), alignment)| {
            let resolve = import_function("paged_loop_retouchable_block_window", resolve);
            let touch = import_function("paged_loop_retouch_block_window", touch);
            (alignment, resolve, touch)
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_vector_window =
        paged_vector_window_type.map(|index| import_function("paged_loop_block_window", index));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dword_window =
        paged_dword_window_type.map(|index| import_function("paged_loop_read_dword_window", index));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dword_window_touch = paged_dword_window_type
        .filter(|_| loop_body.is_some_and(StructuredLoop::paged_scalar_writes))
        .map(|index| import_function("paged_loop_retouch_block_window", index));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_indexed_write = paged_indexed_write.map(|(shape, signature)| {
        (
            shape,
            import_function("paged_loop_indexed_dword_write", signature),
        )
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dword_read_pair = paged_dword_read_pair.zip(paged_dword_read_pair_type).map(
        |((first, second, base, handoff), index)| {
            (
                first,
                second,
                base,
                handoff,
                import_function("paged_loop_read_span", index),
            )
        },
    );
    let paged_loop_write_byte = paged_loop_write_type
        .filter(|_| paged_write_byte)
        .map(|index| import_function("paged_loop_write_byte", index));
    let write_dword = paged_loop_write_type
        .filter(|_| paged_write_dword || prefix_write_dword)
        .map(|index| import_function("paged_loop_write_dword", index));
    let paged_loop_write_dword = write_dword.filter(|_| paged_write_dword);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_byte_write_pair =
        paged_byte_write_pair
            .zip(paged_byte_write_pair_type)
            .map(|((load, store), write_type)| {
                let read = if optional.paged_byte_write_pointer {
                    "paged_loop_read_byte_write_pointer"
                } else {
                    "paged_loop_read_byte_for_store"
                };
                let read = import_function(read, paged_loop_read_type.unwrap());
                let write = import_function("paged_loop_write_byte_after_read", write_type);
                (load, store, read, write)
            });
    let debug_begin = debug_begin_type.map(|index| import_function("debug_begin", index));
    let debug_step = debug_type.map(|index| import_function("debug_finish", index));
    let flat_retire = optional.flat_trace.then(|| {
        let signature = add_function_type(&mut types, [ValType::I64], []);
        import_function("flat_region_retire", signature)
    });
    let external_step = external_step_type.map(|index| import_function("external_step", index));
    let halt = halt_type.map(|index| import_function("halt", index));
    let vector_access_reason = if vector_banks.loads || vector_store.is_some() || store_mxcsr_used {
        Some(access_reason.unwrap_or_else(|| import_function("access_reason", 2)))
    } else {
        None
    };
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_atomic = unit
        .instructions
        .iter()
        .any(|instruction| instruction.shared_atomic())
        .then(|| {
            let release_type = add_function_type(&mut types, [ValType::I32], []);
            let guard_type = add_function_type(
                &mut types,
                [ValType::I64, ValType::I32, ValType::I32],
                [ValType::I32],
            );
            let unguard_type = add_function_type(&mut types, [], []);
            let begin = import_function("parallel_begin", 2);
            import_function("parallel_end", release_type);
            import_function("parallel_guard", guard_type);
            import_function("parallel_unguard", unguard_type);
            begin
        });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_resolve = (!paged && !observed).then(|| {
        let signature = add_function_type(
            &mut types,
            [ValType::I64, ValType::I32, ValType::I32, ValType::I32],
            [ValType::I32],
        );
        import_function("parallel_resolve", signature)
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_vector_dword = (matches!(_guard_reads, GuardedReads::Shared { .. })
        && parallel_resolve.is_some()
        && vector_banks.loads)
        .then(|| {
            let signature =
                add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I64]);
            import_function("parallel_vector_dword", signature)
        });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let compact_shared_reads = matches!(_guard_reads, GuardedReads::Shared { compact: true, .. });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let allow_indexed_bytes = matches!(
        _guard_reads,
        GuardedReads::Shared {
            indexed: SharedIndexedReads::IncludeBytes | SharedIndexedReads::IncludeBaseBytes,
            compact: false,
            ..
        }
    );
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let has_indexed_window = matches!(
        _guard_reads,
        GuardedReads::Shared { indexed, .. } if indexed != SharedIndexedReads::Disabled
    ) && unit.instructions.iter().any(|i| {
        parallel::DwordWindow::eligible(i.operation, allow_indexed_bytes, unit.context.is_long64())
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let has_base_byte_window = matches!(
        _guard_reads,
        GuardedReads::Shared {
            indexed: SharedIndexedReads::IncludeBaseBytes,
            compact: false,
            ..
        }
    ) && unit
        .instructions
        .iter()
        .any(|i| parallel::DwordWindow::eligible_base_byte(i.operation, unit.context.is_long64()));
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let window_resolve = (has_indexed_window || has_base_byte_window).then(|| {
        let signature = add_function_type(&mut types, [ValType::I64], [ValType::I32]);
        import_function("parallel_dword_window", signature)
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let store_span_resolve = (matches!(
        _guard_reads,
        GuardedReads::Shared {
            byte_stores: true,
            ..
        }
    ) && unit
        .instructions
        .iter()
        .any(|i| parallel::ByteStoreSpan::eligible(i.operation, unit.context.is_long64())))
    .then(|| {
        let signature = add_function_type(&mut types, [ValType::I64], [ValType::I64]);
        import_function("parallel_byte_store_span", signature)
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let vector_span_groups = loop_body
        .filter(|body| _extra_spans && parallel_vector_dword.is_some() && body.is_shared_float())
        .map_or([None; 2], |body| {
            parallel::VectorSpanMemo::groups(
                &unit.instructions[body.head as usize..=body.tail as usize],
            )
        });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_vector_span = vector_span_groups.iter().any(Option::is_some).then(|| {
        let signature = add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I64]);
        import_function("parallel_vector_dword_span", signature)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let span_cycles: Vec<_> = unit
        .instructions
        .iter()
        .filter_map(|instruction| {
            let pc = instruction.pc;
            match instruction.operation {
                Operation::Jump { target }
                | Operation::Jcc { target, .. }
                | Operation::CountBranch { target, .. }
                    if target <= pc =>
                {
                    Some(target..=pc)
                }
                Operation::String { repeat, .. } if repeat != Repeat::None => Some(pc..=pc),
                _ => None,
            }
        })
        .collect();
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let linear_call_spans = _extra_spans
        && !observed
        && !paged
        && !mmio
        && unit.context.mode() == crate::x86::context::Mode::Protected
        && unit.context.nonwrapping_code32_fetch(0, 1) == Some(0)
        && unit.context.stack_width() == crate::x86::instruction::Width::W32
        && !span_cycles.is_empty()
        && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Call { target, .. }
                if !matches!(target, crate::x86::instruction::Operand::Immediate(_)))
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let flat_integer_groups = (optional.flat_integer_groups && unit.sampled && linear_call_spans)
        .then(|| flat_integer_groups(unit, loop_body))
        .filter(|groups| groups.iter().any(Option::is_some));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let dense_dispatch = if dynamic_targets
        && (unit.sampled && linear_call_spans || indexed_paging)
        // Structured clones can hand off an entry ordinal independently of RIP.
        && loop_body.is_none()
        && optional.additional_copy.is_none()
    {
        let first = unit.instructions.first().unwrap().pc;
        let distance = unit.instructions.last().unwrap().pc - first;
        (distance < (unit.instructions.len() as u64 * 6).min(1024)).then(|| (first, distance + 1))
    } else {
        None
    };
    #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
    let dense_dispatch: Option<(u64, u64)> = None;
    let target_lookup = dynamic_targets && dense_dispatch.is_none();
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let mut span_import = |name| {
        let signature = add_function_type(
            &mut types,
            [ValType::I64, ValType::I32, ValType::I32],
            [ValType::I32],
        );
        import_function(name, signature)
    };
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_read_span = loop_body
        .and_then(|body| body.paged_read_span)
        .map(|proof| (proof, span_import("paged_loop_read_span")));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dense_read_span = (optional.paged_dense_read_span && paged_dense_bytes).then(|| {
        const _: () =
            assert!("paged_loop_read_span_ready".len() == "paged_loop_read_span_pages".len());
        let name = if loop_body.is_some_and(|body| body.paged_source_is_4k(unit)) {
            "paged_loop_read_span_ready"
        } else {
            "paged_loop_read_span_pages"
        };
        span_import(name)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let read_span = (!observed && !span_cycles.is_empty()).then(|| span_import("read_span"));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let stack_read_span = read_span.filter(|_| {
        linear_call_spans || (_extra_spans && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Load { address, .. } if address.segment == Segment::Ss)
                && span_cycles.iter().any(|range| range.contains(&instruction.pc))
        }))
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let direct_vector_stores =
        optional.direct_vector_stores && vector_banks.stores && !observed && !paged && !mmio;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let (write_segments, cyclic_vector_store) = {
        let mut segments = [linear_call_spans; 2];
        let mut cyclic_vector_store = false;
        if !observed && _extra_spans {
            for instruction in &unit.instructions {
                let store = match instruction.operation {
                    Operation::Store { address, .. }
                    | Operation::NonTemporalStore { address, .. } => Some((address, false)),
                    op if direct_vector_stores => {
                        vector::unaligned_store_address(op).map(|address| (address, true))
                    }
                    _ => None,
                };
                if let Some((address, vector)) = store
                    && span_cycles
                        .iter()
                        .any(|range| range.contains(&instruction.pc))
                {
                    // Stack spills often alternate with writes to a separate backing.
                    segments[usize::from(address.segment == Segment::Ss)] = true;
                    cyclic_vector_store |= vector;
                }
            }
        }
        (segments, cyclic_vector_store)
    };
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let write_span = write_segments
        .contains(&true)
        .then(|| span_import("write_span"));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_f32_read_pair = loop_body
        .filter(|body| optional.paged_f32_read_pair && body.paged_sse())
        .zip(paged_read_span)
        .and_then(|(body, (proof, _))| {
            let instructions = &unit.instructions[body.head as usize..=body.tail as usize];
            let last_stream = proof.last_memory_stream(instructions)?;
            let signature = add_function_type(
                &mut types,
                [ValType::I64, ValType::I32, ValType::I64, ValType::I32],
                [ValType::I64],
            );
            let pair = import_function("paged_loop_read_pair", signature);
            let split = (optional.paged_f32_split_pair && proof.dense_f32_lap(instructions))
                .then(|| import_function("paged_loop_read_split_pair", signature));
            Some((pair, last_stream, split))
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let linear_recurrence = loop_body
        .filter(|body| {
            optional.linear_recurrence && (!observed || body.paged_fetch_bytes.is_some())
        })
        .and_then(|body| body.linear_recurrence(unit));
    let mut functions = FunctionSection::new();
    functions.function(3);
    if target_lookup {
        functions.function(4);
    }
    let mut exports = ExportSection::new();
    exports.export("run", ExportKind::Func, run_index);
    let mut locals = vec![
        (19, ValType::I64),
        (2, ValType::I32),
        (5, ValType::I64),
        (1, ValType::I32),
        (2, ValType::I64),
        (1, ValType::I32),
    ];
    if has_repeat {
        locals.push((2, ValType::I64));
    }
    let mut vector_registers = [None; 16];
    let mut next_local = 35 + 2 * u32::from(has_repeat);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let preloaded_loop = preloaded_tail.zip(preloaded_tail_read).map(|(tail, read)| {
        let local = next_local;
        locals.push((4, ValType::I32));
        next_local += 4;
        (tail, read, [local, local + 1, local + 2, local + 3])
    });
    // The admitted helpers leave the high dwords in the backing RunState.
    let narrow_gprs = !observed && !cfg!(feature = "parallel") && state::narrow_gprs(unit);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let complete_flat_call = narrow_gprs
        && linear_call_spans
        && dense_dispatch.is_some()
        && !unit.reached_instruction_limit;
    #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
    let complete_flat_call = false;
    let gpr_banks = if complete_flat_call {
        // The complete learned owner keeps the inaccessible banks in RunState.
        8
    } else {
        16
    };
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let flat_order = (complete_flat_call && !optional.flat_trace && flat_integer_groups.is_some())
        .then(|| flat_call_order(&unit.instructions))
        .flatten();
    #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
    let flat_order: Option<Vec<usize>> = None;
    let instruction_index = |placement| {
        flat_order
            .as_ref()
            .map_or(placement, |order| order[placement])
    };
    let dword_writes = if narrow_gprs {
        0
    } else {
        loop_body.map_or(0, |body| body.dword_writes(unit))
    };
    let register_upper_dwords = std::array::from_fn(|bank| {
        if dword_writes & (1 << bank) == 0 {
            return None;
        }
        let local = next_local;
        next_local += 1;
        Some(local)
    });
    if dword_writes != 0 {
        locals.push((dword_writes.count_ones(), ValType::I64));
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let mut span_cache = |resolve| {
        let first = next_local;
        locals.push((2, ValType::I64));
        locals.push((2, ValType::I32));
        next_local += 4;
        memory::SpanCache::new(resolve, first)
    };
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let read_span = read_span.map(&mut span_cache);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let stack_dword_bound =
        complete_flat_call && unit.context.segment(Segment::Ss).limit == u32::MAX;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let flat_stack_pointer_span = stack_read_span
        .filter(|_| {
            !paged
                && !mmio
                && unit.sampled
                && unit.context.mode() == crate::x86::context::Mode::Protected
                && unit.context.code32()
        })
        .map(&mut span_cache)
        .map(|span| span.with_dword_bound(stack_dword_bound));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let stack_read_span = stack_read_span.map(&mut span_cache);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let mut write_caches =
        write_segments.map(|used| write_span.filter(|_| used).map(&mut span_cache));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    {
        write_caches[1] = write_caches[1].map(|span| span.with_dword_bound(stack_dword_bound));
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let flat_stack_windows = flat_integer_groups
        .as_ref()
        .filter(|_| {
            complete_flat_call && flat_stack_pointer_span.is_some() && write_caches[1].is_some()
        })
        .map(|groups| {
            groups
                .iter()
                .enumerate()
                .map(|(first, end)| {
                    super::flat_stack::FlatStackWindow::recognize(
                        &unit.context,
                        &unit.instructions[first..(*end)?],
                    )
                })
                .collect::<Vec<_>>()
        })
        .filter(|windows| windows.iter().any(Option::is_some));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let flat_stack_host = flat_stack_windows.as_ref().map(|_| {
        let host = next_local;
        locals.push((1, ValType::I32));
        next_local += 1;
        host
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let flat_data_windows = flat_integer_groups
        .as_ref()
        .filter(|_| complete_flat_call && read_span.is_some() && write_caches[0].is_some())
        .map(|groups| {
            groups
                .iter()
                .enumerate()
                .map(|(first, end)| {
                    super::flat_data::FlatDataWindow::recognize(
                        &unit.context,
                        &unit.instructions[first..(*end)?],
                    )
                })
                .collect::<Vec<_>>()
        })
        .filter(|windows| windows.iter().any(Option::is_some));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let flat_data_bias = flat_data_windows.as_ref().map(|_| {
        let bias = next_local;
        locals.push((1, ValType::I32));
        next_local += 1;
        bias
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let linear_scalar_copy_prefix =
        (!paged && !observed && !mmio && optional.scalar_copy_prefix && !unit.context.is_long64())
            .then(|| ScalarCopyGroup::finite_prefix(unit, [loop_body, None]))
            .flatten()
            .zip(read_span.zip(write_caches[0]));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let linear_vector_copy_prefixes = (!paged && !observed && !mmio && optional.scalar_copy_prefix)
        .then_some(read_span.zip(write_caches[0]))
        .flatten()
        .map(|spans| (CopyGroup::flat_prefixes(unit, loop_body), spans))
        .filter(|(prefixes, _)| !prefixes.is_empty());
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let flat_copy_start_remaining = linear_vector_copy_prefixes
        .as_ref()
        .filter(|(prefixes, _)| prefixes.iter().any(|(_, copy)| copy.can_defer_flags()))
        .map(|_| {
            let start = next_local;
            locals.push((1, ValType::I64));
            next_local += 1;
            start
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let flat_copy_window = linear_vector_copy_prefixes
        .as_ref()
        .filter(|(prefixes, _)| {
            prefixes
                .iter()
                .any(|(_, copy)| copy.can_defer_flags() && copy.can_retain_window())
        })
        .map(|_| {
            let window = next_local;
            locals.push((1, ValType::I32));
            next_local += 1;
            window
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let read_loop_diamond = loop_body.and_then(|body| {
        let enabled = if paged {
            optional.paged_read_diamond
                && !mmio
                // The shape has one dword read. Keep the existing scalar
                // placement in reserve when the window tier exceeds its cap.
                && body.paged_dword_window
                && paged_read_dword
                && !paged_write_byte
                && !paged_write_dword
        } else {
            matches!(_guard_reads, GuardedReads::Outlined { diamond: true, .. })
        };
        if !enabled || !body.forward_edges() {
            return None;
        }
        loop_cfg::ReadLoopDiamond::recognize(&unit.instructions, body.order(), &unit.context, paged)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let proved_stack = loop_body.and_then(|body| {
        let proof = (_guard_reads != GuardedReads::Checked)
            .then(|| body.stack_proof(unit))
            .flatten()?;
        let read = read_span?.resolve;
        let write = if proof.writes {
            Some(write_span?)
        } else {
            None
        };
        let host = next_local;
        next_local += 1;
        locals.push((1, ValType::I32));
        Some(stack_slots::StackGuard {
            slots: proof,
            read,
            write,
            host,
        })
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let indexed_read_window = loop_body.and_then(|body| {
        if !matches!(
            _guard_reads,
            GuardedReads::Outlined {
                indexed_read: true,
                ..
            }
        ) {
            return None;
        }
        let span = read_span?;
        let address = body.indexed_read_address(unit)?;
        let host = next_local;
        locals.extend([(1, ValType::I32), (1, ValType::I64)]);
        next_local += 2;
        Some(read_loop::IndexedReadWindow {
            address,
            span,
            host,
            limit: host + 1,
        })
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let proved_integer_reads = loop_body.and_then(|body| {
        let span = read_span?;
        let proof = (_guard_reads != GuardedReads::Checked)
            .then(|| body.integer_read_proof(unit))
            .flatten()?;
        let first_bias = next_local;
        locals.push((read_loop::ReadLoop::BIAS_LOCALS, ValType::I32));
        next_local += read_loop::ReadLoop::BIAS_LOCALS;
        Some((proof, span, first_bias))
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let proved_reads = loop_body.and_then(|body| {
        let span = read_span?;
        let proof = (_guard_reads != GuardedReads::Checked)
            .then(|| body.read_proof(unit))
            .flatten()?;
        let first_bias = next_local;
        let guard_failed = first_bias + read_loop::ReadLoop::BIAS_LOCALS;
        let retry = guard_failed + 1;
        locals.push((read_loop::ReadLoop::BIAS_LOCALS + 2, ValType::I32));
        next_local = retry + 1;
        Some((proof, span, first_bias, guard_failed, (body.tail, retry)))
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_atomic = parallel_atomic.map(|begin| {
        let token = next_local;
        locals.push((2, ValType::I32));
        next_local += 2;
        parallel::Atomic::new(begin, token)
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_scalar = parallel_resolve.map(|resolve| {
        let first = next_local;
        locals.push((2, ValType::I32));
        locals.push((4, ValType::I64));
        next_local += 6;
        parallel::Scalar::new(resolve, first)
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_vector_span = parallel_vector_span.map(|resolve| {
        let memos = vector_span_groups.map(|key| {
            key.map(|key| {
                let first = next_local;
                locals.push((2, ValType::I64));
                locals.push((1, ValType::I32));
                next_local += 3;
                parallel::VectorSpanMemo::new(key, first)
            })
        });
        let budget_floor = next_local;
        locals.push((1, ValType::I64));
        next_local += 1;
        (resolve, memos, budget_floor)
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_address_memos = if parallel_scalar.is_some()
        && unit.instructions.iter().any(|i| {
            matches!(i.operation, Operation::Jump { target } | Operation::Jcc { target, .. }
            | Operation::CountBranch { target, .. } if target <= i.pc)
        }) {
        let entries = (0..4)
            .map(|index| {
                let address = next_local + index * 2;
                locals.push((1, ValType::I64));
                locals.push((1, ValType::I32));
                (address, address + 1)
            })
            .collect::<Vec<_>>();
        next_local += (entries.len() as u32) * 2;
        entries
    } else {
        Vec::new()
    };
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_native_lock_memo = if optional.parallel_native_lock_memo
        && !parallel_address_memos.is_empty()
        && unit.context.is_long64()
    {
        parallel::native_lock_memo_site(&unit.instructions).map(|site| {
            let address = next_local;
            locals.extend([(1, ValType::I64), (1, ValType::I32)]);
            next_local += 2;
            (site, address, address + 1)
        })
    } else {
        None
    };
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let (parallel_dword_window, parallel_base_byte_window) = {
        let wide = unit.context.is_long64();
        let mut allocate = |resolve, allow_bytes| {
            let first = next_local;
            if wide {
                locals.extend([(2, ValType::I64), (2, ValType::I32)]);
            } else {
                locals.push((4, ValType::I32));
            }
            next_local += 4;
            parallel::DwordWindow::new(resolve, first, wide, allow_bytes)
        };
        let indexed = window_resolve
            .filter(|_| has_indexed_window)
            .map(|resolve| allocate(resolve, allow_indexed_bytes));
        let base = window_resolve
            .filter(|_| has_base_byte_window)
            .map(|resolve| allocate(resolve, true));
        (indexed, base)
    };
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_byte_store_span = store_span_resolve.map(|resolve| {
        let first = next_local;
        locals.extend([(2, ValType::I64), (1, ValType::I32)]);
        next_local += 3;
        parallel::ByteStoreSpan::new(resolve, first)
    });
    let paged_loop_guard = paged_loop_guard.map(|guard| {
        let attempts = next_local;
        next_local += 1;
        locals.push((1, ValType::I32));
        (guard, attempts)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let prefix_fetch_handoff = register_prefixes
        .as_deref()
        .and_then(|prefixes| {
            paged_prefix_handoffs(
                unit,
                prefixes,
                [
                    copy_prefix.map(|(head, _)| head),
                    preloaded_copy_prefix.map(|(head, _)| head),
                    scalar_copy_prefix.map(|(head, _)| head),
                ],
            )
        })
        .map(|edges| {
            let handoff = next_local;
            locals.push((1, ValType::I32));
            next_local += 1;
            (handoff, edges)
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_preamble = paged_preamble.map(|body| {
        let handoff = next_local;
        locals.push((2, ValType::I32));
        next_local += 2;
        (body, handoff, handoff + 1)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_preamble_latch = optional.paged_preamble_latch
        && paged_preamble.is_none_or(|(preamble, _, _)| !preamble.stack_reads);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_read_span = paged_read_span.map(|(proof, resolve)| {
        let bias = next_local;
        let biases = if loop_body.is_some_and(StructuredLoop::paged_sse) {
            read_loop::ReadLoop::BIAS_LOCALS
        } else {
            1
        };
        locals.push((biases, ValType::I32));
        next_local += biases;
        (proof, resolve, bias)
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_f32_prefix_floor = paged_f32_read_pair
        .and_then(|(_, _, split)| split)
        .map(|_| {
            let floor = next_local;
            locals.push((1, ValType::I64));
            next_local += 1;
            floor
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_vector_rearm = paged_vector_window.is_some()
        && optional.paged_vector_rearm
        && paged_read_rearm.is_some()
        && unit.context.is_long64()
        && loop_body.is_some_and(|body| body.paged_source_is_4k(unit));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let mut read_window = |resolve, early_rearm: bool, retouch: Option<((u8, i64), u32, u32)>| {
        let base = next_local;
        locals.extend([(2, ValType::I64), (1, ValType::I32)]);
        next_local += 3;
        let last_early_page = early_rearm.then(|| {
            let local = next_local;
            locals.push((1, ValType::I64));
            next_local += 1;
            local
        });
        let retouch = retouch.map(|(alignment, resolve, touch)| {
            let dirty = next_local;
            locals.push((1, ValType::I32));
            next_local += 1;
            memory::VectorWindowRetouch {
                alignment,
                resolve,
                touch,
                dirty,
            }
        });
        memory::PagedReadWindow {
            resolve,
            base,
            limit: base + 1,
            host: base + 2,
            last_early_page,
            retouch,
        }
    };
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let (paged_dword_window, paged_vector_window) = (
        paged_dword_window.map(|resolve| read_window(resolve, false, None)),
        paged_vector_window
            .map(|resolve| read_window(resolve, paged_vector_rearm, paged_vector_retouch)),
    );
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let copy_window = |group, resolve, locals: &mut Vec<(u32, ValType)>, next_local: &mut u32| {
        let first = *next_local;
        locals.extend([(3, ValType::I64), (2, ValType::I32)]);
        *next_local += 5;
        super::paged_copy::CopyWindow {
            group,
            resolve,
            source_base: first,
            destination_base: first + 1,
            limit: first + 2,
            source_host: first + 3,
            destination_host: first + 4,
        }
    };
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_copy_window = copy_group
        .zip(paged_copy_resolver)
        .map(|(group, resolve)| copy_window(group, resolve, &mut locals, &mut next_local));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let additional_copy = optional
        .additional_copy
        .zip(additional_copy_group.zip(paged_copy_resolver))
        .map(|(body, (group, resolve))| {
            let window = copy_window(group, resolve, &mut locals, &mut next_local);
            let (guard, _) =
                paged_loop_guard.expect("additional copy requires a vector loop guard");
            let attempts = next_local;
            locals.push((1, ValType::I32));
            next_local += 1;
            let rearm = paged_read_rearm.map(|(read, _)| (read, body.order()));
            (body, window, (guard, attempts), rearm)
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dword_read_pair =
        paged_dword_read_pair.map(|(first, second, base, handoff, resolve)| {
            let host = next_local;
            locals.push((2, ValType::I32));
            next_local += 2;
            let handoff = handoff.map(|read| {
                let address = next_local;
                locals.extend([(1, ValType::I64), (1, ValType::I32)]);
                next_local += 2;
                memory::DwordPairHandoff {
                    read,
                    address,
                    ready: address + 1,
                }
            });
            memory::PagedDwordReadPair {
                first,
                second,
                base,
                resolve,
                host,
                resolver_mode: host + 1,
                handoff,
            }
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_indexed_write = paged_indexed_write.map(|(shape, resolve)| {
        let record = next_local;
        locals.push((4, ValType::I32));
        next_local += 4;
        super::paged_indexed_write::GuardedIndexedWrite {
            shape,
            resolve,
            record,
            host: record + 1,
            continuing: record + 2,
            value: record + 3,
        }
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_byte_write_pair = paged_byte_write_pair.map(|(load, store, read, write)| {
        let slot = next_local;
        let host = optional.paged_byte_write_pointer.then_some(slot + 1);
        let count = 1 + u32::from(host.is_some());
        locals.push((count, ValType::I32));
        next_local += count;
        memory::PagedByteWritePair {
            load,
            store,
            read,
            write,
            slot,
            host,
        }
    });
    let boundary_locals = observed.then(|| {
        let result = [next_local, next_local + 1, next_local + 2, next_local + 3];
        locals.push((4, ValType::I32));
        next_local += 4;
        result
    });
    for (bank, local) in vector_registers.iter_mut().enumerate() {
        if vector_banks.used & (1 << bank) != 0 {
            *local = Some(next_local);
            next_local += 1;
        }
    }
    let vector_temporaries = (vector_banks.used != 0 || has_float || has_mmx_vector).then(|| {
        locals.push((vector_banks.used.count_ones() + 3, ValType::V128));
        [next_local, next_local + 1, next_local + 2]
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let float_outline = if let GuardedReads::Outlined { sticky, dyadic, .. } = _guard_reads {
        loop_body
            .zip(proved_reads)
            .and_then(|(body, (proof, _, first_bias, _, _))| {
                let mut outline = float_loop::FloatOutline::new(
                    unit,
                    body,
                    proof,
                    first_bias,
                    vector_registers,
                    run_index + 1 + u32::from(target_lookup),
                    &mut types,
                )?;
                if sticky {
                    outline.enable_sticky();
                }
                if dyadic {
                    let first_local =
                        vector_temporaries.expect("outlined Float temporaries")[2] + 1;
                    outline.dyadic = float_dyadic::DyadicReduction::recognize(
                        proof,
                        body,
                        &unit.instructions,
                        first_local,
                    );
                }
                Some(outline)
            })
    } else {
        None
    };
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if let Some(float_outline) = &float_outline {
        functions.function(float_outline.signature);
        if float_outline.sticky_index.is_some() {
            functions.function(float_outline.signature);
        }
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let copy_access_cache = paged_copy_resolver
        .filter(|_| indexed_paging)
        .map(|resolver| {
            let index = run_index + functions.len();
            functions.function(add_function_type(
                &mut types,
                [ValType::I32, ValType::I64, ValType::I64, ValType::I32],
                [ValType::I32],
            ));
            (index, resolver)
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let outline_locals = float_outline.as_ref().map(|_| locals.clone());
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let outline_rejected = float_outline.as_ref().map(|_| {
        locals.push((1, ValType::I32));
        vector_temporaries.expect("outlined Float temporaries")[2] + 1
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_float_dyadic = loop_body
        .filter(|body| optional.paged_float_dyadic && body.paged_sse())
        .zip(paged_read_span)
        .and_then(|(body, (proof, _, _))| {
            proof.outline_registers()?;
            let first_local = vector_temporaries.expect("paged Float temporaries")[2] + 1;
            let reduction = float_dyadic::DyadicReduction::recognize(
                proof,
                body,
                &unit.instructions,
                first_local,
            )?;
            locals.extend_from_slice(&float_dyadic::DyadicReduction::LOCALS);
            Some(reduction)
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_float_sticky =
        optional.paged_float_sticky && !unit.context.is_long64() && paged_float_dyadic.is_some();
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let mut outlined_functions = Vec::new();
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let quiet_fetch_page = (optional.paged_quiet_fetch
        && optional.paged_quiet_completion
        && paged
        && observed
        && !mmio
        && (unit.sampled || paged_indexed_write.is_some())
        && unit.context.is_long64())
    .then(|| {
        let local = super::GPR + locals.iter().map(|(count, _)| count).sum::<u32>();
        locals.push((1, ValType::I64));
        local
    });
    #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
    let quiet_fetch_page = None;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let indexed_scalar_stamp = (indexed_paging
        && optional.indexed_scalar_read
        && unit.instructions.iter().any(|instruction| {
            super::instruction::indexed_scalar_read(instruction.operation)
                || super::vector::indexed_vector_operand(instruction.operation)
                || indexed_store_fill.is_some()
                    && super::instruction::indexed_scalar_write(instruction.operation, unit.sampled)
        }))
    .then(|| {
        let stamp = super::GPR + locals.iter().map(|(count, _)| count).sum::<u32>();
        locals.push((1, ValType::I64));
        stamp
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let indexed_dword_prefixes = indexed_paging
        && !unit.sampled
        && optional.indexed_dword_prefixes
        && register_prefixes.as_ref().is_some_and(|prefixes| {
            prefixes.iter().enumerate().any(|(start, end)| {
                end.is_some_and(|end| {
                    unit.instructions[start..end]
                        .iter()
                        .copied()
                        .any(super::instruction::paged_prefix_dword_read)
                })
            })
        });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let indexed_paged_read = (indexed_scalar_stamp.is_some()
        || indexed_dword_prefixes
        || indexed_paging && paged_dword_read_pair.is_some_and(|pair| pair.handoff.is_some())
        || indexed_paging && unit.sampled && register_prefix_read.is_some())
    .then(|| {
        let first = super::GPR + locals.iter().map(|(count, _)| count).sum::<u32>();
        locals.push((4, ValType::I32));
        [first, first + 1, first + 2, first + 3]
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let retained_fetch_slot = (retouch_prefix_fetch
        && loop_body.is_none()
        && unit
            .instructions
            .iter()
            .all(|i| i.pc >> 12 == unit.instructions[0].pc >> 12))
    .then(|| {
        let local = super::GPR + locals.iter().map(|(count, _)| count).sum::<u32>();
        locals.push((1, ValType::I32));
        local
    });
    let mut emitter = Emitter {
        paged,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_copy_access_cache: copy_access_cache.map(|(index, _)| index),
        quiet_fetch_page,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        indexed_paged_read,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        indexed_scalar_stamp,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        indexed_store_fill,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        indexed_dword_prefixes,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_quiet_completion: optional.paged_quiet_completion
            && paged
            && observed
            && !mmio
            && (unit.context.is_long64()
                || (unit.context.mode() == crate::x86::context::Mode::Protected
                    && unit.context.code_limit() == u64::from(u32::MAX)
                    && unit.context.nonwrapping_code32_fetch(0, 1) == Some(0))),
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        sampled_region: unit.sampled,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        retouch_prefix_fetch,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        retained_fetch_slot,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        used_paged_quiet_completion: false,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        read_span: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        stack_read_span: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        flat_stack_pointer_span,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        flat_stack_access: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        flat_data_bias: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        stack_pointer_relative: false,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        read_loop_diamond,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        write_spans: [None; 2],
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        linear_call_spans,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        proved_stack: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        indexed_read_window,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        proved_reads: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_proved_f32_reads: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_f32_read_pair,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_f32_prefix_floor,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        proved_read_retry: proved_reads.map(|(_, _, _, _, retry)| retry),
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        zero_upper_xmm: 0,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        normal_low_f32: 0,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        float_outline_params: 0,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        float_raw_sticky: false,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        float_dyadic: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_float_dyadic,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_float_sticky,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_atomic,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_scalar,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_vector_dword,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_vector_span,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        compact_shared_reads,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_dword_window,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_base_byte_window,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_byte_store_span,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_retry: None,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_address_memos,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_native_lock_memo,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_memo_address: false,
        current_index: 0,
        structured_loop: None,
        loop_gates_prechecked: false,
        guarded_paged_lap: false,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_prefix_read: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_prefix_store: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_prefix_fetch_out: None,
        paged_loop_guard,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_preamble,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_preamble_latch,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_read_rearm,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_read_span,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_dense_read_span,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_dword_window,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_dword_window_touch,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_dword_read_pair,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_indexed_write,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_vector_window,
        paged_loop_read_byte,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_byte_write_pair,
        paged_loop_read_dword,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        direct_loop_reads: paged_dword_walk && optional.direct_loop_reads,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_loop_read_block,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_loop_write_block,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_copy_window,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        linear_recurrence,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_write_ready,
        paged_loop_write_byte,
        paged_loop_write_dword,
        register_upper_dwords,
        narrow_gprs,
        omit_result_status: false,
        flags_on_read_fault: None,
        instruction_retirement: control::InstructionRetirement::One,
        entry_lookup: target_lookup.then_some(run_index + 1),
        dense_dispatch: dense_dispatch.is_some(),
        packed_result_status: complete_flat_call,
        multiply_high,
        divide,
        divide_failed,
        stores: false,
        has_repeat,
        restore_string_flags: false,
        context: unit.context.clone(),
        segment: Segment::Ds,
        access_reason,
        mmio_operand,
        target_check,
        vector_registers,
        vector_written: vector_banks.written,
        vector_temporaries,
        vector_load,
        block16_for_write,
        vector_store,
        vector_access_reason,
        float,
        inline_f32_compares: optional.inline_f32_compares,
        inline_f32_sub: optional.inline_f32_sub,
        inline_f32_packed: optional.inline_f32_packed,
        inline_cmpnleps: optional.inline_cmpnleps,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        direct_vector_loads: optional.direct_vector_loads && !observed && !paged && !mmio,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        used_direct_vector_load: false,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        direct_vector_stores: cyclic_vector_store,
        load_mxcsr,
        store_mxcsr,
        x87,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        x87_register_pairs,
        io,
        mmx,
        numeric_image,
        masked_store,
        clflush,
        protection,
        far_control,
        compound_stack,
        software_event,
        system_flags,
        debug_step,
        flat_retire,
        debug_register,
        paging_control,
        invpcid,
        processor,
        protection_key,
        fast_system,
        debug_begin,
        external_step,
        halt,
        observed,
        mutable_controls: has_system_flags || has_software_events || has_fast_system,
        publish_shadow: has_fast_system
            || observed
            || has_system_flags
            || has_software_events
            || unit
                .instructions
                .iter()
                .any(|i| boundary::ss_producer(i.operation)),
        sampled_tf: boundary_locals.map(|l| l[0]),
        incoming_shadow: boundary_locals.map(|l| l[1]),
        next_shadow: boundary_locals.map(|l| l[2]),
        boundary_result: boundary_locals.map(|l| l[3]),
        current_pc: 0,
        current_operation: Operation::Nop,
        body: Function::new(locals),
        branch_hints: Vec::new(),
        entries: (0..unit.instructions.len())
            .map(|placement| {
                (
                    unit.instructions[instruction_index(placement)].pc,
                    placement as u32,
                )
            })
            .collect(),
    };
    emitter.load_state(gpr_banks);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if !paged
        && !observed
        && !mmio
        && unit.context.mode() == crate::x86::context::Mode::Protected
        && unit.context.code32()
    {
        // Full learned regions stay within one activation on their hot path.
        // Avoid adding write cache setup there when most stores are local hits.
        let retain_call_writes =
            linear_call_spans && (!unit.sampled || unit.reached_instruction_limit);
        emitter.load_retained_flat_spans(
            [read_span, stack_read_span, flat_stack_pointer_span],
            [
                write_caches[0].filter(|_| {
                    retain_call_writes
                        || cyclic_vector_store
                        || linear_scalar_copy_prefix.is_some()
                        || linear_vector_copy_prefixes.is_some()
                }),
                write_caches[1].filter(|_| retain_call_writes),
            ],
        );
    }
    emitter.op(Op::Block(BlockType::Empty));
    emitter.gates(0);
    emitter.profile_guard(0);
    let count = unit.instructions.len() as u32;
    // The owner selects the entry ordinal for the loaded RIP. Every in-unit
    // edge commits its destination RIP before routing to the next instruction.
    if dense_dispatch.is_none() {
        emitter
            .op(Op::LocalGet(ENTRY_INDEX))
            .op(Op::LocalSet(ENTRY));
    }
    emitter
        .op(Op::Loop(BlockType::Empty))
        .op(Op::Block(BlockType::Empty));
    for _ in &unit.instructions {
        emitter.op(Op::Block(BlockType::Empty));
    }
    if let Some((base, span)) = dense_dispatch {
        use super::{ADDRESS, RIP};
        // Use the committed guest IP directly, including on dynamic transfers.
        let mut labels = vec![count; span as usize];
        for placement in 0..unit.instructions.len() {
            let instruction = &unit.instructions[instruction_index(placement)];
            labels[(instruction.pc - base) as usize] = placement as u32;
        }
        emitter
            .op(Op::LocalGet(RIP))
            .op(Op::I64Const(base as i64))
            .op(Op::I64Sub)
            .op(Op::LocalTee(ADDRESS))
            .op(Op::I64Const(span as i64))
            .op(Op::I64GeU)
            .op(Op::BrIf(count))
            .op(Op::LocalGet(ADDRESS))
            .op(Op::I32WrapI64)
            .op(Op::BrTable(labels.into(), count));
    } else {
        emitter
            .op(Op::LocalGet(ENTRY))
            .op(Op::BrTable((0..count).collect::<Vec<_>>().into(), count));
    }
    for placement in 0..unit.instructions.len() {
        let index = instruction_index(placement);
        let instruction = &unit.instructions[index];
        emitter.op(Op::End);
        emitter.current_index = placement as u32;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        {
            // Keep one-shot accesses out of the span caches even in an artifact with a hot loop.
            let in_cycle = linear_call_spans
                || span_cycles
                    .iter()
                    .any(|range| range.contains(&instruction.pc));
            emitter.read_span = read_span.filter(|_| in_cycle);
            emitter.stack_read_span = stack_read_span.filter(|_| in_cycle);
            emitter.stack_pointer_relative = false;
            emitter.write_spans = if in_cycle { write_caches } else { [None; 2] };
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        {
            emitter.parallel_memo_address = false;
        }
        let loop_depth = count - placement as u32;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(end) = flat_integer_groups
            .as_ref()
            .and_then(|groups| groups[index])
        {
            let stack_window = flat_stack_windows
                .as_ref()
                .and_then(|windows| windows[index].as_ref())
                .zip(flat_stack_host);
            let data_window = flat_data_windows
                .as_ref()
                .and_then(|windows| windows[index].as_ref())
                .zip(flat_data_bias);
            emitter.emit_flat_integer_group(
                &unit.instructions[index..end],
                loop_depth,
                stack_window,
                data_window,
            );
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let additional =
            additional_copy.filter(|(body, _, _, _)| body.order().position(index as u32).is_some());
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        {
            // Keep the replay state of the two disjoint loops independent.
            emitter.paged_loop_guard =
                additional.map_or(paged_loop_guard, |(_, _, guard, _)| Some(guard));
            emitter.paged_read_rearm =
                additional.map_or(paged_read_rearm, |(_, _, _, rearm)| rearm);
            emitter.paged_copy_window =
                additional.map_or(paged_copy_window, |(_, window, _, _)| Some(window));
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some((head, group)) = copy_prefix
            .filter(|(head, _)| *head == index)
            .or_else(|| preloaded_copy_prefix.filter(|(head, _)| *head == index))
        {
            if let Some((tail, read, locals)) = preloaded_loop
                && preloaded_copy_prefix.is_some_and(|(start, _)| start == head)
            {
                emitter.emit_preloaded_copy_loop(
                    &unit.instructions[head..head + 7],
                    group,
                    tail,
                    loop_depth,
                    [
                        copy_prefix_guard.expect("copy prefix guard"),
                        paged_copy_resolver.expect("copy prefix operand resolver"),
                        read,
                    ],
                    locals,
                );
            }
            emitter.emit_finite_copy_prefix(
                &unit.instructions[head..head + group.prefix_guard_count()],
                group,
                loop_depth,
                copy_prefix_guard.expect("copy prefix guard"),
                paged_copy_resolver.expect("copy prefix operand resolver"),
            );
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some((head, group)) = scalar_copy_prefix.filter(|(head, _)| *head == index) {
            let guard = copy_prefix_guard.expect("copy prefix guard");
            let resolve = paged_copy_resolver.expect("copy prefix operand resolver");
            if let Some(target) = group.following_jump(unit, head)
                && emitter.entries.contains_key(&target)
            {
                emitter.emit_finite_scalar_copy(
                    &unit.instructions[head..head + group.instructions + 1],
                    group,
                    loop_depth,
                    guard,
                    resolve,
                    Some(target),
                );
            }
            emitter.emit_finite_scalar_copy(
                &unit.instructions[head..head + group.instructions],
                group,
                loop_depth,
                guard,
                resolve,
                None,
            );
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(((head, group), (read, write))) =
            linear_scalar_copy_prefix.filter(|((head, _), _)| *head == index)
        {
            emitter.emit_linear_scalar_copy(
                &unit.instructions[head..head + group.instructions],
                group,
                loop_depth,
                (read, write),
            );
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some(((head, group), spans)) =
            linear_vector_copy_prefixes
                .as_ref()
                .and_then(|(prefixes, spans)| {
                    prefixes
                        .iter()
                        .copied()
                        .find(|(head, _)| *head == index)
                        .map(|group| (group, *spans))
                })
        {
            emitter.emit_linear_vector_copy(
                &unit.instructions[head..head + group.instruction_count() as usize],
                group,
                loop_depth,
                spans,
                flat_copy_start_remaining,
                flat_copy_window,
            );
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some((end, guard)) = register_prefixes
            .as_ref()
            .and_then(|p| p[index])
            .zip(register_prefix_guard)
        {
            emitter.emit_guarded_register_prefix(
                &unit.instructions[index..end],
                loop_depth,
                guard,
                register_prefix_read,
                write_dword.filter(|_| prefix_write_dword),
                prefix_fetch_handoff.as_ref().map(|(local, edges)| {
                    (*local, edges[index].receives, edges[index].destination)
                }),
            );
        }

        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if let Some((preamble, handoff, inherited)) =
            paged_preamble.filter(|(preamble, _, _)| preamble.body.head == index as u32)
        {
            emitter.emit_guarded_preamble(
                &unit.instructions,
                preamble.body,
                loop_depth,
                (handoff, inherited),
                paged_preamble_stack_guard,
            );
        }
        let head = loop_body.filter(|body| body.head == index as u32);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let head = head.or(additional
            .map(|(body, _, _, _)| body)
            .filter(|body| body.head == index as u32));
        let canonical_checked = |body: &StructuredLoop| {
            let natural = body.natural_forward_loop();
            #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
            let natural = natural
                || (paged_dense_read_span.is_some()
                    && body.straight_paged_reads()
                    && (!unit.context.is_long64() || body.paged_source_is_4k(unit)));
            natural
        };
        if let Some((body, guard)) = head.filter(canonical_checked).zip(emitter.paged_loop_guard) {
            // The canonical entries cover the checked fallback. Dense straight
            // reads reach the same head on a checked backedge through dispatch.
            emitter.emit_guarded_paged_loop(&unit.instructions, body, loop_depth, guard);
        } else if let Some(loop_body) = head {
            if !observed && loop_body.precheck_gates_without_read_proof() {
                // Partial laps and in-loop stops use the original entry labels.
                emitter.exclude_loop_stop(&unit.instructions, loop_body);
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if let Some(guard) = proved_stack {
                    emitter.emit_stack_loops(
                        &unit.instructions,
                        loop_body,
                        loop_depth,
                        write_segment_base,
                        guard,
                    );
                } else if let Some(guard) = proved_integer_reads {
                    emitter.emit_integer_read_loops(
                        &unit.instructions,
                        loop_body,
                        loop_depth,
                        write_segment_base,
                        guard,
                    );
                } else {
                    emitter.emit_prechecked_loop(
                        &unit.instructions,
                        loop_body,
                        loop_depth,
                        write_segment_base,
                    );
                }
                #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
                emitter.emit_prechecked_loop(
                    &unit.instructions,
                    loop_body,
                    loop_depth,
                    write_segment_base,
                );
            } else {
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if let Some((proof, span, first_bias, guard_failed, (_, retry))) = proved_reads {
                    emitter.guard_read_loop(proof, span, first_bias, guard_failed, retry);
                    emitter.exclude_loop_stop(&unit.instructions, loop_body);
                    emitter.op(Op::I32And);
                    emitter.proved_reads = Some((proof, first_bias));
                    if let Some(outline) = &float_outline {
                        let rejected = outline_rejected.expect("outline rejection local");
                        let locals = outline_locals.as_deref().expect("outline locals");
                        outlined_functions.push(emitter.emit_float_outline(
                            outline,
                            locals,
                            &unit.instructions,
                            loop_body,
                            false,
                        ));
                        if outline.sticky_index.is_some() {
                            outlined_functions.push(emitter.emit_float_outline(
                                outline,
                                locals,
                                &unit.instructions,
                                loop_body,
                                true,
                            ));
                        }
                        emitter.op(Op::If(BlockType::Empty));
                        emitter.multiple_read_laps(proof);
                        // Keep the proved inline placement for later visits after
                        // the same generated activation needed numeric replay.
                        emitter
                            .op(Op::LocalGet(rejected))
                            .op(Op::I32Eqz)
                            .op(Op::I32And)
                            .op(Op::If(BlockType::Empty));
                        emitter.call_float_outline(
                            outline,
                            loop_body,
                            unit.instructions[loop_body.tail as usize],
                            loop_depth + 2,
                            retry,
                            rejected,
                        );
                        emitter.op(Op::Else);
                        emitter.emit_prechecked_body(
                            &unit.instructions,
                            loop_body,
                            loop_depth + 2,
                            write_segment_base,
                        );
                        emitter.op(Op::End).op(Op::End);
                    } else {
                        emitter.emit_prechecked_loop(
                            &unit.instructions,
                            loop_body,
                            loop_depth,
                            write_segment_base,
                        );
                    }
                    emitter.proved_reads = None;
                }
                emitter.emit_structured_loop(
                    &unit.instructions,
                    loop_body,
                    loop_depth,
                    write_segment_base,
                );
                continue;
            }
        }
        emitter.instruction(
            *instruction,
            unit.instructions.get(index + 1),
            loop_depth,
            write_segment_base,
            None,
        );
    }
    emitter.op(Op::End);
    emitter.exit(DISPATCH, 1);
    emitter.op(Op::End).op(Op::End);
    emitter.publish_state(gpr_banks);
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    emitter.leave_parallel_atomic_if_held();
    emitter.op(Op::LocalGet(REASON)).op(Op::End);
    let mut code = CodeSection::new();
    code.function(&emitter.body);
    if target_lookup {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let dense = linear_call_spans;
        #[cfg(not(all(not(feature = "parallel"), target_arch = "wasm32")))]
        let dense = false;
        code.function(&entry_lookup::emit(&emitter.entries, dense));
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    for (_, body, _) in &outlined_functions {
        code.function(body);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if let Some((_, resolver)) = copy_access_cache {
        code.function(&super::paged_copy::emit_copy_access_cache(resolver));
    }
    let mut module = Module::new();
    module
        .section(&types)
        .section(&imports)
        .section(&functions)
        .section(&exports);
    let mut bytes = module.finish();
    let hints_start = bytes.len();
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let outline_has_hints = outlined_functions
        .iter()
        .any(|(_, _, hints)| !hints.is_empty());
    #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
    let outline_has_hints = false;
    if !emitter.branch_hints.is_empty() || outline_has_hints {
        let mut hints = wasm_encoder::BranchHints::new();
        if !emitter.branch_hints.is_empty() {
            hints.function_hints(run_index, emitter.branch_hints);
        }
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        for (function, _, branches) in outlined_functions {
            if !branches.is_empty() {
                hints.function_hints(function, branches);
            }
        }
        hints.append_to(&mut bytes);
    }
    let code_start = bytes.len();
    code.append_to(&mut bytes);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && emitter.direct_loop_reads {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                direct_loop_reads: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && flat_integer_groups.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                flat_integer_groups: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && indexed_dword_prefixes {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                indexed_dword_prefixes: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && indexed_scalar_stamp.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                indexed_scalar_read: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && (quiet_fetch_page.is_some() || emitter.used_paged_quiet_completion)
    {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_quiet_completion: quiet_fetch_page.is_some(),
                paged_quiet_fetch: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_indexed_write.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_indexed_write: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && (scalar_copy_prefix.is_some()
            || linear_scalar_copy_prefix.is_some()
            || linear_vector_copy_prefixes.is_some())
    {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                scalar_copy_prefix: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && (copy_prefix.is_some() || preloaded_copy_prefix.is_some())
    {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                copy_prefix: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && additional_copy.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                additional_copy: None,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && parallel_native_lock_memo.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                parallel_native_lock_memo: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_vector_retouch.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_vector_retouch: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && paged_vector_window.is_some_and(|window| window.last_early_page.is_some())
    {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_vector_rearm: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_vector_window.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_vector_window: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && linear_recurrence.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                linear_recurrence: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && register_prefix_guard.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_register_prefixes: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && x87_register_pairs {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                x87_register_pairs: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_dword_read_pair.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_dword_read_pair: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_byte_write_pair.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_byte_write_pair: optional.paged_byte_write_pointer,
                paged_byte_write_pointer: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && paged_read_span.is_some_and(|(proof, _, _)| proof.is_countdown())
    {
        return (bytes, false);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_float_sticky {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_float_sticky: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && paged_f32_read_pair.is_some_and(|(_, _, split)| split.is_some())
    {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_f32_split_pair: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_preamble.is_some() && paged_preamble_latch {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_preamble_latch: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_preamble.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_preamble: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_split_vectors {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_split_vectors: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    if bytes.len() > max_artifact_bytes && paged_sse_write {
        return (bytes, false);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && cyclic_vector_store {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                direct_vector_stores: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    if bytes.len() > max_artifact_bytes
        && optional.inline_cmpnleps
        && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Float(float) if super::float::is_cmpnleps(float))
        })
    {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                inline_cmpnleps: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged && read_loop_diamond.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_read_diamond: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_read_rearm.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_read_rearm: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_f32_read_pair.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_f32_read_pair: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_dense_read_span.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_dense_read_span: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && paged_float_dyadic.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                paged_float_dyadic: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(not(feature = "parallel"))]
    if bytes.len() > max_artifact_bytes && loop_body.is_some_and(StructuredLoop::paged_sse) {
        return (bytes, false);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && emitter.used_direct_vector_load {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                direct_vector_loads: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    if bytes.len() > max_artifact_bytes
        && optional.inline_f32_packed
        && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Float(float) if super::float::is_packed_f32_arithmetic(float))
        })
    {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                inline_f32_packed: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    if bytes.len() > max_artifact_bytes
        && optional.inline_f32_sub
        && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Float(float) if super::float::is_scalar_f32_sub(float))
        })
    {
        // Drop the new arithmetic path before reducing the compare or loop placement.
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                inline_f32_sub: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    if bytes.len() > max_artifact_bytes
        && optional.inline_f32_compares
        && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Float(float) if super::float::is_f32_flag_compare(float))
        })
    {
        // Keep the original placement if the direct compare exceeds its cap.
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                inline_f32_compares: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && loop_body.is_some_and(StructuredLoop::shared_memory_prechecked)
    {
        // Retry the original placement before trading a memory window for the clone.
        return (bytes, false);
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && let GuardedReads::Shared {
            indexed, compact, ..
        } = _guard_reads
    {
        let wide = unit.context.is_long64();
        let has_indexed_bytes = allow_indexed_bytes
            && parallel_dword_window.is_some()
            && unit.instructions.iter().any(|i| {
                parallel::DwordWindow::eligible(i.operation, true, wide)
                    && !parallel::DwordWindow::eligible(i.operation, false, wide)
            });
        let retry = if parallel_byte_store_span.is_some() {
            // Preserve the existing read placement before retrying smaller layouts.
            Some((indexed, compact))
        } else if parallel_base_byte_window.is_some() {
            // Retain the existing indexed read paths when only the second window exceeds the cap.
            Some((SharedIndexedReads::IncludeBytes, compact))
        } else if wide && parallel_dword_window.is_some() {
            // Retry with the dword window before compacting the original Long64 placement.
            let indexed = if has_indexed_bytes {
                SharedIndexedReads::DwordsOnly
            } else {
                SharedIndexedReads::Disabled
            };
            Some((indexed, compact))
        } else if !compact && (parallel_vector_span.is_some() || has_indexed_bytes) {
            // Keep the existing dword and FP32 paths while compacting.
            Some((indexed, true))
        } else if parallel_dword_window.is_some() {
            Some((SharedIndexedReads::Disabled, compact))
        } else {
            None
        };
        if let Some((indexed, compact)) = retry {
            return emit_body(
                unit,
                observed,
                (paged, mmio),
                loop_body,
                GuardedReads::Shared {
                    indexed,
                    compact,
                    byte_stores: false,
                },
                optional,
                max_artifact_bytes,
            );
        }
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && parallel_vector_span.is_some() {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            _guard_reads,
            OptionalLowering {
                extra_spans: false,
                ..optional
            },
            max_artifact_bytes,
        );
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && parallel_vector_dword.is_some()
        && dword_writes == 0
        && !loop_body.is_some_and(StructuredLoop::is_optional_shared_loop)
    {
        // Retry ordinary placement before trading shared dword reads for a loop clone.
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            GuardedReads::Checked,
            optional,
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && read_loop_diamond.is_some()
        && let GuardedReads::Outlined {
            sticky,
            dyadic,
            indexed_read,
            ..
        } = _guard_reads
    {
        // Keep the original loop placement when only the merged latch is too large.
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            GuardedReads::Outlined {
                sticky,
                dyadic,
                diamond: false,
                indexed_read,
            },
            optional,
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && indexed_read_window.is_some()
        && let GuardedReads::Outlined {
            sticky,
            dyadic,
            diamond,
            ..
        } = _guard_reads
    {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            GuardedReads::Outlined {
                sticky,
                dyadic,
                diamond,
                indexed_read: false,
            },
            optional,
            max_artifact_bytes,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes
        && let Some(outline) = &float_outline
        && let GuardedReads::Outlined { sticky, .. } = _guard_reads
        && (outline.dyadic.is_some() || sticky)
    {
        // Drop packed reduction before displacing an existing outline or its hints.
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            GuardedReads::Outlined {
                sticky: sticky && outline.dyadic.is_some(),
                dyadic: false,
                diamond: false,
                indexed_read: false,
            },
            optional,
            max_artifact_bytes,
        );
    }
    if bytes.len() > max_artifact_bytes {
        // Optional annotations never displace executable code under the owner's limit.
        bytes.drain(hints_start..code_start);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if float_outline.is_some() && bytes.len() > max_artifact_bytes {
        return emit_body(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            GuardedReads::Inline,
            optional,
            max_artifact_bytes,
        );
    }
    #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
    let extra_spans = false;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let extra_spans = write_span.is_some() || stack_read_span.is_some();
    (bytes, extra_spans)
}
