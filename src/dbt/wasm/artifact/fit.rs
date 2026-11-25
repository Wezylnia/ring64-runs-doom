use super::{
    GuardedReads, OptionalLowering, StructuredLoop, Unit, binary::EncodedArtifact, emit_candidate,
};
#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
use super::{SharedIndexedReads, parallel};
use crate::dbt::wasm::numeric::float;
use crate::x86::instruction::Operation;

pub(super) struct Candidate {
    pub(super) artifact: EncodedArtifact,
    pub(super) extra_spans: bool,
    pub(super) oversize: Option<OversizeCandidate>,
}

// Fallback needs facts about what the candidate emitted, in addition to its requested options.
pub(super) struct OversizeCandidate {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) retouch_prefix_fetch: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) direct_loop_reads: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) flat_integer_groups: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) indexed_dword_prefixes: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) indexed_scalar_read: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_quiet_fetch: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_quiet_completion: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_indexed_write: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) scalar_copy_prefix: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) copy_prefix: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) additional_copy: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) parallel_native_lock_memo: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_vector_retouch: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_vector_rearm: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_vector_window: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) linear_recurrence: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_register_prefixes: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) x87_register_pairs: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_dword_read_pair: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_byte_write_pair: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_countdown_read_span: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_float_sticky: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_f32_split_pair: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_preamble_latch: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_preamble: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_split_vectors: bool,
    pub(super) paged_sse_write: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) cyclic_vector_store: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) read_loop_diamond: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_read_rearm: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_f32_read_pair: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_dense_read_span: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) paged_float_dyadic: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) direct_vector_loads: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) allow_indexed_bytes: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) parallel_dword_window: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) parallel_byte_store_span: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) parallel_base_byte_window: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) parallel_vector_span: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) parallel_vector_dword: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    pub(super) no_dword_writes: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) indexed_read_window: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) float_outline_dyadic: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    pub(super) float_outline: bool,
}

enum Fallback {
    Retry(GuardedReads, OptionalLowering),
    TryAnotherPlacement,
    DiscardHints,
}

pub(super) fn emit_body(
    unit: &Unit,
    observed: bool,
    (paged, mmio): (bool, bool),
    loop_body: Option<StructuredLoop>,
    mut guard_reads: GuardedReads,
    mut optional: OptionalLowering,
    max_artifact_bytes: usize,
) -> (Vec<u8>, bool) {
    loop {
        let Candidate {
            mut artifact,
            extra_spans,
            oversize,
        } = emit_candidate(
            unit,
            observed,
            (paged, mmio),
            loop_body,
            guard_reads,
            optional,
            max_artifact_bytes,
        );
        let Some(facts) = oversize else {
            return (artifact.bytes, extra_spans);
        };
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if facts.retouch_prefix_fetch && artifact.code_fits(max_artifact_bytes) {
            // Preserve the ordinary call region when only its miss hints exceed the limit.
            artifact.discard_hints();
            return (artifact.bytes, extra_spans);
        }
        match fallback(unit, paged, loop_body, guard_reads, optional, &facts) {
            Fallback::Retry(reads, lowering) => {
                guard_reads = reads;
                optional = lowering;
            }
            Fallback::TryAnotherPlacement => return (artifact.bytes, false),
            Fallback::DiscardHints => {
                // Optional annotations never displace executable code under the owner's limit.
                artifact.discard_hints();
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if facts.float_outline && artifact.bytes.len() > max_artifact_bytes {
                    guard_reads = GuardedReads::Inline;
                    continue;
                }
                return (artifact.bytes, extra_spans);
            }
        }
    }
}

fn fallback(
    unit: &Unit,
    _paged: bool,
    _loop_body: Option<StructuredLoop>,
    guard_reads: GuardedReads,
    mut optional: OptionalLowering,
    facts: &OversizeCandidate,
) -> Fallback {
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.direct_loop_reads {
        optional.direct_loop_reads = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.flat_integer_groups {
        optional.flat_integer_groups = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.indexed_dword_prefixes {
        optional.indexed_dword_prefixes = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.indexed_scalar_read {
        optional.indexed_scalar_read = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_quiet_fetch || facts.paged_quiet_completion {
        optional.paged_quiet_completion = facts.paged_quiet_fetch;
        optional.paged_quiet_fetch = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_indexed_write {
        optional.paged_indexed_write = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.scalar_copy_prefix {
        optional.scalar_copy_prefix = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.copy_prefix {
        optional.copy_prefix = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.additional_copy {
        optional.additional_copy = None;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if facts.parallel_native_lock_memo {
        optional.parallel_native_lock_memo = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_vector_retouch {
        optional.paged_vector_retouch = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_vector_rearm {
        optional.paged_vector_rearm = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_vector_window {
        optional.paged_vector_window = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.linear_recurrence {
        optional.linear_recurrence = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_register_prefixes {
        optional.paged_register_prefixes = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.x87_register_pairs {
        optional.x87_register_pairs = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_dword_read_pair {
        optional.paged_dword_read_pair = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_byte_write_pair {
        optional.paged_byte_write_pair = optional.paged_byte_write_pointer;
        optional.paged_byte_write_pointer = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_countdown_read_span {
        return Fallback::TryAnotherPlacement;
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_float_sticky {
        optional.paged_float_sticky = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_f32_split_pair {
        optional.paged_f32_split_pair = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_preamble && facts.paged_preamble_latch {
        optional.paged_preamble_latch = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_preamble {
        optional.paged_preamble = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_split_vectors {
        optional.paged_split_vectors = false;
        return Fallback::Retry(guard_reads, optional);
    }
    if facts.paged_sse_write {
        return Fallback::TryAnotherPlacement;
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.cyclic_vector_store {
        optional.direct_vector_stores = false;
        return Fallback::Retry(guard_reads, optional);
    }
    if optional.inline_cmpnleps
        && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Float(float) if float::is_cmpnleps(float))
        })
    {
        optional.inline_cmpnleps = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if _paged && facts.read_loop_diamond {
        optional.paged_read_diamond = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_read_rearm {
        optional.paged_read_rearm = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_f32_read_pair {
        optional.paged_f32_read_pair = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_dense_read_span {
        optional.paged_dense_read_span = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.paged_float_dyadic {
        optional.paged_float_dyadic = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(not(feature = "parallel"))]
    if _loop_body.is_some_and(StructuredLoop::paged_sse) {
        return Fallback::TryAnotherPlacement;
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.direct_vector_loads {
        optional.direct_vector_loads = false;
        return Fallback::Retry(guard_reads, optional);
    }
    if optional.inline_f32_packed
        && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Float(float) if float::is_packed_f32_arithmetic(float))
        })
    {
        optional.inline_f32_packed = false;
        return Fallback::Retry(guard_reads, optional);
    }
    if optional.inline_f32_sub
        && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Float(float) if float::is_scalar_f32_sub(float))
        })
    {
        // Drop the new arithmetic path before reducing the compare or loop placement.
        optional.inline_f32_sub = false;
        return Fallback::Retry(guard_reads, optional);
    }
    if optional.inline_f32_compares
        && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Float(float) if float::is_f32_flag_compare(float))
        })
    {
        // Keep the original placement if the direct compare exceeds its cap.
        optional.inline_f32_compares = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if _loop_body.is_some_and(StructuredLoop::shared_memory_prechecked) {
        // Retry the original placement before trading a memory window for the clone.
        return Fallback::TryAnotherPlacement;
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if let GuardedReads::Shared {
        indexed, compact, ..
    } = guard_reads
    {
        let wide = unit.context.is_long64();
        let has_indexed_bytes = facts.allow_indexed_bytes
            && facts.parallel_dword_window
            && unit.instructions.iter().any(|i| {
                parallel::DwordWindow::eligible(i.operation, true, wide)
                    && !parallel::DwordWindow::eligible(i.operation, false, wide)
            });
        let retry = if facts.parallel_byte_store_span {
            // Preserve the existing read placement before retrying smaller layouts.
            Some((indexed, compact))
        } else if facts.parallel_base_byte_window {
            // Retain the existing indexed read paths when only the second window exceeds the cap.
            Some((SharedIndexedReads::IncludeBytes, compact))
        } else if wide && facts.parallel_dword_window {
            // Retry with the dword window before compacting the original Long64 placement.
            let indexed = if has_indexed_bytes {
                SharedIndexedReads::DwordsOnly
            } else {
                SharedIndexedReads::Disabled
            };
            Some((indexed, compact))
        } else if !compact && (facts.parallel_vector_span || has_indexed_bytes) {
            // Keep the existing dword and FP32 paths while compacting.
            Some((indexed, true))
        } else if facts.parallel_dword_window {
            Some((SharedIndexedReads::Disabled, compact))
        } else {
            None
        };
        if let Some((indexed, compact)) = retry {
            return Fallback::Retry(
                GuardedReads::Shared {
                    indexed,
                    compact,
                    byte_stores: false,
                },
                optional,
            );
        }
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if facts.parallel_vector_span {
        optional.extra_spans = false;
        return Fallback::Retry(guard_reads, optional);
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if facts.parallel_vector_dword
        && facts.no_dword_writes
        && !_loop_body.is_some_and(StructuredLoop::is_optional_shared_loop)
    {
        // Retry ordinary placement before trading shared dword reads for a loop clone.
        return Fallback::Retry(GuardedReads::Checked, optional);
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.read_loop_diamond
        && let GuardedReads::Outlined {
            sticky,
            dyadic,
            indexed_read,
            ..
        } = guard_reads
    {
        // Keep the original loop placement when only the merged latch is too large.
        return Fallback::Retry(
            GuardedReads::Outlined {
                sticky,
                dyadic,
                diamond: false,
                indexed_read,
            },
            optional,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.indexed_read_window
        && let GuardedReads::Outlined {
            sticky,
            dyadic,
            diamond,
            ..
        } = guard_reads
    {
        return Fallback::Retry(
            GuardedReads::Outlined {
                sticky,
                dyadic,
                diamond,
                indexed_read: false,
            },
            optional,
        );
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    if facts.float_outline
        && let GuardedReads::Outlined { sticky, .. } = guard_reads
        && (facts.float_outline_dyadic || sticky)
    {
        // Drop packed reduction before displacing an existing outline or its hints.
        return Fallback::Retry(
            GuardedReads::Outlined {
                sticky: sticky && facts.float_outline_dyadic,
                dyadic: false,
                diamond: false,
                indexed_read: false,
            },
            optional,
        );
    }
    Fallback::DiscardHints
}
