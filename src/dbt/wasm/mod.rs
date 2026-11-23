mod execution;
mod integer;
mod memory;
mod numeric;
mod system;

mod artifact;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) use artifact::placement::emit_flat_trace;
#[cfg(all(not(feature = "parallel"), any(target_arch = "wasm32", test)))]
pub(crate) use artifact::placement::emit_mmio_profile;
#[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
pub(crate) use artifact::placement::emit_paged_vector_copy;
pub(crate) use artifact::placement::emit_profile;

use std::collections::BTreeMap;
use wasm_encoder::{Function, Instruction as Op};

use super::unit::Unit;
use crate::x86::context::{ExecutionContext, Segment};
use crate::x86::instruction::Operation;
use execution::loops::structured_loop::StructuredLoop;

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
    #[cfg(guest_memory64)]
    guest64_cache: Option<memory::guest64::SpanLocals>,
    paged: bool,
    quiet_fetch_page: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_paged_read: Option<[u32; 4]>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_scalar_stamp: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_store_fill: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_dword_prefixes: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_quiet_completion: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    sampled_region: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    retouch_prefix_fetch: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    retained_fetch_slot: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    ordered_operand: Option<(u32, u32, u32)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    used_paged_quiet_completion: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    read_span: Option<memory::SpanCache>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    stack_read_span: Option<memory::SpanCache>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    flat_stack_pointer_span: Option<memory::SpanCache>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    flat_stack_access: Option<(u32, memory::flat::stack::StackAccess)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    flat_data_bias: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    stack_pointer_relative: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    read_loop_diamond: Option<execution::loops::loop_cfg::ReadLoopDiamond>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    write_spans: [Option<memory::SpanCache>; 2],
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    linear_call_spans: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    proved_stack: Option<(memory::stack_slots::StackSlots, u32)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    indexed_read_window: Option<memory::read_loop::IndexedReadWindow>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    proved_reads: Option<(memory::read_loop::ReadLoop, u32)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_proved_f32_reads: Option<(memory::read_loop::ReadLoop, u32)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_f32_read_pair: Option<(u32, u32, Option<u32>)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_f32_prefix_floor: Option<u32>,
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
    float_dyadic: Option<execution::loops::float_dyadic::DyadicReduction>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_float_dyadic: Option<execution::loops::float_dyadic::DyadicReduction>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_float_sticky: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_atomic: Option<memory::parallel::Atomic>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_scalar: Option<memory::parallel::Scalar>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_vector_dword: Option<u32>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_vector_span: Option<(u32, [Option<memory::parallel::VectorSpanMemo>; 2], u32)>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    compact_shared_reads: bool,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_dword_window: Option<memory::parallel::DwordWindow>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_base_byte_window: Option<memory::parallel::DwordWindow>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_byte_store_span: Option<memory::parallel::ByteStoreSpan>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_retry: Option<u32>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_address_memos: Vec<(u32, u32)>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_native_lock_memo: Option<(u32, u32, u32)>,
    #[cfg(all(feature = "parallel", target_arch = "wasm32"))]
    parallel_memo_address: bool,
    body: Function,
    branch_hints: Vec<wasm_encoder::BranchHint>,
    entries: BTreeMap<u64, u32>,
    current_index: u32,
    structured_loop: Option<(StructuredLoop, u32)>,
    loop_gates_prechecked: bool,
    guarded_paged_lap: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_prefix_read: Option<(u32, bool, bool)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_prefix_store: Option<(u32, bool)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_prefix_fetch_out: Option<(u32, usize)>,
    paged_loop_guard: Option<(u32, u32)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_preamble: Option<(execution::loops::structured_loop::PagedPreamble, u32, u32)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_preamble_latch: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_read_rearm: Option<(u32, execution::loops::loop_cfg::LoopOrder)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_read_span: Option<(memory::read_loop::ReadLoop, u32, u32)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_dense_read_span: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_dword_window: Option<memory::paged_read::PagedReadWindow>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_dword_window_touch: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_dword_read_pair: Option<memory::paged_read::PagedDwordReadPair>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_indexed_write: Option<execution::paged::indexed_write::GuardedIndexedWrite>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_vector_window: Option<memory::paged_read::PagedReadWindow>,
    paged_loop_read_byte: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_byte_write_pair: Option<memory::PagedByteWritePair>,
    paged_loop_read_dword: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    direct_loop_reads: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    direct_read_range: Option<(crate::x86::instruction::Address, [u32; 3])>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_loop_read_block: Option<(u32, bool)>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_loop_write_block: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_copy_window: Option<execution::paged::copy::CopyWindow>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_copy_access_cache: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    linear_recurrence: Option<execution::loops::linear_recurrence::Recurrence>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    paged_write_ready: Option<u32>,
    paged_loop_write_byte: Option<u32>,
    paged_loop_write_dword: Option<u32>,
    register_upper_dwords: [Option<u32>; 16],
    narrow_gprs: bool,
    omit_result_status: bool,
    flags_on_read_fault: Option<execution::loops::loop_cfg::ReadFaultFlags>,
    instruction_retirement: execution::control::InstructionRetirement,
    entry_lookup: Option<u32>,
    dense_dispatch: bool,
    packed_result_status: bool,
    multiply_high: Option<u32>,
    divide: Option<u32>,
    divide_failed: Option<u32>,
    stores: bool,
    has_repeat: bool,
    restore_string_flags: bool,
    context: ExecutionContext,
    segment: Segment,
    access_reason: Option<u32>,
    mmio_operand: Option<u32>,
    target_check: Option<u32>,
    vector_registers: [Option<u32>; 16],
    vector_written: u16,
    vector_temporaries: Option<[u32; 3]>,
    block16_for_write: Option<u32>,
    vector_load: Option<u32>,
    vector_store: Option<u32>,
    vector_access_reason: Option<u32>,
    float: Option<u32>,
    inline_f32_compares: bool,
    inline_f32_sub: bool,
    inline_f32_packed: bool,
    inline_cmpnleps: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    direct_vector_loads: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    used_direct_vector_load: bool,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    direct_vector_stores: bool,
    load_mxcsr: Option<u32>,
    store_mxcsr: Option<u32>,
    x87: Option<u32>,
    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    x87_register_pairs: bool,
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
    flat_retire: Option<u32>,
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
        #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
        if matches!(instruction, Op::Call(_))
            && let Some(slot) = self.retained_fetch_slot
        {
            // A successful helper can change code, translations, debug or event state.
            self.body.instruction(&Op::I32Const(0));
            self.body.instruction(&Op::LocalSet(slot));
            if let Some((_, _, rights)) = self.ordered_operand {
                self.body.instruction(&Op::I32Const(0));
                self.body.instruction(&Op::LocalSet(rights));
            }
        }
        self.body.instruction(&instruction);
        self
    }

    fn unlikely(&mut self, branch: Op<'_>) -> &mut Self {
        self.hint_unlikely(branch, !self.observed && !cfg!(feature = "parallel"))
    }

    #[cfg(all(not(feature = "parallel"), target_arch = "wasm32"))]
    fn indexed_miss(&mut self, branch: Op<'_>) -> &mut Self {
        self.hint_unlikely(branch, self.retouch_prefix_fetch)
    }

    fn hint_unlikely(&mut self, branch: Op<'_>, enabled: bool) -> &mut Self {
        debug_assert!(matches!(branch, Op::If(_) | Op::BrIf(_)));
        if enabled {
            self.branch_hints.push(wasm_encoder::BranchHint {
                branch_func_offset: self.body.byte_len() as u32,
                branch_hint_value: 0,
            });
        }
        self.op(branch)
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests;
