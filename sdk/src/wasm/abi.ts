import type { DebugLayout } from '../state/execution/debug.js';
import type {
  EventLayout,
  InterruptLayout,
  PagingSourceLayout,
} from '../state/execution/events.js';
import type { NumericSourceLayout } from '../state/numeric/numeric-controls.js';
import type { X87Layout } from '../state/numeric/x87.js';
import type { ProtectionKeysLayout } from '../state/system/protection-keys.js';
import type { TimeLayout } from '../state/system/time.js';

export interface CoreStateLayout {
  time: TimeLayout;
  protectionKeys: ProtectionKeysLayout;
  xcr0?: number;
  pagingSource?: PagingSourceLayout;
  numericSource?: NumericSourceLayout;
  debug: DebugLayout;
  size: number;
  scalarSize: number;
  xmm: number;
  xmmBytes: number;
  vectorScratch: number;
  mxcsr: number;
  mxcsrBytes: number;
  x87: X87Layout;
  event: EventLayout;
  interrupts: InterruptLayout;
  deliveryEnabled: number;
  registers: number;
  flags: number;
  rip: number;
  remaining: number;
  repeatRemaining: number;
  interruptibility: number;
  interruptibilityBytes: number;
  reasons: string[];
}

export type CoreExports = WebAssembly.Exports & {
  memory: WebAssembly.Memory;
  core_input(length: number): number;
  core_init(codeBase: bigint, codeLen: number, ramBase: bigint, ramLen: number): number;
  core_init_mapped(entry: bigint, metadataLength: number): number;
  core_add_backing(metadataLength: number): number;
  core_remove_backing(): number;
  core_set_mappings(): number;
  core_validity(owner: bigint): number;
  core_take_source_invalidation?(): number;
  core_publish(owner: bigint): number;
  core_activate(owner: bigint): number;
  core_release(owner: bigint): void;
  core_context_info(): void;
  core_context_version(): bigint;
  core_boundary_observed(): number;
  core_set_context(): number;
  core_paging_state(): number;
  core_import_paging(): number;
  core_fast_system_state(): number;
  core_import_fast_system(): number;
  core_write_segment_base?(segment: number, base: bigint): number;
  core_fast_system(
    kind: number,
    wide: number,
    next: bigint,
    sp: bigint,
    rcx: bigint,
    rdx: bigint,
    r11: bigint,
    flags: bigint,
  ): number;
  core_paging_control(
    kind: number,
    register: number,
    value: bigint,
    pc: bigint,
    flags: bigint,
    retired: bigint,
  ): number;
  core_invpcid(segment: number, offset: bigint, kind: bigint, flags: bigint): number;
  core_processor(kind: number, eax: number, ecx: number, retired: bigint): number;
  core_protection_key(kind: number, eax: number, ecx: number, edx: number): number;
  core_io(
    descriptor: number,
    port: number,
    offset: bigint,
    value: bigint,
    segment: number,
    flags: bigint,
  ): number;
  core_io_observed: CoreExports['core_io'];
  core_io_configure(present: number): number;
  core_io_effect_active(): number;
  core_mmio_effect_active?(): number;
  core_mmio_operand?(descriptor: number, offset: bigint, value: bigint, segment: number): number;
  core_segmented_load(offset: bigint, width: number, segment: number): bigint;
  core_segmented_load_for_write(offset: bigint, width: number, segment: number): bigint;
  core_segmented_store(offset: bigint, width: number, value: bigint, segment: number): number;
  core_segmented_access_failed(): number;
  core_access_reason(): number;
  core_target_check(target: bigint): number;
  core_state_ptr(): number;
  core_import_state(): number;
  core_load_observed: CoreExports['core_load'];
  core_load_for_write_observed: CoreExports['core_load_for_write'];
  core_store_observed: CoreExports['core_store'];
  core_segmented_load_observed: CoreExports['core_segmented_load'];
  core_segmented_load_for_write_observed: CoreExports['core_segmented_load_for_write'];
  core_segmented_store_observed: CoreExports['core_segmented_store'];
  core_vector_load_observed: CoreExports['core_vector_load'];
  core_vector_store_observed: CoreExports['core_vector_store'];
  core_load_mxcsr_observed: CoreExports['core_load_mxcsr'];
  core_store_mxcsr_observed: CoreExports['core_store_mxcsr'];
  core_x87_observed: CoreExports['core_x87'];
  core_mmx_observed: CoreExports['core_mmx'];
  core_numeric_image_observed: CoreExports['core_numeric_image'];
  core_masked_store_observed: CoreExports['core_masked_store'];
  core_protection_observed: CoreExports['core_protection'];
  core_far_control_observed: CoreExports['core_far_control'];
  core_compound_stack_observed: CoreExports['core_compound_stack'];
  core_software_event_observed: CoreExports['core_software_event'];
  core_debug_finish(
    pc: bigint,
    resume: bigint,
    completed: number,
    flags: bigint,
    decision: number,
  ): number;
  core_debug_begin(pc: bigint, flags: bigint, shadow: number): number;
  core_paged_loop_guard?(pc: bigint, bytes: number, flags: bigint, shadow: number): number;
  core_paged_walk_guard?(pc: bigint, bytes: number, flags: bigint, shadow: number): number;
  core_paged_walk_loop_guard32?(pc: bigint, bytes: number, flags: bigint, shadow: number): number;
  core_paged_read_loop_guard32?(pc: bigint, bytes: number, flags: bigint, shadow: number): number;
  core_paged_sse_loop_guard?(pc: bigint, bytes: number, flags: bigint, shadow: number): number;
  core_paged_sse_write_loop_guard32?(
    pc: bigint,
    bytes: number,
    flags: bigint,
    shadow: number,
  ): number;
  core_paged_write_loop_guard32?(pc: bigint, bytes: number, flags: bigint, shadow: number): number;
  core_paged_loop_read_dword?(address: bigint): bigint;
  core_paged_loop_walk_dword?(address: bigint): bigint;
  core_paged_loop_read_dword_window?(address: bigint): number;
  core_paged_loop_read_ready?(address: bigint, width: number): number;
  core_paged_loop_split_read_ready?(address: bigint, width: number): number;
  core_paged_loop_read_block?(address: bigint): number;
  core_paged_loop_block_host?(address: bigint): number;
  core_paged_loop_block_window?(address: bigint): bigint;
  core_paged_loop_write_ready?(address: bigint, width: number): number;
  core_paged_loop_read_byte?(address: bigint): bigint;
  core_paged_loop_read_byte_for_store?(address: bigint): bigint;
  core_paged_loop_read_byte_write_pointer?(address: bigint): bigint;
  core_paged_loop_read_span?(address: bigint, bytes: number, segment: number): number;
  core_paged_loop_read_pair?(
    first: bigint,
    firstBytes: number,
    last: bigint,
    lastBytes: number,
  ): bigint;
  core_paged_loop_read_split_pair?(
    first: bigint,
    firstBytes: number,
    last: bigint,
    lastBytes: number,
  ): bigint;
  core_paged_loop_read_span_pages?(address: bigint, bytes: number, segment: number): number;
  core_paged_loop_read_span_ready?(address: bigint, bytes: number, segment: number): number;
  core_paged_loop_write_byte?(address: bigint, value: number): number;
  core_paged_loop_write_byte_after_read?(address: bigint, value: number, slot: number): number;
  core_paged_loop_write_dword?(address: bigint, value: number): number;
  core_debug_register(operation: number, input: bigint, pc: bigint, flags: bigint): number;
  core_import_debug(): number;
  core_import_xmm(): number;
  core_import_mxcsr(): number;
  core_import_x87(): number;
  core_mmx(operation: number, operand: bigint, segment: number): number;
  core_numeric_image(operation: number, offset: bigint, segment: number): number;
  core_masked_store(
    offset: bigint,
    width: number,
    data: number,
    mask: number,
    segment: number,
  ): number;
  core_far_control(
    descriptor: number,
    offset: bigint,
    selector: number,
    inputSp: bigint,
    nextPc: bigint,
    segment: number,
  ): number;
  core_compound_stack(descriptor: number): number;
  core_flags(operation: number, flags: bigint, input: bigint): number;
  core_software_event(operation: number, inputSp: bigint, flags: bigint, nextPc: bigint): number;
  core_debug_step(
    instructionPc: bigint,
    resumePc: bigint,
    instructionCompleted: number,
    flags: bigint,
  ): number;
  core_configure_delivery(enabled: number): number;
  core_stage_exception(reason: number): number;
  core_complete_task(): number;
  core_deliver_exception(): number;
  core_request_interrupt(vector: number): number;
  core_request_nmi(): number;
  core_external_boundary(): number;
  core_shutdown_can_wake(): number;
  core_external_step(pc: bigint, flags: bigint, shadow: number): number;
  core_halt(flags: bigint): number;
  core_protection(descriptor: number, offset: bigint, input: bigint, segment: number): number;
  core_clflush(offset: bigint, segment: number): number;
  core_x87(
    operation: number,
    offset: bigint,
    segment: number,
    pc: bigint,
    flags: bigint,
    rax: bigint,
  ): number;
  core_float(
    operation: number,
    leftLo: bigint,
    leftHi: bigint,
    rightLo: bigint,
    rightHi: bigint,
  ): number;
  core_load_mxcsr(offset: bigint, segment: number): number;
  core_store_mxcsr(offset: bigint, segment: number): number;
  core_block16_for_write(offset: bigint, segment: number): number;
  core_vector_load(offset: bigint, width: number, segment: number, alignment: number): number;
  core_vector_store(
    offset: bigint,
    width: number,
    segment: number,
    alignment: number,
    value: number,
  ): number;
  core_clear_fault(): number;
  core_probe(pc: bigint): number;
  core_compile(
    pc: bigint,
    maxInstructions: number,
    maxSourceBytes: number,
    maxArtifactBytes: number,
  ): number;
  core_compile_with_paged_vector_copy?: CoreExports['core_compile'];
  core_paged_loop_write_block?(address: bigint, valuePointer: number): number;
  core_load(address: bigint, width: number): bigint;
  core_read_span?(address: bigint, width: number, segment: number): number;
  core_write_span?(address: bigint, width: number, segment: number): number;
  core_load_for_write(address: bigint, width: number): bigint;
  core_store(address: bigint, width: number, value: bigint): number;
  core_access_failed(): number;
  core_multiply_high(left: bigint, right: bigint, signed: number): bigint;
  core_divide(high: bigint, low: bigint, divisor: bigint, signed: number): bigint;
  core_divide_failed(): number;
  core_fault_info(reason: number): void;
  core_read_memory(address: bigint, length: number): number;
  core_write_memory(address: bigint): number;
  core_output_ptr(): number;
  core_output_len(): number;
  core_info_ptr(): number;
  core_info_len(): number;
  core_dispose(): void;
  core_reset(): number;
  core_numeric_fault_info?(): void;
};

export type ParallelCoreExports = CoreExports & {
  core_parallel_abi(): number;
  core_parallel_add_backing(length: number, offset: number, incarnation: bigint): number;
  core_parallel_admin_changed(): void;
  core_parallel_byte_store_span?(offset: bigint): bigint;
  core_parallel_dword_window?(offset: bigint): number;
  core_parallel_finish_slice(): void;
  core_parallel_guard(offset: bigint, width: number, segment: number): number;
  core_parallel_invalidate(offset: number, length: number): void;
  core_parallel_proof(owner: bigint): number;
  core_parallel_resolve(address: bigint, width: number, access: number, segment: number): number;
  core_parallel_unguard(): void;
  core_parallel_vector_dword?(offset: bigint, segment: number): bigint;
  core_parallel_vector_dword_span?(offset: bigint, segment: number): bigint;
};

export type GeneratedRun = (
  state: number,
  budget: bigint,
  stop: bigint,
  hasStop: number,
  entryIndex: number,
) => number;
