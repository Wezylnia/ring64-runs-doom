#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
use super::parallel;
use super::{
    ENTRY, ENTRY_INDEX, Emitter, REASON, StructuredLoop, Unit, boundary, control, entry_lookup,
    mmx, state, vector,
};
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
use super::{float_dyadic, float_loop, loop_cfg, memory, read_loop, stack_slots};
use crate::embedding::layout::DISPATCH;
use crate::x86::context::Segment;
use crate::x86::instruction::{Location, MemoryAluOperands, Operation, Repeat};
use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction as Op, MemoryType, Module, Section, TypeSection, ValType,
};

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
    if paged && let Some(body) = StructuredLoop::select_paged_registers(&unit.instructions) {
        if let Some(guarded) = body.within_paged_fetch(unit) {
            let bytes = emit_placement(
                unit,
                observed,
                (paged, false),
                Some(guarded),
                false,
                max_artifact_bytes,
            );
            if bytes.len() <= max_artifact_bytes {
                return bytes;
            }
        }
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
    #[cfg(not(feature = "parallel"))]
    if paged && let Some(body) = StructuredLoop::select_paged_reads(unit) {
        #[cfg(target_arch = "wasm32")]
        if let Some(window) = body.with_paged_dword_window(unit) {
            let bytes = emit_placement(
                unit,
                observed,
                (paged, false),
                Some(window),
                false,
                max_artifact_bytes,
            );
            if bytes.len() <= max_artifact_bytes {
                return bytes;
            }
        }
        #[cfg(target_arch = "wasm32")]
        if let Some(span) = body.with_paged_read_span(unit) {
            let bytes = emit_placement(
                unit,
                observed,
                (paged, false),
                Some(span),
                false,
                max_artifact_bytes,
            );
            if bytes.len() <= max_artifact_bytes {
                return bytes;
            }
        }
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
    #[cfg(not(feature = "parallel"))]
    if paged && let Some(body) = StructuredLoop::select_paged_scalar_f32(unit) {
        #[cfg(target_arch = "wasm32")]
        if let Some(span) = body.with_paged_f32_read_span(unit) {
            let bytes = emit_placement(
                unit,
                observed,
                (paged, false),
                Some(span),
                false,
                max_artifact_bytes,
            );
            if bytes.len() <= max_artifact_bytes {
                return bytes;
            }
        }
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

fn emit_placement(
    unit: &Unit,
    observed: bool,
    (paged, mmio): (bool, bool),
    loop_body: Option<StructuredLoop>,
    guard_reads: bool,
    max_artifact_bytes: usize,
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
        extra_spans: true,
        inline_f32_compares: true,
        inline_f32_sub: true,
        inline_f32_packed: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        direct_vector_loads: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_float_dyadic: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_f32_read_pair: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_dense_read_span: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_read_rearm: true,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_read_diamond: true,
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
    extra_spans: bool,
    inline_f32_compares: bool,
    inline_f32_sub: bool,
    inline_f32_packed: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    direct_vector_loads: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_float_dyadic: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_f32_read_pair: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_dense_read_span: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_read_rearm: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_read_diamond: bool,
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
    let paged_loop_guard_type = loop_body.and_then(|body| body.paged_fetch_bytes).map(|_| {
        add_function_type(
            &mut types,
            [ValType::I64, ValType::I32, ValType::I64, ValType::I32],
            [ValType::I32],
        )
    });
    #[cfg(not(feature = "parallel"))]
    let [paged_read_byte, paged_read_dword] =
        loop_body.map_or([false; 2], |body| body.paged_read_widths(unit));
    #[cfg(feature = "parallel")]
    let [paged_read_byte, paged_read_dword] = [false; 2];
    let paged_loop_read_type = (paged_read_byte || paged_read_dword)
        .then(|| add_function_type(&mut types, [ValType::I64], [ValType::I64]));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dword_window_type = loop_body
        .is_some_and(|body| body.paged_dword_window)
        .then(|| add_function_type(&mut types, [ValType::I64], [ValType::I32]));
    #[cfg(not(feature = "parallel"))]
    let paged_write_byte = loop_body.is_some_and(|body| body.has_paged_byte_store(unit));
    #[cfg(feature = "parallel")]
    let paged_write_byte = false;
    let paged_write_dword = loop_body.is_some_and(|body| body.paged_stack_writes);
    let paged_loop_write_type = (paged_write_byte || paged_write_dword)
        .then(|| add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I32]));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_read_rearm_type = loop_body
        .filter(|body| {
            optional.paged_read_rearm
                && !mmio
                && paged_read_dword
                && !paged_write_byte
                && !paged_write_dword
                && body.paged_fetch_bytes.is_some()
                && body.forward_edges()
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
    let paged_loop_guard = paged_loop_guard_type.map(|index| {
        #[cfg(not(feature = "parallel"))]
        if loop_body.is_some_and(StructuredLoop::paged_sse) {
            return import_function("paged_sse_loop_guard", index);
        }
        let name = if paged_write_dword {
            "paged_write_loop_guard32"
        } else if !unit.context.is_long64() && (paged_read_byte || paged_read_dword) {
            "paged_read_loop_guard32"
        } else {
            "paged_loop_guard"
        };
        import_function(name, index)
    });
    let paged_loop_read_byte = paged_loop_read_type
        .filter(|_| paged_read_byte)
        .map(|index| import_function("paged_loop_read_byte", index));
    let paged_loop_read_dword = paged_loop_read_type
        .filter(|_| paged_read_dword)
        .map(|index| import_function("paged_loop_read_dword", index));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_read_rearm = paged_read_rearm_type
        .map(|(index, order)| (import_function("paged_loop_read_ready", index), order));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let paged_dword_window =
        paged_dword_window_type.map(|index| import_function("paged_loop_read_dword_window", index));
    let paged_loop_write_byte = paged_loop_write_type
        .filter(|_| paged_write_byte)
        .map(|index| import_function("paged_loop_write_byte", index));
    let paged_loop_write_dword = paged_loop_write_type
        .filter(|_| paged_write_dword)
        .map(|index| import_function("paged_loop_write_dword", index));
    let debug_begin = debug_begin_type.map(|index| import_function("debug_begin", index));
    let debug_step = debug_type.map(|index| import_function("debug_finish", index));
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
    let paged_dense_read_span = loop_body
        .filter(|_| optional.paged_dense_read_span)
        .zip(paged_read_span)
        .filter(|(body, (proof, _))| {
            proof.dense_ascending_bytes(&unit.instructions[body.head as usize..=body.tail as usize])
        })
        .map(|_| span_import("paged_loop_read_span_pages"));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let read_span = (!observed && !span_cycles.is_empty()).then(|| span_import("read_span"));
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let stack_read_span = read_span.filter(|_| {
        _extra_spans && unit.instructions.iter().any(|instruction| {
            matches!(instruction.operation, Operation::Load { address, .. } if address.segment == Segment::Ss)
                && span_cycles.iter().any(|range| range.contains(&instruction.pc))
        })
    });
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let write_segments = {
        let mut segments = [false; 2];
        if !observed && _extra_spans {
            for instruction in &unit.instructions {
                if let Operation::Store { address, .. }
                | Operation::NonTemporalStore { address, .. } = instruction.operation
                    && span_cycles
                        .iter()
                        .any(|range| range.contains(&instruction.pc))
                {
                    // Stack spills often alternate with writes to a separate backing.
                    segments[usize::from(address.segment == Segment::Ss)] = true;
                }
            }
        }
        segments
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
            let last_stream = proof
                .last_memory_stream(&unit.instructions[body.head as usize..=body.tail as usize])?;
            let signature = add_function_type(
                &mut types,
                [ValType::I64, ValType::I32, ValType::I64, ValType::I32],
                [ValType::I64],
            );
            Some((
                import_function("paged_loop_read_pair", signature),
                last_stream,
            ))
        });
    let mut functions = FunctionSection::new();
    functions.function(3);
    if dynamic_targets {
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
    // The admitted helpers leave the high dwords in the backing RunState.
    let narrow_gprs = !observed && !cfg!(feature = "parallel") && state::narrow_gprs(unit);
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
    let stack_read_span = stack_read_span.map(&mut span_cache);
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let write_caches = write_segments.map(|used| write_span.filter(|_| used).map(&mut span_cache));
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
    let paged_dword_window = paged_dword_window.map(|resolve| {
        let first = next_local;
        locals.extend([(2, ValType::I64), (1, ValType::I32)]);
        next_local += 3;
        memory::PagedDwordWindow {
            resolve,
            base: first,
            limit: first + 1,
            host: first + 2,
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
                    run_index + 1 + u32::from(dynamic_targets),
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
    let mut outlined_functions = Vec::new();
    let mut emitter = Emitter {
        paged,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        read_span: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        stack_read_span: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        read_loop_diamond,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        write_spans: [None; 2],
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
        parallel_memo_address: false,
        current_index: 0,
        structured_loop: None,
        loop_gates_prechecked: false,
        guarded_paged_lap: false,
        paged_loop_guard,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_read_rearm,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_read_span,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_dense_read_span,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        paged_dword_window,
        paged_loop_read_byte,
        paged_loop_read_dword,
        paged_loop_write_byte,
        paged_loop_write_dword,
        register_upper_dwords,
        narrow_gprs,
        omit_result_status: false,
        flags_on_read_fault: None,
        instruction_retirement: control::InstructionRetirement::One,
        entry_lookup: dynamic_targets.then_some(run_index + 1),
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        direct_vector_loads: optional.direct_vector_loads && !observed && !paged && !mmio,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        used_direct_vector_load: false,
        load_mxcsr,
        store_mxcsr,
        x87,
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
        entries: unit
            .instructions
            .iter()
            .enumerate()
            .map(|(index, i)| (i.pc, index as u32))
            .collect(),
    };
    emitter.load_state();
    emitter.op(Op::Block(BlockType::Empty));
    emitter.gates(0);
    emitter.profile_guard(0);
    let count = unit.instructions.len() as u32;
    // The owner selects the entry ordinal for the loaded RIP. Every in-unit
    // edge commits its destination RIP before routing to the next instruction.
    emitter
        .op(Op::LocalGet(ENTRY_INDEX))
        .op(Op::LocalSet(ENTRY));
    emitter
        .op(Op::Loop(BlockType::Empty))
        .op(Op::Block(BlockType::Empty));
    for _ in &unit.instructions {
        emitter.op(Op::Block(BlockType::Empty));
    }
    emitter
        .op(Op::LocalGet(ENTRY))
        .op(Op::BrTable((0..count).collect::<Vec<_>>().into(), count));
    for (index, instruction) in unit.instructions.iter().enumerate() {
        emitter.op(Op::End);
        emitter.current_index = index as u32;
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        {
            // Keep one-shot accesses out of the span caches even in an artifact with a hot loop.
            let in_cycle = span_cycles
                .iter()
                .any(|range| range.contains(&instruction.pc));
            emitter.read_span = read_span.filter(|_| in_cycle);
            emitter.stack_read_span = stack_read_span.filter(|_| in_cycle);
            emitter.write_spans = if in_cycle { write_caches } else { [None; 2] };
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        {
            emitter.parallel_memo_address = false;
        }
        let loop_depth = count - index as u32;
        let head = loop_body.filter(|body| body.head == index as u32);
        if let Some((body, guard)) = head
            .filter(|body| body.natural_forward_loop())
            .zip(paged_loop_guard)
        {
            // The checked forward graph already follows the canonical entry labels.
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
    emitter.publish_state();
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    emitter.leave_parallel_atomic_if_held();
    emitter.op(Op::LocalGet(REASON)).op(Op::End);
    let mut code = CodeSection::new();
    code.function(&emitter.body);
    if dynamic_targets {
        code.function(&entry_lookup::emit(&emitter.entries));
    }
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    for (_, body, _) in &outlined_functions {
        code.function(body);
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
