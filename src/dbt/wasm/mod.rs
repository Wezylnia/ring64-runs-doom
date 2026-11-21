// Group source files by responsibility while keeping the emitter modules in one scope.
#[path = "integer/bits.rs"]
mod bits;
#[path = "integer/bmi.rs"]
mod bmi;
#[path = "execution/boundary.rs"]
mod boundary;
#[path = "execution/compound_stack.rs"]
mod compound_stack;
#[path = "execution/control.rs"]
mod control;
#[path = "integer/counts.rs"]
mod counts;
#[path = "integer/data.rs"]
mod data;
#[path = "system/debug.rs"]
mod debug;
#[path = "execution/entry_lookup.rs"]
mod entry_lookup;
#[path = "integer/exchange.rs"]
mod exchange;
#[path = "execution/far.rs"]
mod far;
#[path = "system/fast_system.rs"]
mod fast_system;
#[path = "numeric/float.rs"]
mod float;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[path = "execution/float_dyadic.rs"]
mod float_dyadic;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
#[path = "execution/float_loop.rs"]
mod float_loop;
#[path = "integer/integer.rs"]
mod integer;
#[path = "integer/integer_memory.rs"]
mod integer_memory;
#[path = "system/io.rs"]
mod io;
#[path = "execution/loop_cfg.rs"]
mod loop_cfg;
#[path = "memory/memory.rs"]
mod memory;
#[path = "numeric/mmx.rs"]
mod mmx;
#[path = "integer/multiply_divide.rs"]
mod multiply_divide;
#[path = "numeric/numeric_state.rs"]
mod numeric_state;
#[path = "system/paging.rs"]
mod paging;
#[path = "system/processor.rs"]
mod processor;
#[path = "system/protection.rs"]
mod protection;
#[path = "system/protection_keys.rs"]
mod protection_keys;
#[cfg(any(test, all(not(feature = "parallel"), target_arch = "wasm32")))]
#[path = "memory/read_loop.rs"]
mod read_loop;
#[path = "system/software_events.rs"]
mod software_events;
#[path = "numeric/ssse3.rs"]
mod ssse3;
#[path = "execution/stack.rs"]
mod stack;
#[cfg(any(test, all(not(feature = "parallel"), target_arch = "wasm32")))]
#[path = "memory/stack_slots.rs"]
mod stack_slots;
#[path = "execution/state.rs"]
mod state;
#[path = "memory/streaming.rs"]
mod streaming;
#[path = "memory/strings.rs"]
mod strings;
#[path = "execution/structured_loop.rs"]
mod structured_loop;
#[path = "system/system_flags.rs"]
mod system_flags;
#[path = "integer/unary.rs"]
mod unary;
#[path = "numeric/vector.rs"]
mod vector;
#[path = "numeric/x87.rs"]
mod x87;

#[cfg(all(feature = "parallel", target_arch = "wasm32"))]
#[path = "memory/parallel.rs"]
mod parallel;

use std::collections::BTreeMap;
use wasm_encoder::{
    BlockType, CodeSection, EntityType, ExportKind, ExportSection, Function, FunctionSection,
    ImportSection, Instruction as Op, MemoryType, Module, Section, TypeSection, ValType,
};

use super::unit::Unit;
use crate::embedding::layout::DISPATCH;
use crate::x86::context::{ExecutionContext, Segment};
use crate::x86::instruction::{Alu, Instruction, Location, MemoryAluOperands, Operation, Repeat};
use structured_loop::StructuredLoop;

const ENTRY_INDEX: u32 = 4;
const GPR: u32 = 5;
const FLAGS: u32 = 21;
const RIP: u32 = 22;
const REMAINING: u32 = 23;
const ENTRY: u32 = 24;
const REASON: u32 = 25;
const LHS: u32 = 26;
const RHS: u32 = 27;
const RESULT: u32 = 28;
const ADDRESS: u32 = 29;
const VALUE: u32 = 30;
const STORE_STATUS: u32 = 31;
const DIRECTION: u32 = 32;
const SYSTEM: u32 = 33;
const SHADOW: u32 = 34;
const REPEAT_REMAINING: u32 = 35;
const ENTRY_FLAGS: u32 = 36;

struct Emitter {
    paged: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    read_span: Option<memory::SpanCache>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    stack_read_span: Option<memory::SpanCache>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    write_spans: [Option<memory::SpanCache>; 2],
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    proved_stack: Option<(stack_slots::StackSlots, u32)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    proved_reads: Option<(read_loop::ReadLoop, u32)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    proved_read_retry: Option<(u32, u32)>, // tail ordinal and one-shot retry local
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    zero_upper_xmm: u16,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    normal_low_f32: u16,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    float_outline_params: u32,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    float_raw_sticky: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    float_dyadic: Option<float_dyadic::DyadicReduction>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_atomic: Option<parallel::Atomic>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_scalar: Option<parallel::Scalar>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_vector_dword: Option<u32>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_vector_span: Option<(u32, [Option<parallel::VectorSpanMemo>; 2], u32)>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_dword_window: Option<parallel::DwordWindow>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_retry: Option<u32>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_address_memos: Vec<(u32, u32)>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_memo_address: bool,
    body: Function,
    branch_hints: Vec<wasm_encoder::BranchHint>,
    entries: BTreeMap<u64, u32>,
    current_index: u32,
    structured_loop: Option<(StructuredLoop, u32)>,
    loop_gates_prechecked: bool,
    register_upper_dwords: [Option<u32>; 16],
    narrow_gprs: bool,
    omit_result_status: bool,
    flags_on_read_fault: Option<loop_cfg::ReadFaultFlags>,
    instruction_retirement: control::InstructionRetirement,
    entry_lookup: Option<u32>,
    multiply_high: Option<u32>,
    divide: Option<u32>,
    divide_failed: Option<u32>,
    stores: bool,
    has_repeat: bool,
    restore_string_flags: bool,
    context: ExecutionContext,
    segment: Segment,
    access_reason: Option<u32>,
    target_check: Option<u32>,
    vector_registers: [Option<u32>; 16],
    vector_written: u16,
    vector_temporaries: Option<[u32; 3]>,
    block16_for_write: Option<u32>,
    vector_load: Option<u32>,
    vector_store: Option<u32>,
    vector_access_reason: Option<u32>,
    float: Option<u32>,
    load_mxcsr: Option<u32>,
    store_mxcsr: Option<u32>,
    x87: Option<u32>,
    io: Option<u32>,
    mmx: Option<u32>,
    numeric_image: Option<u32>,
    masked_store: Option<u32>,
    clflush: Option<u32>,
    protection: Option<u32>,
    far_control: Option<u32>,
    compound_stack: Option<u32>,
    software_event: Option<u32>,
    system_flags: Option<u32>,
    debug_step: Option<u32>,
    debug_register: Option<u32>,
    paging_control: Option<u32>,
    invpcid: Option<u32>,
    processor: Option<u32>,
    protection_key: Option<u32>,
    fast_system: Option<u32>,
    debug_begin: Option<u32>,
    external_step: Option<u32>,
    halt: Option<u32>,
    observed: bool,
    mutable_controls: bool,
    publish_shadow: bool,
    sampled_tf: Option<u32>,
    incoming_shadow: Option<u32>,
    next_shadow: Option<u32>,
    boundary_result: Option<u32>,
    current_pc: u64,
    current_operation: Operation,
}

impl Emitter {
    fn op(&mut self, instruction: Op<'_>) -> &mut Self {
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        let instruction = match instruction {
            Op::LocalGet(local) => Op::LocalGet(local + self.float_outline_params),
            Op::LocalSet(local) => Op::LocalSet(local + self.float_outline_params),
            Op::LocalTee(local) => Op::LocalTee(local + self.float_outline_params),
            other => other,
        };
        self.body.instruction(&instruction);
        self
    }

    fn unlikely(&mut self, branch: Op<'_>) -> &mut Self {
        debug_assert!(matches!(branch, Op::If(_) | Op::BrIf(_)));
        if !self.observed && !cfg!(feature = "parallel") {
            self.branch_hints.push(wasm_encoder::BranchHint {
                branch_func_offset: self.body.byte_len() as u32,
                branch_hint_value: 0,
            });
        }
        self.op(branch)
    }

    fn instruction(
        &mut self,
        instruction: Instruction,
        next: Option<&Instruction>,
        loop_depth: u32,
        write_segment_base: Option<u32>,
        compare_source: Option<loop_cfg::ConditionSource>,
    ) {
        self.current_pc = instruction.pc;
        self.current_operation = instruction.operation;
        self.stores = false;
        let exit_depth = loop_depth + 1;
        if !self.loop_gates_prechecked {
            self.gates(exit_depth);
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        self.parallel_boundary(exit_depth);
        self.begin(instruction.operation, exit_depth);
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        let exit_depth = self.begin_parallel_instruction(instruction, exit_depth);
        match instruction.operation {
            Operation::ProtectionKey(operation) => self.protection_key(operation, exit_depth),
            Operation::Processor(operation) => {
                self.processor(operation, exit_depth);
                if operation == crate::x86::processor::Instruction::Cpuid {
                    self.commit(instruction.next_pc);
                    if self.finish_instruction(exit_depth) {
                        return;
                    }
                    self.external_boundary(exit_depth);
                    self.gates(exit_depth);
                    self.exit(DISPATCH, exit_depth);
                    return;
                }
            }
            Operation::FastSystem(operation) => {
                self.fast_system(operation, instruction.next_pc, exit_depth);
                return;
            }
            Operation::SegmentBase {
                segment,
                register,
                width,
                write,
            } => {
                if write {
                    self.op(Op::I32Const(segment as i32));
                    self.register(register, width);
                    self.op(Op::Call(
                        write_segment_base.expect("segment base write import"),
                    ))
                    .op(Op::LocalTee(REASON))
                    .op(Op::BrIf(exit_depth));
                } else {
                    let base = self.context.segment_base(segment) & width.mask();
                    self.op(Op::I64Const(base as i64))
                        .op(Op::LocalSet(GPR + u32::from(register)));
                }
            }
            Operation::Paging(operation) => {
                self.paging_control(operation, instruction.next_pc, exit_depth);
                return;
            }
            Operation::Halt => {
                self.halt(instruction.next_pc, exit_depth);
                return;
            }
            Operation::SoftwareEvent(operation) => {
                if self.software_event(operation, instruction.next_pc, exit_depth) {
                    return;
                }
            }
            Operation::DebugRegister {
                register,
                gpr,
                write,
                width,
            } => {
                self.debug_register(register, gpr, write, width, instruction.next_pc, exit_depth);
                return;
            }
            Operation::SystemFlags(operation) => self.system_flags(operation, exit_depth),
            Operation::PopSs { width } => self.pop_ss(width, exit_depth),
            Operation::MaskedStore {
                data,
                mask,
                mmx,
                address,
            } => self.masked_store(data, mask, mmx, address, instruction.next_pc, exit_depth),
            Operation::Far(far) => {
                self.far_control(far, instruction.next_pc, exit_depth);
                return;
            }
            Operation::Protection(operation) => {
                self.protection(operation, instruction.next_pc, exit_depth)
            }
            Operation::Clflush { address } => {
                self.clflush(address, instruction.next_pc, exit_depth)
            }
            Operation::Io { input, port, width } => self.io(input, port, width, exit_depth),
            Operation::Nop | Operation::X87CompatibilityNop => {}
            Operation::Mmx { op, address } => {
                self.mmx(op, address, instruction.next_pc, exit_depth)
            }
            Operation::NumericImage { op, address } => {
                self.numeric_image(op, address, instruction.next_pc, exit_depth)
            }
            Operation::X87(x87) => self.x87(x87, instruction.pc, instruction.next_pc, exit_depth),
            Operation::Float(float) => self.float(float, instruction.next_pc, exit_depth),
            Operation::Mxcsr { address, store } => {
                self.mxcsr(address, store, instruction.next_pc, exit_depth)
            }
            Operation::Vector(vector) => self.vector(vector, instruction.next_pc, exit_depth),
            Operation::Exchange { dst, src, width } => {
                self.exchange(dst, src, width, instruction.next_pc, exit_depth)
            }
            Operation::Xadd { dst, src, width } => {
                self.xadd(dst, src, width, instruction.next_pc, exit_depth)
            }
            Operation::CompareExchange { dst, src, width } => {
                self.compare_exchange(dst, src, width, instruction.next_pc, exit_depth)
            }
            Operation::CompareExchange8b { address } => {
                self.compare_exchange8b(address, instruction.next_pc, exit_depth)
            }
            Operation::CompareExchange16b { address } => {
                self.compare_exchange16b(address, instruction.next_pc, exit_depth)
            }
            Operation::Direction { set } => self.direction(set),
            Operation::String {
                kind,
                width,
                address_width,
                source_segment,
                repeat,
            } => {
                self.string(
                    kind,
                    (width, address_width),
                    source_segment,
                    repeat,
                    exit_depth,
                );
            }
            Operation::Mov { dst, src, width } => {
                self.data_operand(src, width);
                self.write_register(dst, width);
            }
            Operation::Alu {
                kind,
                dst,
                src,
                width,
            } => {
                let overwrite = !self.observed
                    && !cfg!(feature = "parallel")
                    && matches!(kind, Alu::Add | Alu::Sub)
                    && next.is_some_and(|next| {
                        next.pc == instruction.next_pc
                            && matches!(
                                next.operation,
                                Operation::Alu {
                                    kind: Alu::Cmp | Alu::Test,
                                    ..
                                }
                            )
                    });
                self.integer(
                    kind,
                    dst,
                    src,
                    width,
                    overwrite.then_some(instruction.next_pc),
                );
            }
            Operation::MemoryAlu {
                kind,
                address,
                operands,
                width,
            } => self.memory_integer(
                kind,
                address,
                operands,
                width,
                instruction.next_pc,
                exit_depth,
            ),
            Operation::Load {
                dst,
                address,
                width,
            } => self.load(dst, address, width, instruction.next_pc, exit_depth),
            Operation::Store {
                src,
                address,
                width,
            } => self.store(src, address, width, instruction.next_pc, exit_depth),
            Operation::Lea {
                dst,
                address,
                width,
            } => self.lea(dst, address, width, instruction.next_pc),
            Operation::Extend {
                dst,
                src,
                source_width,
                width,
                signed,
            } => self.extend(
                dst,
                src,
                (source_width, width),
                signed,
                instruction.next_pc,
                exit_depth,
            ),
            Operation::SignHigh { width } => self.sign_high(width),
            Operation::Unary { kind, dst, width } => {
                self.unary(kind, dst, width, instruction.next_pc, exit_depth)
            }
            Operation::Multiply { signed, src, width } => {
                self.multiply(signed, src, width, instruction.next_pc, exit_depth)
            }
            Operation::Imul {
                dst,
                src,
                immediate,
                width,
            } => self.imul(dst, src, immediate, width, instruction.next_pc, exit_depth),
            Operation::Divide { signed, src, width } => {
                self.divide(signed, src, width, instruction.next_pc, exit_depth)
            }
            Operation::Shift {
                kind,
                dst,
                count,
                width,
            } => self.shift(kind, dst, count, width, instruction.next_pc, exit_depth),
            Operation::DoubleShift {
                kind,
                dst,
                src,
                count,
                width,
            } => self.double_shift(
                kind,
                dst,
                (src, count),
                width,
                instruction.next_pc,
                exit_depth,
            ),
            Operation::Carry { kind } => self.carry(kind),
            Operation::Lahf => self.lahf(),
            Operation::Sahf => self.sahf(),
            Operation::Bit {
                kind,
                dst,
                index,
                width,
            } => self.bit(kind, dst, index, width, instruction.next_pc, exit_depth),
            Operation::Scan {
                reverse,
                dst,
                src,
                width,
            } => self.scan(reverse, dst, src, width, instruction.next_pc, exit_depth),
            Operation::Bmi {
                kind,
                dst,
                src,
                width,
            } => self.bmi(kind, dst, src, width, instruction.next_pc, exit_depth),
            Operation::Bswap { dst, width } => self.bswap(dst, width),
            Operation::Setcc { dst, condition } => {
                self.setcc(dst, condition, instruction.next_pc, exit_depth);
            }
            Operation::Cmov {
                dst,
                src,
                width,
                condition,
            } => self.cmov(
                dst,
                src,
                width,
                (condition, compare_source),
                instruction.next_pc,
                exit_depth,
            ),
            Operation::Push { src, width } => {
                self.push(src, width, instruction.next_pc, exit_depth)
            }
            Operation::Pop { dst, width } => self.pop(dst, width, exit_depth),
            Operation::PopMemory { address, width } => {
                self.pop_memory(address, width, instruction.next_pc, exit_depth)
            }
            Operation::Leave { width } => self.leave(width, exit_depth),
            Operation::CompoundStack(operation) => self.compound_stack(operation, exit_depth),
            Operation::Call { target, width } => {
                self.call(target, width, instruction.next_pc, loop_depth, exit_depth);
                return;
            }
            Operation::JumpRegister { target, width } => {
                self.jump_register(target, width, exit_depth);
                return;
            }
            Operation::JumpMemory { address, width } => {
                self.jump_memory(address, width, instruction.next_pc, exit_depth);
                return;
            }
            Operation::Return { cleanup, width } => {
                self.return_stack(cleanup, width, exit_depth);
                return;
            }
            Operation::Jump { target } => {
                self.target(target, loop_depth, exit_depth);
                return;
            }
            Operation::Jcc { condition, target } => {
                self.cloned_condition(condition, compare_source);
                self.op(Op::If(BlockType::Empty));
                if let Some(loop_cfg::ConditionSource::OperandsWithTakenFlags(width)) =
                    compare_source
                {
                    self.arithmetic_result(Alu::Cmp, width);
                    self.arithmetic_flags(Alu::Cmp, width);
                }
                self.target(target, loop_depth + 1, exit_depth + 1);
                self.op(Op::End);
                #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
                if let Some((tail, retry)) = self.proved_read_retry
                    && self.current_index == tail
                    && self.structured_loop.is_none()
                {
                    // Falling out of the resumed lap loses its stream proof.
                    self.op(Op::I32Const(0)).op(Op::LocalSet(retry));
                }
            }
            Operation::CountBranch {
                condition,
                count_width,
                target,
            } => {
                self.count_branch(condition, count_width, target, loop_depth, exit_depth);
            }
        }
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        let exit_depth = self.end_parallel_instruction(instruction, exit_depth);
        self.commit(instruction.next_pc);
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if self.float_outline_params != 0
            && let Some((body, _)) = self.structured_loop
            && self.current_index == body.tail
        {
            self.op(Op::I32Const((body.tail + 1) as i32))
                .op(Op::LocalSet(ENTRY))
                .op(Op::Br(1));
            return;
        }
        self.route(instruction.next_pc, loop_depth, exit_depth);
    }
}

#[cfg(test)]
pub(crate) fn emit(unit: &Unit) -> Vec<u8> {
    emit_observed(unit, false)
}

#[cfg(test)]
pub(crate) fn emit_observed(unit: &Unit, observed: bool) -> Vec<u8> {
    emit_profile(unit, observed, false, usize::MAX)
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
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if !observed
        && let Some(body) = StructuredLoop::select_shared_float(&unit.instructions)
            .or_else(|| StructuredLoop::select_shared_rotated_reads(unit))
    {
        // Every shared placement retains its exact per-instruction signal check.
        let bytes = emit_placement(unit, observed, paged, Some(body), false, max_artifact_bytes);
        if bytes.len() <= max_artifact_bytes {
            return bytes;
        }
    }
    if !observed && let Some(loop_body) = StructuredLoop::select(&unit.instructions) {
        let bytes = emit_placement(
            unit,
            observed,
            paged,
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
                paged,
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
                paged,
                Some(loop_body),
                false,
                max_artifact_bytes,
            );
            if bytes.len() <= max_artifact_bytes {
                return bytes;
            }
        }
    }
    emit_placement(unit, observed, paged, None, false, max_artifact_bytes)
}

fn emit_placement(
    unit: &Unit,
    observed: bool,
    paged: bool,
    loop_body: Option<StructuredLoop>,
    guard_reads: bool,
    max_artifact_bytes: usize,
) -> Vec<u8> {
    let reads = if cfg!(all(feature = "parallel", target_arch = "wasm32"))
        && !observed
        && !paged
        && unit.context.mode() == crate::x86::context::Mode::Protected
        && unit.context.code32()
        && unit.context.translate(0, 4, Segment::Ds, crate::memory::Access::Read, false).ok() == Some(0)
        && unit.instructions.iter().any(|i| {
            matches!(i.operation, Operation::Jcc { target, .. } | Operation::Jump { target } if target < i.pc)
        })
    {
        GuardedReads::SharedWindow
    } else if cfg!(all(feature = "parallel", target_arch = "wasm32")) {
        GuardedReads::SharedDword
    } else if !guard_reads {
        GuardedReads::Checked
    } else if cfg!(all(not(feature = "parallel"), target_arch = "wasm32")) {
        GuardedReads::Outlined {
            sticky: true,
            dyadic: true,
        }
    } else {
        GuardedReads::Inline
    };
    let (bytes, extra_spans) = emit_body(
        unit,
        observed,
        paged,
        loop_body,
        reads,
        true,
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
            paged,
            loop_body,
            reads,
            false,
            max_artifact_bytes,
        )
        .0;
    }
    bytes
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum GuardedReads {
    Checked,
    Inline,
    SharedDword,
    SharedWindow,
    Outlined { sticky: bool, dyadic: bool },
}

fn emit_body(
    unit: &Unit,
    observed: bool,
    paged: bool,
    loop_body: Option<StructuredLoop>,
    _guard_reads: GuardedReads,
    _extra_spans: bool,
    max_artifact_bytes: usize,
) -> (Vec<u8>, bool) {
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
    let multiply_high = multiply_type.map(|index| {
        imports.import("env", "multiply_high", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let divide = divide_type.map(|index| {
        imports.import("env", "divide", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let divide_failed = wide_divide.then(|| {
        imports.import("env", "divide_failed", EntityType::Function(2));
        let function = run_index;
        run_index += 1;
        function
    });
    let access_reason = (segmented || paged).then(|| {
        imports.import("env", "access_reason", EntityType::Function(2));
        let function = run_index;
        run_index += 1;
        function
    });
    let target_check = segmented.then(|| {
        imports.import("env", "target_check", EntityType::Function(4));
        let function = run_index;
        run_index += 1;
        function
    });
    let vector_load = vector_load_type.map(|index| {
        imports.import("env", "vector_load", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let block16_for_write = block16_for_write_type.map(|index| {
        imports.import("env", "block16_for_write", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let vector_store = vector_store_type.map(|index| {
        imports.import("env", "vector_store", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let float = float_type.map(|index| {
        imports.import("env", "float", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let load_mxcsr = load_mxcsr_used.then(|| {
        imports.import(
            "env",
            "load_mxcsr",
            EntityType::Function(mxcsr_type.unwrap()),
        );
        let function = run_index;
        run_index += 1;
        function
    });
    let store_mxcsr = store_mxcsr_used.then(|| {
        imports.import(
            "env",
            "store_mxcsr",
            EntityType::Function(mxcsr_type.unwrap()),
        );
        let function = run_index;
        run_index += 1;
        function
    });
    let io = io_type.map(|index| {
        imports.import("env", "io", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let x87 = x87_type.map(|index| {
        imports.import("env", "x87", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let mmx = has_mmx.then(|| {
        imports.import(
            "env",
            "mmx",
            EntityType::Function(numeric_state_type.unwrap()),
        );
        let function = run_index;
        run_index += 1;
        function
    });
    let numeric_image = has_numeric_image.then(|| {
        imports.import(
            "env",
            "numeric_image",
            EntityType::Function(numeric_state_type.unwrap()),
        );
        let function = run_index;
        run_index += 1;
        function
    });
    let masked_store = masked_store_type.map(|index| {
        imports.import("env", "masked_store", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let clflush = clflush_type.map(|index| {
        imports.import("env", "clflush", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let protection = protection_type.map(|index| {
        imports.import("env", "protection", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let compound_stack = compound_stack_type.map(|index| {
        imports.import("env", "compound_stack", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let far_control = far_type.map(|index| {
        imports.import("env", "far_control", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let software_event = software_event_type.map(|index| {
        imports.import("env", "software_event", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let system_flags = system_flags_type.map(|index| {
        imports.import("env", "flags", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let debug_register = debug_register_type.map(|index| {
        imports.import("env", "debug_register", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let paging_control = paging_type.map(|index| {
        imports.import("env", "paging_control", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let invpcid = invpcid_type.map(|index| {
        imports.import("env", "invpcid", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let fast_system = fast_system_type.map(|index| {
        imports.import("env", "fast_system", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let write_segment_base = write_segment_base_type.map(|index| {
        imports.import("env", "write_segment_base", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let processor = processor_type.map(|index| {
        imports.import("env", "processor", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let protection_key = protection_key_type.map(|index| {
        imports.import("env", "protection_key", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let debug_begin = debug_begin_type.map(|index| {
        imports.import("env", "debug_begin", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let debug_step = debug_type.map(|index| {
        imports.import("env", "debug_finish", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let external_step = external_step_type.map(|index| {
        imports.import("env", "external_step", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let halt = halt_type.map(|index| {
        imports.import("env", "halt", EntityType::Function(index));
        let function = run_index;
        run_index += 1;
        function
    });
    let vector_access_reason = if vector_banks.loads || vector_store.is_some() || store_mxcsr_used {
        Some(access_reason.unwrap_or_else(|| {
            imports.import("env", "access_reason", EntityType::Function(2));
            let function = run_index;
            run_index += 1;
            function
        }))
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
            imports.import("env", "parallel_begin", EntityType::Function(2));
            imports.import("env", "parallel_end", EntityType::Function(release_type));
            imports.import("env", "parallel_guard", EntityType::Function(guard_type));
            imports.import(
                "env",
                "parallel_unguard",
                EntityType::Function(unguard_type),
            );
            let begin = run_index;
            run_index += 4;
            begin
        });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_resolve = (!paged && !observed).then(|| {
        let signature = add_function_type(
            &mut types,
            [ValType::I64, ValType::I32, ValType::I32, ValType::I32],
            [ValType::I32],
        );
        imports.import("env", "parallel_resolve", EntityType::Function(signature));
        let index = run_index;
        run_index += 1;
        index
    });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let parallel_vector_dword = (matches!(
        _guard_reads,
        GuardedReads::SharedDword | GuardedReads::SharedWindow
    ) && parallel_resolve.is_some()
        && vector_banks.loads)
        .then(|| {
            let signature =
                add_function_type(&mut types, [ValType::I64, ValType::I32], [ValType::I64]);
            imports.import(
                "env",
                "parallel_vector_dword",
                EntityType::Function(signature),
            );
            let index = run_index;
            run_index += 1;
            index
        });
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    let window_resolve = (_guard_reads == GuardedReads::SharedWindow
        && unit
            .instructions
            .iter()
            .any(|i| parallel::DwordWindow::eligible(i.operation)))
    .then(|| {
        let signature = add_function_type(&mut types, [ValType::I64], [ValType::I32]);
        imports.import(
            "env",
            "parallel_dword_window",
            EntityType::Function(signature),
        );
        let index = run_index;
        run_index += 1;
        index
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
        imports.import(
            "env",
            "parallel_vector_dword_span",
            EntityType::Function(signature),
        );
        let index = run_index;
        run_index += 1;
        index
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
        imports.import("env", name, EntityType::Function(signature));
        let index = run_index;
        run_index += 1;
        index
    };
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
                if let Operation::Store { address, .. } = instruction.operation
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
    let parallel_dword_window = window_resolve.map(|resolver| {
        let first = next_local;
        locals.push((4, ValType::I32));
        next_local += 4;
        parallel::DwordWindow::new(resolver, first)
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
    let float_outline = if let GuardedReads::Outlined { sticky, dyadic } = _guard_reads {
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
    let mut outlined_functions = Vec::new();
    let mut emitter = Emitter {
        paged,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        read_span: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        stack_read_span: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        write_spans: [None; 2],
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        proved_stack: None,
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        proved_reads: None,
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
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_atomic,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_scalar,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_vector_dword,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_vector_span,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_dword_window,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_retry: None,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_address_memos,
        #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
        parallel_memo_address: false,
        current_index: 0,
        structured_loop: None,
        loop_gates_prechecked: false,
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
        target_check,
        vector_registers,
        vector_written: vector_banks.written,
        vector_temporaries,
        vector_load,
        block16_for_write,
        vector_store,
        vector_access_reason,
        float,
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
        if let Some(loop_body) = loop_body.filter(|body| body.head == index as u32) {
            if loop_body.precheck_gates_without_read_proof() {
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
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && parallel_dword_window.is_some() {
        // Keep any existing shared FP32 trial when the window alone exceeds the cap.
        return emit_body(
            unit,
            observed,
            paged,
            loop_body,
            GuardedReads::SharedDword,
            _extra_spans,
            max_artifact_bytes,
        );
    }
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    if bytes.len() > max_artifact_bytes && parallel_vector_span.is_some() {
        return emit_body(
            unit,
            observed,
            paged,
            loop_body,
            _guard_reads,
            false,
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
            paged,
            loop_body,
            GuardedReads::Checked,
            _extra_spans,
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
            paged,
            loop_body,
            GuardedReads::Outlined {
                sticky: sticky && outline.dyadic.is_some(),
                dyadic: false,
            },
            _extra_spans,
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
            paged,
            loop_body,
            GuardedReads::Inline,
            _extra_spans,
            max_artifact_bytes,
        );
    }
    #[cfg(any(feature = "parallel", not(target_arch = "wasm32")))]
    let extra_spans = false;
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    let extra_spans = write_span.is_some() || stack_read_span.is_some();
    (bytes, extra_spans)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
