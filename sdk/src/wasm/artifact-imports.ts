import type { CoreExports } from './abi.js';

const PAGED_HELPERS = {
  load: 'core_paged_load',
  load_for_write: 'core_paged_load_for_write',
  store: 'core_paged_store',
  segmented_load: 'core_paged_segmented_load',
  segmented_load_for_write: 'core_paged_segmented_load_for_write',
  segmented_store: 'core_paged_segmented_store',
  vector_load: 'core_paged_vector_load',
  vector_store: 'core_paged_vector_store',
  load_mxcsr: 'core_paged_load_mxcsr',
  store_mxcsr: 'core_paged_store_mxcsr',
  x87: 'core_paged_x87',
  mmx: 'core_paged_mmx',
  numeric_image: 'core_paged_numeric_image',
  masked_store: 'core_paged_masked_store',
  clflush: 'core_paged_clflush',
  protection: 'core_paged_protection',
  far_control: 'core_paged_far_control',
  compound_stack: 'core_paged_compound_stack',
  software_event: 'core_paged_software_event',
  access_failed: 'core_paged_access_failed',
  segmented_access_failed: 'core_paged_segmented_access_failed',
  access_reason: 'core_paged_access_reason',
  debug_begin: 'core_paged_debug_begin',
  debug_finish: 'core_paged_debug_finish',
} as const;

const PAGED_OBSERVED_HELPERS = {
  load: 'core_paged_load_observed',
  load_for_write: 'core_paged_load_for_write_observed',
  store: 'core_paged_store_observed',
  segmented_load: 'core_paged_segmented_load_observed',
  segmented_load_for_write: 'core_paged_segmented_load_for_write_observed',
  segmented_store: 'core_paged_segmented_store_observed',
  vector_load: 'core_paged_vector_load_observed',
  vector_store: 'core_paged_vector_store_observed',
  load_mxcsr: 'core_paged_load_mxcsr_observed',
  store_mxcsr: 'core_paged_store_mxcsr_observed',
  x87: 'core_paged_x87_observed',
  mmx: 'core_paged_mmx_observed',
  numeric_image: 'core_paged_numeric_image_observed',
  masked_store: 'core_paged_masked_store_observed',
  protection: 'core_paged_protection_observed',
  far_control: 'core_paged_far_control_observed',
  compound_stack: 'core_paged_compound_stack_observed',
  software_event: 'core_paged_software_event_observed',
} as const satisfies {
  [Helper in keyof typeof PAGED_HELPERS]?: `${(typeof PAGED_HELPERS)[Helper]}_observed`;
};

const OPTIONAL_PAGED_IMPORTS = [
  'paged_loop_guard',
  'paged_walk_guard',
  'paged_walk_loop_guard32',
  'paged_read_loop_guard32',
  'paged_sse_loop_guard',
  'paged_sse_write_loop_guard32',
  'paged_write_loop_guard32',
  'paged_loop_read_dword',
  'paged_loop_walk_dword',
  'paged_loop_read_dword_window',
  'paged_loop_read_ready',
  'paged_loop_split_read_ready',
  'paged_loop_read_block',
  'paged_loop_block_host',
  'paged_loop_write_block',
  'paged_loop_copy_window',
  'paged_loop_block_window',
  'paged_loop_retouchable_block_window',
  'paged_loop_retouch_block_window',
  'paged_store_fill',
  'paged_loop_write_ready',
  'paged_loop_read_byte',
  'paged_loop_read_pair',
  'paged_loop_read_split_pair',
  'paged_loop_read_span',
  'paged_loop_read_span_pages',
  'paged_loop_read_span_ready',
  'paged_loop_write_byte',
  'paged_loop_write_byte_after_read',
  'paged_loop_write_dword',
  'paged_loop_indexed_dword_write',
  'write_segment_base',
] as const;

const optionalPagedBindings = OPTIONAL_PAGED_IMPORTS.map((name) => [name, `core_${name}`] as const);
const pagedBindings = Object.entries(PAGED_HELPERS);
const observedPagedBindings = Object.entries({ ...PAGED_HELPERS, ...PAGED_OBSERVED_HELPERS });

export const requiredPagedArtifactExports = [
  ...Object.values(PAGED_HELPERS),
  ...Object.values(PAGED_OBSERVED_HELPERS),
];

export function bindArtifactImports(
  exports: CoreExports,
  { observed, paged, mmio }: { observed: boolean; paged: boolean; mmio: boolean },
): Record<string, WebAssembly.ImportValue> {
  const operands = observed
    ? {
        core_io: exports.core_io_observed,
        core_load: exports.core_load_observed,
        core_load_for_write: exports.core_load_for_write_observed,
        core_store: exports.core_store_observed,
        core_segmented_load: exports.core_segmented_load_observed,
        core_segmented_load_for_write: exports.core_segmented_load_for_write_observed,
        core_segmented_store: exports.core_segmented_store_observed,
        core_vector_load: exports.core_vector_load_observed,
        core_vector_store: exports.core_vector_store_observed,
        core_load_mxcsr: exports.core_load_mxcsr_observed,
        core_store_mxcsr: exports.core_store_mxcsr_observed,
        core_x87: exports.core_x87_observed,
        core_mmx: exports.core_mmx_observed,
        core_numeric_image: exports.core_numeric_image_observed,
        core_masked_store: exports.core_masked_store_observed,
        core_protection: exports.core_protection_observed,
        core_far_control: exports.core_far_control_observed,
        core_compound_stack: exports.core_compound_stack_observed,
        core_software_event: exports.core_software_event_observed,
      }
    : exports;
  const env: Record<string, WebAssembly.ImportValue> = {
    memory: exports.memory,
    io: operands.core_io,
    load: operands.core_load,
    load_for_write: operands.core_load_for_write,
    store: operands.core_store,
    access_failed: exports.core_access_failed,
    segmented_load: operands.core_segmented_load,
    segmented_load_for_write: operands.core_segmented_load_for_write,
    segmented_store: operands.core_segmented_store,
    segmented_access_failed: exports.core_segmented_access_failed,
    access_reason: exports.core_access_reason,
    block16_for_write: paged
      ? exports.core_paged_block16_for_write!
      : exports.core_block16_for_write,
    vector_load: operands.core_vector_load,
    vector_store: operands.core_vector_store,
    float: exports.core_float,
    load_mxcsr: operands.core_load_mxcsr,
    store_mxcsr: operands.core_store_mxcsr,
    x87: operands.core_x87,
    mmx: operands.core_mmx,
    numeric_image: operands.core_numeric_image,
    masked_store: operands.core_masked_store,
    clflush: exports.core_clflush,
    protection: operands.core_protection,
    far_control: operands.core_far_control,
    compound_stack: operands.core_compound_stack,
    software_event: operands.core_software_event,
    flags: exports.core_flags,
    debug_step: exports.core_debug_step,
    debug_finish: exports.core_debug_finish,
    debug_register: exports.core_debug_register,
    paging_control: exports.core_paging_control,
    invpcid: exports.core_invpcid,
    processor: exports.core_processor,
    protection_key: exports.core_protection_key,
    debug_begin: exports.core_debug_begin,
    external_step: exports.core_external_step,
    halt: exports.core_halt,
    target_check: exports.core_target_check,
    multiply_high: exports.core_multiply_high,
    divide: exports.core_divide,
    divide_failed: exports.core_divide_failed,
  };
  if (exports.core_read_span) env.read_span = exports.core_read_span;
  if (exports.core_write_span) env.write_span = exports.core_write_span;
  if (mmio && exports.core_mmio_operand) env.mmio_operand = exports.core_mmio_operand;
  if (paged) {
    env.fast_system = exports.core_fast_system;
    for (const [name, key] of optionalPagedBindings) {
      const helper = exports[key];
      if (helper) env[name] = helper;
    }
    if (exports.core_paged_loop_read_byte_for_store) {
      env.paged_loop_read_byte_for_store = exports.core_paged_loop_read_byte_for_store;
      env.paged_loop_read_byte_write_pointer =
        exports.core_paged_loop_read_byte_write_pointer ??
        exports.core_paged_loop_read_byte_for_store;
    }
    for (const [name, key] of observed ? observedPagedBindings : pagedBindings)
      env[name] = exports[key]!;
  }
  return env;
}
