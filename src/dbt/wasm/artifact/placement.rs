use super::{
    GuardedReads, OptionalLowering, SharedIndexedReads, StructuredLoop, Unit, fit::emit_body,
};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use crate::dbt::wasm::execution::paged::copy::{CopyGroup, ScalarCopyGroup};
use crate::x86::context::Segment;
use crate::x86::instruction::Operation;

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
