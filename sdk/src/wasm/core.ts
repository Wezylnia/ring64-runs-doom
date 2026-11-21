import { IoBridge, type IoBinding } from '../embedding/io.js';
import { byteLength, U64_MAX, unsigned64 } from '../embedding/input.js';
import { MmioBridge, type MmioBinding, unconfiguredMmioImports } from '../embedding/mmio.js';
import {
  captureMxcsr,
  captureState,
  captureXmmState,
  type Registers,
  type State,
  type StateSnapshot,
} from '../state/execution/registers.js';
import {
  captureNumericFault,
  readNumericSource,
  writeNumericSource,
  validateNumericSourceLayout,
  type NumericFault,
} from '../state/numeric/numeric-controls.js';
import {
  captureDebugState,
  readDebugSource,
  writeDebugSource,
  readDebugContinuation,
  writeDebugContinuation,
  validateDebugLayout,
  type DebugState,
} from '../state/execution/debug.js';
import {
  contextFromJson,
  type ContextInfo,
  type ExecutionContext,
  type ExecutionContextSnapshot,
  type SegmentFault,
  type SegmentName,
} from '../state/system/context.js';
import type { InitialMemory, MemoryBackingInput, MemoryMapping } from '../embedding/memory.js';
import {
  capturePagingState,
  pagingStateFromJson,
  type PagingState,
} from '../state/system/paging.js';
import { captureTimeState, validateTimeLayout, type TimeState } from '../state/system/time.js';
import {
  captureProtectionKeysState,
  validateProtectionKeysLayout,
  type ProtectionKeysState,
} from '../state/system/protection-keys.js';
import {
  captureFastSystemState,
  fastSystemStateFromJson,
  zeroFastSystemState,
  type FastSystemState,
} from '../state/system/fast-system.js';
import {
  eventKind,
  readEvent,
  readInterrupts,
  writePagingSource,
  validatePagingSourceLayout,
  validateEventLayout,
  validateInterruptLayout,
  writeEvent,
  writeInterrupts,
  type EventState,
} from '../state/execution/events.js';
import {
  captureX87State,
  readX87,
  writeX87,
  validateX87Layout,
  type X87State,
  type X87Exception,
} from '../state/numeric/x87.js';

import type { CoreExports, CoreStateLayout, GeneratedRun } from './abi.js';

// The standard Module declaration is structural. The intersection excludes primitives.
export type CompilerSource = Uint8Array | (WebAssembly.Module & object);

export interface DebugException {
  vector: 1;
  class: 'trap' | 'fault';
  cause:
    | 'single-step'
    | 'execution-breakpoint'
    | 'general-detect'
    | 'data-breakpoint'
    | 'combined'
    | 'task-switch';
  breakpoints?: number;
  singleStep?: boolean;
  deferredOriginPc?: bigint;
  instructionPc: bigint;
  resumePc: bigint;
  instructionCompleted: boolean;
  resumeFlags: bigint;
}

export interface Fault {
  segment: SegmentName;
  instructionPc: bigint;
  address: bigint;
  width: number;
  access: 'read' | 'write' | 'execute';
  cause: 'address-range' | 'noncanonical' | 'unmapped' | 'permission';
  failingAddress: bigint;
}

export interface AlignmentFault {
  segment: SegmentName;
  instructionPc: bigint;
  offset: bigint;
  address: bigint;
  width: number;
  access: 'read' | 'write';
  alignment: number;
}

export interface AlignmentCheck extends AlignmentFault {
  vector: 17;
  errorCode: 0 | 1;
}

export interface SimdException {
  instructionPc: bigint;
  vector: 19;
  stage: 'pre-computation' | 'post-computation';
  raised: number;
  unmasked: number;
  mxcsrBefore: number;
  mxcsrAfter: number;
}

export interface GeneralProtectionFault {
  instructionPc: bigint;
  vector: 13;
  errorCode: 0;
  cause: 'mxcsr-reserved-bits';
  loadedValue: number;
}

export interface ProtectionFault {
  instructionPc: bigint;
  vector: 10 | 11 | 12 | 13;
  errorCode: number;
  phase: 'privilege' | 'selector' | 'descriptor' | 'target';
  selector?: number;
}

interface SystemMemoryAccess {
  instructionPc: bigint;
  address: bigint;
  width: number;
  access: 'read' | 'write';
  cause: 'address-range' | 'noncanonical' | 'unmapped' | 'permission';
  failingAddress: bigint;
}
type SystemMemoryReference =
  | {
      table: 'gdt' | 'ldt';
      selector: number;
      phase: 'descriptor-read' | 'descriptor-accessed-write' | 'descriptor-busy-write';
    }
  | { table: 'ivt' | 'idt'; vector: number; phase: 'gate-read' }
  | { table: 'tss'; selector: number; phase: 'tss-read' | 'tss-write' };
export type SystemMemoryFault = SystemMemoryAccess & SystemMemoryReference;

export interface PageFault {
  instructionPc: bigint;
  vector: 14;
  table?: 'gdt' | 'ldt' | 'idt' | 'tss';
  systemPhase?: string;
  selector?: number;
  referenceVector?: number;
  address: bigint;
  access: 'read' | 'write' | 'execute';
  errorCode: number;
}

export interface PhysicalAccessFault {
  instructionPc: bigint;
  address: bigint;
  access: 'read' | 'write' | 'execute';
  physicalAddress: bigint;
  width: number;
  failingPhysicalAddress: bigint;
  physicalAccess: 'read' | 'write' | 'execute';
  cause: 'unmapped' | 'permission' | 'address-range';
  phase: 'walk-read' | 'accessed-write' | 'dirty-write' | 'final-access';
  segment?: SegmentName;
  table?: 'gdt' | 'ldt' | 'idt' | 'tss';
  systemPhase?: string;
  selector?: number;
  referenceVector?: number;
}

export interface PdpteLoadFault {
  instructionPc: bigint;
  phase: 'pdpte-load';
  address?: never;
  access?: never;
  segment?: never;
  table?: never;
  systemPhase?: never;
  selector?: never;
  referenceVector?: never;
  control: 'cr0' | 'cr3' | 'cr4';
  slot: 0 | 1 | 2 | 3;
  physicalAddress: bigint;
  width: 8;
  failingPhysicalAddress: bigint;
  physicalAccess: 'read';
  cause: 'unmapped' | 'permission' | 'address-range';
}
export type PhysicalMemoryFault = PhysicalAccessFault | PdpteLoadFault;

export interface Span {
  base: bigint;
  bytes: Uint8Array;
}

export interface CompileLimits {
  maxInstructions: number;
  maxSourceBytes: number;
  maxArtifactBytes: number;
}

export interface UnitMetadata {
  // Array positions are the generated entry indices supplied by Rust.
  entries: bigint[];
  instructionCount: number;
  blockCount: number;
  sourceBytes: number;
  owner: bigint;
  contextVersion: bigint;
  boundaryObserved?: boolean;
  dependencies: { start: bigint; length: number }[];
  byteLength: number;
}

export function backingKey(name: string): string {
  // JSON's Unicode strings cannot carry lone UTF-16 surrogates into Rust.
  let key = '';
  for (let index = 0; index < name.length; index++)
    key += name.charCodeAt(index).toString(16).padStart(4, '0');
  return key;
}

export function mappingInput(mappings: readonly MemoryMapping[]): MemoryMapping[] {
  return mappings.map((mapping) => ({ ...mapping, backing: backingKey(mapping.backing) }));
}

export function writeInput(
  exports: CoreExports,
  metadata: unknown,
  backings: readonly Uint8Array[] = [],
): number {
  const header = new TextEncoder().encode(
    JSON.stringify(metadata, (_key, value: unknown) =>
      typeof value === 'bigint' ? `0x${value.toString(16)}` : value,
    ),
  );
  let length = header.length;
  for (const bytes of backings) length += bytes.length;
  if (length > 0xffffffff) throw new RangeError('mapped input exceeds wasm32 capacity');
  // Wasm i32 results carry unsigned wasm32 pointers as signed JS numbers.
  const pointer = exports.core_input(length) >>> 0;
  const input = new Uint8Array(exports.memory.buffer, pointer, length);
  input.set(header);
  let offset = header.length;
  for (const bytes of backings) {
    input.set(bytes, offset);
    offset += bytes.length;
  }
  return header.length;
}

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

export function captureMemoryProfile(
  memory: Pick<InitialMemory, 'memoryModel' | 'paging' | 'fastSystem'>,
  context: ExecutionContext | undefined,
) {
  const memoryModel = memory.memoryModel ?? 'linear';
  const isPaged = memoryModel !== 'linear';
  const physicalControls = memoryModel === 'physical';
  const paging = isPaged ? capturePagingState(memory.paging!) : undefined;
  const fastSystem = isPaged
    ? captureFastSystemState(memory.fastSystem ?? zeroFastSystemState())
    : undefined;
  return {
    isPaged,
    physicalControls,
    metadata: isPaged
      ? { memoryModel, paging, fastSystem, ...(physicalControls ? { context } : {}) }
      : {},
  };
}

export function instantiateArtifact(
  module: WebAssembly.Module,
  env: Record<string, WebAssembly.ImportValue>,
): GeneratedRun {
  const instance = new WebAssembly.Instance(module, { env });
  if (typeof instance.exports.run !== 'function')
    throw new Error('generated module must export run');
  return instance.exports.run as GeneratedRun;
}

export class Core {
  #taskRetry: 'instruction' | 'delivery' | undefined;
  readonly takeSourceInvalidation?: () => boolean;

  get taskRetry() {
    return this.#taskRetry;
  }
  readonly #exports: CoreExports;
  readonly #layout: CoreStateLayout;
  readonly #statePointer: number;
  #stateDataView?: DataView;
  #standaloneStateBuffer: ArrayBuffer | undefined;

  protected constructor(
    exports: CoreExports,
    readonly isPaged = false,
    readonly physicalControls = false,
    private readonly io?: IoBridge,
    private readonly mmio?: MmioBridge,
  ) {
    this.#exports = exports;
    const takeSourceInvalidation = exports.core_take_source_invalidation;
    if (typeof takeSourceInvalidation === 'function') {
      this.takeSourceInvalidation = () => takeSourceInvalidation() !== 0;
    }
    this.#layout = this.info<CoreStateLayout>();
    this.#statePointer = exports.core_state_ptr() >>> 0;
    for (const name of [
      'size',
      'scalarSize',
      'xmm',
      'xmmBytes',
      'vectorScratch',
      'mxcsr',
      'mxcsrBytes',
      'registers',
      'flags',
      'rip',
      'remaining',
      'repeatRemaining',
      'interruptibility',
      'interruptibilityBytes',
      'deliveryEnabled',
    ] as const) {
      if (
        !Number.isSafeInteger(this.#layout[name]) ||
        this.#layout[name] < 0 ||
        this.#layout[name] > 0xffffffff
      ) {
        throw new Error('core returned an invalid transport layout');
      }
    }
    if (
      !Array.isArray(this.#layout.reasons) ||
      this.#layout.reasons.some((reason) => typeof reason !== 'string')
    ) {
      throw new Error('core returned invalid exit names');
    }
    const layout = this.#layout;
    if (
      layout.scalarSize !== 176 ||
      layout.registers !== 0 ||
      layout.flags !== 128 ||
      layout.rip !== 136 ||
      layout.remaining !== 144 ||
      layout.repeatRemaining !== 168 ||
      layout.interruptibility !== 164 ||
      layout.interruptibilityBytes !== 4 ||
      layout.xmm < layout.scalarSize ||
      layout.xmmBytes !== 256 ||
      layout.xmm + layout.xmmBytes > layout.size ||
      layout.vectorScratch < layout.xmm + layout.xmmBytes ||
      layout.vectorScratch + 16 > layout.size ||
      layout.mxcsr < 448 ||
      layout.mxcsr < layout.vectorScratch + 16 ||
      layout.mxcsrBytes !== 4 ||
      layout.mxcsr + layout.mxcsrBytes > layout.size
    )
      throw new Error('core returned an incompatible transport layout');
    validateX87Layout(layout.x87, layout.size, layout.mxcsr + layout.mxcsrBytes);
    validateEventLayout(layout.event, layout.size, layout.x87.offset + layout.x87.bytes);
    if (layout.deliveryEnabled < layout.event.offset + layout.event.bytes)
      throw new Error('core returned an overlapping delivery control');
    validateInterruptLayout(layout.interrupts, layout.size, layout.deliveryEnabled + 4);
    validateDebugLayout(
      layout.debug,
      layout.size,
      layout.interrupts.offset + layout.interrupts.bytes,
    );
    if (layout.pagingSource !== undefined)
      validatePagingSourceLayout(
        layout.pagingSource,
        layout.size,
        layout.debug.offset + layout.debug.layout.bytes,
      );
    if (isPaged && layout.pagingSource === undefined)
      throw new Error('paged core must describe its paging source transport');
    if (layout.numericSource !== undefined) {
      validateNumericSourceLayout(
        layout.numericSource,
        layout.size,
        (layout.pagingSource?.offset ?? layout.debug.offset + layout.debug.layout.bytes) +
          (layout.pagingSource?.bytes ?? 0),
      );
      if (typeof exports.core_numeric_fault_info !== 'function')
        throw new Error('core omitted numeric source export');
    }
    validateTimeLayout(
      layout.time,
      layout.size,
      layout.numericSource
        ? layout.numericSource.offset + layout.numericSource.bytes
        : layout.pagingSource
          ? layout.pagingSource.offset + layout.pagingSource.bytes
          : layout.debug.offset + layout.debug.layout.bytes,
    );
    validateProtectionKeysLayout(
      layout.protectionKeys,
      layout.size,
      layout.time.offset + layout.time.size,
    );
    if (
      layout.xcr0 !== undefined &&
      (!Number.isSafeInteger(layout.xcr0) ||
        layout.xcr0 < layout.protectionKeys.offset + layout.protectionKeys.size ||
        layout.xcr0 + 8 > layout.size)
    )
      throw new Error('core returned an invalid XCR0 offset');
    this.stateView();
  }

  static async create(
    compiler: CompilerSource,
    memory: InitialMemory,
    context?: ExecutionContext,
    ioBinding?: IoBinding,
    mmioBinding?: MmioBinding,
  ): Promise<Core> {
    const { isPaged, physicalControls, metadata } = captureMemoryProfile(memory, context);
    let module: WebAssembly.Module;
    if (compiler instanceof WebAssembly.Module) module = compiler;
    else {
      if (!(compiler instanceof Uint8Array))
        throw new TypeError('compiler must be bytes or a WebAssembly.Module');
      module = await WebAssembly.compile(new Uint8Array(compiler));
    }
    const io = new IoBridge(ioBinding);
    const mmio = mmioBinding && new MmioBridge(mmioBinding, io);
    try {
      const instance = await WebAssembly.instantiate(module, {
        io: io.imports,
        mmio: mmio?.imports ?? unconfiguredMmioImports,
      });
      const exports = instance.exports as CoreExports;
      Core.validateExports(exports, isPaged, physicalControls);
      if (mmio) {
        if (
          typeof exports.core_mmio_effect_active !== 'function' ||
          typeof exports.core_mmio_operand !== 'function'
        )
          throw new TypeError('compiler does not support MMIO');
        mmio.effectActive = exports.core_mmio_effect_active;
      }
      const metadataLength = writeInput(
        exports,
        {
          ...metadata,
          memoryLimits: memory.memoryLimits,
          backings: memory.backings.map(({ id, bytes }) => ({
            id: backingKey(id),
            length: bytes.length,
          })),
          mappings: mappingInput(memory.mappings),
          ...(mmioBinding ? { mmioRanges: mmioBinding.ranges } : {}),
        },
        memory.backings.map(({ bytes }) => bytes),
      );
      return Core.initialize(
        exports,
        memory.entry,
        metadataLength,
        io,
        physicalControls ? undefined : context,
        () => new Core(exports, isPaged, physicalControls, io, mmio),
      );
    } catch (error) {
      io.release();
      mmio?.release();
      throw error;
    }
  }

  protected static validateExports(
    exports: CoreExports,
    isPaged: boolean,
    physicalControls: boolean,
  ): void {
    if (!(exports.memory instanceof WebAssembly.Memory)) throw new Error('core must export memory');
    for (const name of [
      'core_io',
      'core_io_observed',
      'core_io_configure',
      'core_io_effect_active',
      'core_input',
      'core_init_mapped',
      'core_add_backing',
      'core_remove_backing',
      'core_set_mappings',
      'core_validity',
      'core_activate',
      'core_publish',
      'core_release',
      'core_context_info',
      'core_context_version',
      'core_boundary_observed',
      'core_set_context',
      'core_segmented_load',
      'core_segmented_load_for_write',
      'core_segmented_store',
      'core_segmented_access_failed',
      'core_access_reason',
      'core_target_check',
      'core_state_ptr',
      'core_import_state',
      'core_load_observed',
      'core_load_for_write_observed',
      'core_store_observed',
      'core_segmented_load_observed',
      'core_segmented_load_for_write_observed',
      'core_segmented_store_observed',
      'core_vector_load_observed',
      'core_vector_store_observed',
      'core_load_mxcsr_observed',
      'core_store_mxcsr_observed',
      'core_x87_observed',
      'core_mmx_observed',
      'core_numeric_image_observed',
      'core_masked_store_observed',
      'core_protection_observed',
      'core_far_control_observed',
      'core_compound_stack_observed',
      'core_software_event_observed',
      'core_debug_finish',
      'core_debug_begin',
      'core_debug_register',
      'core_import_debug',
      'core_import_xmm',
      'core_import_mxcsr',
      'core_import_x87',
      'core_x87',
      'core_mmx',
      'core_numeric_image',
      'core_masked_store',
      'core_clflush',
      'core_protection',
      'core_far_control',
      'core_compound_stack',
      'core_software_event',
      'core_flags',
      'core_debug_step',
      'core_configure_delivery',
      'core_stage_exception',
      'core_deliver_exception',
      'core_request_interrupt',
      'core_request_nmi',
      'core_external_boundary',
      'core_shutdown_can_wake',
      'core_external_step',
      'core_halt',
      'core_float',
      'core_load_mxcsr',
      'core_store_mxcsr',
      'core_vector_load',
      'core_vector_store',
      'core_clear_fault',
      'core_processor',
      'core_protection_key',
      'core_probe',
      'core_compile',
      'core_load',
      'core_load_for_write',
      'core_store',
      'core_access_failed',
      'core_multiply_high',
      'core_divide',
      'core_divide_failed',
      'core_fault_info',
      'core_read_memory',
      'core_write_memory',
      'core_output_ptr',
      'core_output_len',
      'core_info_ptr',
      'core_info_len',
      'core_dispose',
    ]) {
      if (typeof exports[name] !== 'function') throw new Error(`core must export ${name}`);
    }
    if (physicalControls && typeof exports.core_reset !== 'function')
      throw new Error('core must export core_reset');
    if (isPaged)
      for (const name of [
        'core_paging_state',
        'core_import_paging',
        'core_paging_control',
        'core_invpcid',
        'core_fast_system_state',
        'core_import_fast_system',
        'core_fast_system',
        ...Object.values(PAGED_HELPERS),
        ...Object.values(PAGED_OBSERVED_HELPERS),
      ])
        if (typeof exports[name] !== 'function') throw new Error(`core must export ${name}`);
  }

  protected static initialize<CpuCore extends Core>(
    exports: CoreExports,
    entry: bigint,
    metadataLength: number,
    io: IoBridge,
    context: ExecutionContext | undefined,
    construct: () => CpuCore,
  ): CpuCore {
    const status = exports.core_init_mapped(entry, metadataLength);
    if (status !== 0) {
      const text = new TextDecoder().decode(
        new Uint8Array(
          exports.memory.buffer,
          exports.core_info_ptr() >>> 0,
          exports.core_info_len() >>> 0,
        ),
      );
      exports.core_dispose();
      const error = (JSON.parse(text) as { error: string }).error;
      if (status === 1) throw new TypeError(error);
      if (status === 2) throw new RangeError(error);
      throw new Error('mapped initialization returned an invalid status');
    }
    try {
      if (exports.core_io_configure(Number(io.present)) !== 0)
        throw new Error('core refused I/O configuration');
      io.effectActive = exports.core_io_effect_active;
      const core = construct();
      if (context !== undefined) core.setContext(context);
      return core;
    } catch (error) {
      exports.core_dispose();
      throw error;
    }
  }

  protected get interruptLayout() {
    return this.#layout.interrupts;
  }

  protected setExitReason(value: number, reason: string): void {
    this.#layout.reasons[value] = reason;
  }

  private info<T>(): T {
    const bytes = new Uint8Array(
      this.#exports.memory.buffer,
      this.#exports.core_info_ptr() >>> 0,
      this.#exports.core_info_len() >>> 0,
    );
    return JSON.parse(new TextDecoder().decode(bytes)) as T;
  }

  protected output(): Uint8Array {
    return new Uint8Array(
      this.#exports.memory.buffer,
      this.#exports.core_output_ptr() >>> 0,
      this.#exports.core_output_len() >>> 0,
    ).slice();
  }

  protected stateView(): DataView {
    let view = this.#stateDataView;
    // Growing non-shared memory detaches its fixed buffer, even for grow(0).
    if (view !== undefined && this.#standaloneStateBuffer?.byteLength) return view;
    const buffer = this.#exports.memory.buffer;
    if (view === undefined || view.buffer !== buffer) {
      view = new DataView(buffer, this.#statePointer, this.#layout.size);
      this.#stateDataView = view;
      this.#standaloneStateBuffer = buffer instanceof ArrayBuffer ? buffer : undefined;
    }
    return view;
  }

  snapshot(): StateSnapshot {
    const view = this.stateView();
    const interruptibility = view.getUint32(this.#layout.interruptibility, true);
    const event = readEvent(
      view,
      this.#layout.event,
      this.#layout.event.offset,
      this.#layout.pagingSource,
    );
    const d = this.#layout.debug,
      raw = d.layout;
    const source = readDebugSource(view, raw.source, d.offset + raw.sourceOffset);
    if (source) {
      if (event.kind !== 'pending' || event.exception.vector !== 1)
        throw new Error('core returned an incompatible debug source');
      event.exception.debug = source;
    }
    const numeric = this.#layout.numericSource
      ? readNumericSource(view, this.#layout.numericSource)
      : undefined;
    if (numeric) {
      if (
        event.kind !== 'pending' ||
        event.exception.vector !== numeric.vector ||
        event.exception.instructionPc !== numeric.instructionPc ||
        event.exception.returnPc !== numeric.instructionPc ||
        source
      )
        throw new Error('core returned incompatible numeric source');
      if (numeric.vector === 7) event.exception.deviceNotAvailable = numeric;
      else event.exception.numericControlFault = numeric;
    }
    const debugContinuation = readDebugContinuation(
      view,
      raw.continuation,
      d.offset + raw.continuationOffset,
    );
    return {
      ...(debugContinuation ? { debugContinuation } : {}),
      ...(this.#taskRetry ? { taskSwitchIncomplete: true as const } : {}),
      registers: Array.from({ length: 16 satisfies Registers['length'] }, (_, i) =>
        view.getBigUint64(this.#layout.registers + i * 8, true),
      ) as Registers,
      flags: view.getBigUint64(this.#layout.flags, true),
      rip: view.getBigUint64(this.#layout.rip, true),
      interruptibility: {
        sti: (interruptibility & 1) !== 0,
        ss: (interruptibility & 2) !== 0,
      },
      event,
      interrupts: readInterrupts(view, this.#layout.interrupts, this.#layout.interrupts.offset),
    };
  }

  setState(state: State): void {
    const input = captureState(state);
    if (input.event.kind === 'pending' && input.event.exception.vector === 14 && !this.isPaged)
      throw new TypeError('page fault source requires ia32e-paged memory');
    const baseSize =
      this.#layout.scalarSize + this.#layout.event.bytes + this.#layout.interrupts.bytes;
    const debug = this.#layout.debug.layout;
    const oldSize = baseSize + debug.source.bytes + debug.continuation.bytes;
    const pagingSource = this.#layout.pagingSource;
    const numericSource = this.#layout.numericSource;
    const size = oldSize + (pagingSource?.bytes ?? 0) + (numericSource?.bytes ?? 0);
    const numeric =
      input.event.kind === 'pending'
        ? (input.event.exception.deviceNotAvailable ?? input.event.exception.numericControlFault)
        : undefined;
    if (numeric && !numericSource) throw new TypeError('core does not support numeric source');
    const pointer = this.#exports.core_input(size) >>> 0;
    const view = new DataView(this.#exports.memory.buffer, pointer, size);
    for (let index = 0; index < 16; index++) {
      view.setBigUint64(this.#layout.registers + index * 8, input.registers[index]!, true);
    }
    view.setBigUint64(this.#layout.flags, input.flags, true);
    view.setBigUint64(this.#layout.rip, input.rip, true);
    view.setUint32(
      this.#layout.interruptibility,
      Number(input.interruptibility.sti) | (Number(input.interruptibility.ss) << 1),
      true,
    );
    writeEvent(
      new DataView(
        view.buffer,
        view.byteOffset + this.#layout.scalarSize,
        this.#layout.event.bytes,
      ),
      this.#layout.event,
      input.event,
    );
    writeInterrupts(
      new DataView(
        view.buffer,
        view.byteOffset + this.#layout.scalarSize + this.#layout.event.bytes,
        this.#layout.interrupts.bytes,
      ),
      this.#layout.interrupts,
      input.interrupts,
    );
    writeDebugSource(
      new DataView(view.buffer, view.byteOffset + baseSize, debug.source.bytes),
      debug.source,
      input.event.kind === 'pending' ? input.event.exception.debug : undefined,
    );
    writeDebugContinuation(
      new DataView(
        view.buffer,
        view.byteOffset + baseSize + debug.source.bytes,
        debug.continuation.bytes,
      ),
      debug.continuation,
      input.debugContinuation,
    );
    if (pagingSource)
      writePagingSource(
        new DataView(view.buffer, view.byteOffset + oldSize, pagingSource.bytes),
        pagingSource,
        input.event.kind === 'pending' && input.event.exception.vector === 14
          ? input.event.exception.pageFaultAddress
          : undefined,
      );
    if (numericSource)
      writeNumericSource(
        new DataView(
          view.buffer,
          view.byteOffset + oldSize + (pagingSource?.bytes ?? 0),
          numericSource.bytes,
        ),
        numericSource,
        numeric,
      );
    if (this.#exports.core_import_state() !== 0)
      throw new TypeError(this.info<{ error: string }>().error);
    this.#taskRetry = undefined;
  }

  readDebugState(): DebugState {
    const view = this.stateView(),
      d = this.#layout.debug,
      l = d.layout;
    return {
      addresses: Array.from({ length: 4 }, (_, i) =>
        view.getBigUint64(d.offset + l.addresses + i * 8, true),
      ),
      dr6: view.getBigUint64(d.offset + l.dr6, true),
      dr7: view.getBigUint64(d.offset + l.dr7, true),
    };
  }
  setDebugState(state: DebugState): void {
    const input = captureDebugState(state);
    const d = this.#layout.debug.layout;
    const pointer = this.#exports.core_input(d.sourceOffset) >>> 0;
    const view = new DataView(this.#exports.memory.buffer, pointer, d.sourceOffset);
    input.addresses.forEach((a, i) => view.setBigUint64(d.addresses + i * 8, a, true));
    view.setBigUint64(d.dr6, input.dr6, true);
    view.setBigUint64(d.dr7, input.dr7, true);
    if (this.#exports.core_import_debug() !== 0)
      throw new TypeError(this.info<{ error: string }>().error);
  }
  readPagingState(): PagingState {
    if (!this.isPaged) throw new TypeError('method requires ia32e-paged memory');
    if (this.#exports.core_paging_state() !== 0)
      throw new TypeError(this.info<{ error: string }>().error);
    return pagingStateFromJson(this.info<unknown>());
  }
  setPagingState(state: PagingState): void {
    if (!this.isPaged) throw new TypeError('method requires ia32e-paged memory');
    writeInput(this.#exports, capturePagingState(state));
    if (this.#exports.core_import_paging() !== 0)
      throw new TypeError(this.info<{ error: string }>().error);
    this.#taskRetry = undefined;
  }
  readFastSystemState(): FastSystemState {
    if (!this.isPaged) throw new TypeError('method requires ia32e-paged memory');
    if (this.#exports.core_fast_system_state() !== 0)
      throw new TypeError(this.info<{ error: string }>().error);
    return fastSystemStateFromJson(this.info<unknown>());
  }
  setFastSystemState(state: FastSystemState): void {
    if (!this.isPaged) throw new TypeError('method requires ia32e-paged memory');
    writeInput(this.#exports, captureFastSystemState(state));
    if (this.#exports.core_import_fast_system() !== 0)
      throw new TypeError(this.info<{ error: string }>().error);
  }
  readTimeState(): TimeState {
    const view = this.stateView(),
      layout = this.#layout.time;
    return {
      tsc: view.getBigUint64(layout.offset + layout.tsc, true),
      tscAux: view.getUint32(layout.offset + layout.tscAux, true),
    };
  }
  setTimeState(state: TimeState): void {
    const input = captureTimeState(state),
      view = this.stateView(),
      layout = this.#layout.time;
    view.setBigUint64(layout.offset + layout.tsc, input.tsc, true);
    view.setUint32(layout.offset + layout.tscAux, input.tscAux, true);
  }
  readXcr0(): bigint {
    if (this.#layout.xcr0 === undefined) throw new TypeError('core lacks XCR0');
    return this.stateView().getBigUint64(this.#layout.xcr0, true);
  }
  setXcr0(value: bigint): void {
    if (this.#layout.xcr0 === undefined) throw new TypeError('core lacks XCR0');
    if (typeof value !== 'bigint' || (value & ~0x203n) !== 0n || (value & 1n) === 0n)
      throw new TypeError('XCR0 requires bit 0 and supports bits 0, 1 and 9');
    this.stateView().setBigUint64(this.#layout.xcr0, value, true);
  }
  readProtectionKeysState(): ProtectionKeysState {
    const view = this.stateView(),
      layout = this.#layout.protectionKeys;
    return {
      pkru: view.getUint32(layout.offset + layout.pkru, true),
      pkrs: view.getUint32(layout.offset + layout.pkrs, true),
    };
  }
  setProtectionKeysState(state: ProtectionKeysState): void {
    const input = captureProtectionKeysState(state),
      view = this.stateView(),
      layout = this.#layout.protectionKeys;
    view.setUint32(layout.offset + layout.pkru, input.pkru, true);
    view.setUint32(layout.offset + layout.pkrs, input.pkrs, true);
  }
  readXmmState(): bigint[] {
    const view = this.stateView();
    return Array.from({ length: 16 }, (_, index) => {
      const offset = this.#layout.xmm + index * 16;
      return view.getBigUint64(offset, true) | (view.getBigUint64(offset + 8, true) << 64n);
    });
  }

  setXmmState(state: bigint[]): void {
    const bank = captureXmmState(state);
    const pointer = this.#exports.core_input(this.#layout.xmmBytes) >>> 0;
    const view = new DataView(this.#exports.memory.buffer, pointer, this.#layout.xmmBytes);
    for (let index = 0; index < 16; index++) {
      const value = bank[index]!;
      view.setBigUint64(index * 16, value & U64_MAX, true);
      view.setBigUint64(index * 16 + 8, value >> 64n, true);
    }
    if (this.#exports.core_import_xmm() !== 0)
      throw new TypeError(this.info<{ error: string }>().error);
  }

  readMxcsr(): number {
    return this.stateView().getUint32(this.#layout.mxcsr, true);
  }

  readX87State(): X87State {
    const layout = this.#layout.x87;
    return readX87(
      new DataView(this.#exports.memory.buffer, this.#statePointer + layout.offset, layout.bytes),
      layout,
    );
  }

  setX87State(value: X87State): void {
    const input = captureX87State(value),
      layout = this.#layout.x87;
    const pointer = this.#exports.core_input(layout.bytes) >>> 0;
    writeX87(new DataView(this.#exports.memory.buffer, pointer, layout.bytes), layout, input);
    if (this.#exports.core_import_x87() !== 0)
      throw new TypeError(this.info<{ error: string }>().error);
  }

  setMxcsr(value: number): void {
    const input = captureMxcsr(value);
    const pointer = this.#exports.core_input(this.#layout.mxcsrBytes) >>> 0;
    new DataView(this.#exports.memory.buffer, pointer, this.#layout.mxcsrBytes).setUint32(
      0,
      input,
      true,
    );
    if (this.#exports.core_import_mxcsr() !== 0)
      throw new TypeError(this.info<{ error: string }>().error);
  }

  current(): { rip: bigint; remaining: bigint; repeatRemaining: bigint } {
    const view = this.stateView();
    return {
      rip: view.getBigUint64(this.#layout.rip, true),
      remaining: view.getBigUint64(this.#layout.remaining, true),
      repeatRemaining: view.getBigUint64(this.#layout.repeatRemaining, true),
    };
  }

  private diagnosticPc(): bigint {
    const view = this.stateView();
    const event = this.#layout.event;
    return view.getBigUint64(
      view.getUint32(event.offset + event.kind, true) === 1
        ? event.offset + event.instructionPc
        : this.#layout.rip,
      true,
    );
  }

  clearFault(): void {
    this.#exports.core_clear_fault();
  }
  prepareEntry(): boolean {
    const value = this.#exports.core_clear_fault();
    if (value !== 0 && value !== 1) throw new Error('core returned an invalid boundary profile');
    return value === 1;
  }
  eventKind(): EventState['kind'] {
    return eventKind(
      this.stateView().getUint32(this.#layout.event.offset + this.#layout.event.kind, true),
    );
  }
  hasQueuedInterrupts(): boolean {
    const view = this.stateView();
    const layout = this.#layout.interrupts;
    return (
      view.getUint32(layout.offset + layout.irq, true) !== 0 ||
      view.getUint32(layout.offset + layout.nmiPending, true) !== 0
    );
  }
  configureDelivery(enabled: boolean): void {
    if (this.#exports.core_configure_delivery(Number(enabled)) !== 0)
      throw new Error('core returned an invalid delivery configuration status');
  }
  stageException(reason: string): boolean {
    const id = this.#layout.reasons.indexOf(reason);
    if (id < 0) throw new Error('cannot stage an unknown execution reason');
    const status = this.#exports.core_stage_exception(id);
    if (status !== 0 && status !== 1)
      throw new Error('core returned an invalid exception staging status');
    return status === 1;
  }
  deliverException(): string {
    const status = this.#exports.core_deliver_exception();
    return status < 0 ? this.taskPrivateStatus(status, 'delivery') : this.reason(status);
  }

  private taskPrivateStatus(status: number, source: 'instruction' | 'delivery'): string {
    if (status === -128 - this.#layout.reasons.indexOf('physical-memory-fault')) {
      this.#taskRetry = source;
      return 'physical-memory-fault';
    }
    throw new Error('invalid private task switch status');
  }

  private completeTask(source: 'instruction' | 'delivery'): string | { taskEvent: string } {
    const status = this.#exports.core_complete_task();
    this.#taskRetry = undefined;
    if (status >= 0) return this.reason(status);
    if (status <= -64 && status > -128 && source === 'instruction') {
      const report = this.reason(-64 - status);
      if (
        ![
          'protection-fault',
          'page-fault',
          'segment-fault',
          'debug-exception',
          'alignment-check',
          'system-memory-fault',
          'data-fault',
        ].includes(report) ||
        this.eventKind() !== 'pending'
      )
        throw new Error('invalid incoming task exception');
      return { taskEvent: report };
    }
    return this.taskPrivateStatus(status, source);
  }

  resumeTask(): string | { taskEvent: string } {
    if (!this.#taskRetry) throw new Error('no task switch to resume');
    return this.completeTask(this.#taskRetry);
  }
  requestInterrupt(vector: number): boolean {
    const result = this.#exports.core_request_interrupt(vector);
    if (result !== 0 && result !== 1) throw new Error('core returned an invalid IRQ intake status');
    return result === 0;
  }
  requestNmi(): void {
    if (this.#exports.core_request_nmi() !== 0)
      throw new Error('core returned an invalid NMI intake status');
  }
  externalBoundary(): string {
    const reason = this.reason(this.#exports.core_external_boundary());
    if (!['dispatch', 'pending-interrupt', 'halted', 'shutdown'].includes(reason))
      throw new Error('core returned an invalid external boundary status');
    return reason;
  }
  shutdownCanWake(): boolean {
    const result = this.#exports.core_shutdown_can_wake();
    if (result !== 0 && result !== 1)
      throw new Error('core returned an invalid shutdown wake status');
    return result === 1;
  }
  probe(pc: bigint): string {
    return this.reason(this.#exports.core_probe(pc));
  }

  reason(value: number): string;
  reason(value: number, generated: true): string | { taskEvent: string };
  reason(value: number, generated?: true): string | { taskEvent: string } {
    const reason = this.#layout.reasons[value];
    if (!Number.isInteger(value) || reason === undefined) {
      if (generated && value === -2) return this.completeTask('instruction');
      throw new Error('generated execution returned an invalid reason');
    }
    return reason;
  }

  compile(
    pc: bigint,
    limits: CompileLimits,
  ): { bytes: Uint8Array; metadata: UnitMetadata } | undefined {
    const status = this.#exports.core_compile(
      pc,
      limits.maxInstructions,
      limits.maxSourceBytes,
      limits.maxArtifactBytes,
    );
    if (this.reason(status) === 'resource-limit') return undefined;
    if (status !== 0) throw new Error('translator returned an unexpected result');
    const metadata = this.info<
      Omit<UnitMetadata, 'entries' | 'byteLength' | 'owner' | 'dependencies' | 'contextVersion'> & {
        entries: string[];
        owner: string;
        contextVersion: string;
        dependencies: { start: string; length: number }[];
      }
    >();
    const bytes = this.output();
    if (typeof metadata.boundaryObserved !== 'boolean')
      throw new Error('core returned an invalid boundary profile');
    return {
      bytes,
      metadata: {
        ...metadata,
        entries: metadata.entries.map((pc) => BigInt(pc)),
        owner: BigInt(metadata.owner),
        contextVersion: BigInt(metadata.contextVersion),
        dependencies: metadata.dependencies.map(({ start, length }) => ({
          start: BigInt(start),
          length,
        })),
        byteLength: bytes.length,
      },
    };
  }

  instantiate(module: WebAssembly.Module, observed = false): GeneratedRun {
    return instantiateArtifact(module, this.artifactImports(observed));
  }

  protected artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue> {
    const operands = observed
      ? {
          core_io: this.#exports.core_io_observed,
          core_load: this.#exports.core_load_observed,
          core_load_for_write: this.#exports.core_load_for_write_observed,
          core_store: this.#exports.core_store_observed,
          core_segmented_load: this.#exports.core_segmented_load_observed,
          core_segmented_load_for_write: this.#exports.core_segmented_load_for_write_observed,
          core_segmented_store: this.#exports.core_segmented_store_observed,
          core_vector_load: this.#exports.core_vector_load_observed,
          core_vector_store: this.#exports.core_vector_store_observed,
          core_load_mxcsr: this.#exports.core_load_mxcsr_observed,
          core_store_mxcsr: this.#exports.core_store_mxcsr_observed,
          core_x87: this.#exports.core_x87_observed,
          core_mmx: this.#exports.core_mmx_observed,
          core_numeric_image: this.#exports.core_numeric_image_observed,
          core_masked_store: this.#exports.core_masked_store_observed,
          core_protection: this.#exports.core_protection_observed,
          core_far_control: this.#exports.core_far_control_observed,
          core_compound_stack: this.#exports.core_compound_stack_observed,
          core_software_event: this.#exports.core_software_event_observed,
        }
      : this.#exports;
    const env: Record<string, WebAssembly.ImportValue> = {
      memory: this.#exports.memory,
      io: operands.core_io,
      load: operands.core_load,
      load_for_write: operands.core_load_for_write,
      store: operands.core_store,
      access_failed: this.#exports.core_access_failed,
      segmented_load: operands.core_segmented_load,
      segmented_load_for_write: operands.core_segmented_load_for_write,
      segmented_store: operands.core_segmented_store,
      segmented_access_failed: this.#exports.core_segmented_access_failed,
      access_reason: this.#exports.core_access_reason,
      block16_for_write: this.isPaged
        ? this.#exports.core_paged_block16_for_write!
        : this.#exports.core_block16_for_write,
      vector_load: operands.core_vector_load,
      vector_store: operands.core_vector_store,
      float: this.#exports.core_float,
      load_mxcsr: operands.core_load_mxcsr,
      store_mxcsr: operands.core_store_mxcsr,
      x87: operands.core_x87,
      mmx: operands.core_mmx,
      numeric_image: operands.core_numeric_image,
      masked_store: operands.core_masked_store,
      clflush: this.#exports.core_clflush,
      protection: operands.core_protection,
      far_control: operands.core_far_control,
      compound_stack: operands.core_compound_stack,
      software_event: operands.core_software_event,
      flags: this.#exports.core_flags,
      debug_step: this.#exports.core_debug_step,
      debug_finish: this.#exports.core_debug_finish,
      debug_register: this.#exports.core_debug_register,
      paging_control: this.#exports.core_paging_control,
      invpcid: this.#exports.core_invpcid,
      processor: this.#exports.core_processor,
      protection_key: this.#exports.core_protection_key,
      debug_begin: this.#exports.core_debug_begin,
      external_step: this.#exports.core_external_step,
      halt: this.#exports.core_halt,
      target_check: this.#exports.core_target_check,
      multiply_high: this.#exports.core_multiply_high,
      divide: this.#exports.core_divide,
      divide_failed: this.#exports.core_divide_failed,
    };
    if (this.#exports.core_read_span) env.read_span = this.#exports.core_read_span;
    if (this.#exports.core_write_span) env.write_span = this.#exports.core_write_span;
    if (this.mmio && this.#exports.core_mmio_operand)
      env.mmio_operand = this.#exports.core_mmio_operand;
    if (this.isPaged) {
      env.fast_system = this.#exports.core_fast_system;
      if (this.#exports.core_paged_loop_guard)
        env.paged_loop_guard = this.#exports.core_paged_loop_guard;
      if (this.#exports.core_paged_loop_read_dword)
        env.paged_loop_read_dword = this.#exports.core_paged_loop_read_dword;
      if (this.#exports.core_paged_loop_read_byte)
        env.paged_loop_read_byte = this.#exports.core_paged_loop_read_byte;
      if (this.#exports.core_paged_loop_read_span)
        env.paged_loop_read_span = this.#exports.core_paged_loop_read_span;
      if (this.#exports.core_write_segment_base)
        env.write_segment_base = this.#exports.core_write_segment_base;
      for (const [name, key] of Object.entries({
        ...PAGED_HELPERS,
        ...(observed ? PAGED_OBSERVED_HELPERS : {}),
      }))
        env[name] = this.#exports[key]!;
    }
    return env;
  }

  execute(
    run: GeneratedRun,
    entryIndex: number,
    budget: bigint,
    stop: bigint | undefined,
    repeatBudget: bigint,
  ): string | { taskEvent: string } {
    this.stateView().setBigUint64(this.#layout.repeatRemaining, repeatBudget, true);
    if (this.io) this.io.executing = true;
    try {
      return this.reason(
        run(this.#statePointer, budget, stop ?? 0n, stop === undefined ? 0 : 1, entryIndex),
        true,
      );
    } finally {
      if (this.io) this.io.executing = false;
    }
  }

  fault(reason: string): Fault | undefined {
    if (reason !== 'data-fault' && reason !== 'fetch-fault' && reason !== 'target-fault')
      return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    const raw = this.info<null | {
      address: string;
      width: number;
      access: Fault['access'];
      cause: Fault['cause'];
      failingAddress: string;
      segment: SegmentName;
    }>();
    return raw === null
      ? undefined
      : {
          ...raw,
          instructionPc: this.diagnosticPc(),
          address: BigInt(raw.address),
          failingAddress: BigInt(raw.failingAddress),
        };
  }

  readContext(): ExecutionContextSnapshot {
    this.#exports.core_context_info();
    return contextFromJson(this.info<ContextInfo>(), this.physicalControls);
  }

  setContext(context: ExecutionContext): void {
    writeInput(this.#exports, context);
    const status = this.#exports.core_set_context();
    if (status === 1) throw new TypeError(this.info<{ error: string }>().error);
    if (status !== 0) throw new Error('invalid context mutation status');
    this.#taskRetry = undefined;
  }

  contextVersion(): bigint {
    return BigInt.asUintN(64, this.#exports.core_context_version());
  }

  boundaryObserved(): boolean {
    const value = this.#exports.core_boundary_observed();
    if (value !== 0 && value !== 1) throw new Error('core returned an invalid boundary profile');
    return value === 1;
  }

  segmentFault(reason: string): SegmentFault | undefined {
    if (reason !== 'segment-fault') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    const raw = this.info<
      Omit<SegmentFault, 'instructionPc' | 'offset' | 'failingOffset'> & {
        offset: string;
        failingOffset: string;
      }
    >();
    return {
      ...raw,
      instructionPc: this.diagnosticPc(),
      offset: BigInt(raw.offset),
      failingOffset: BigInt(raw.failingOffset),
    };
  }

  alignmentFault(reason: string): AlignmentFault | undefined {
    if (reason !== 'alignment-fault') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    const raw = this.info<
      Omit<AlignmentFault, 'instructionPc' | 'offset' | 'address'> & {
        offset: string;
        address: string;
      }
    >();
    return {
      ...raw,
      instructionPc: this.diagnosticPc(),
      offset: BigInt(raw.offset),
      address: BigInt(raw.address),
    };
  }

  alignmentCheck(reason: string): AlignmentCheck | undefined {
    if (reason !== 'alignment-check') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    const raw = this.info<
      Omit<AlignmentCheck, 'instructionPc' | 'offset' | 'address'> & {
        offset: string;
        address: string;
      }
    >();
    return {
      ...raw,
      instructionPc: this.diagnosticPc(),
      offset: BigInt(raw.offset),
      address: BigInt(raw.address),
    };
  }

  simdException(reason: string): SimdException | undefined {
    if (reason !== 'simd-exception') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    return {
      ...this.info<Omit<SimdException, 'instructionPc'>>(),
      instructionPc: this.diagnosticPc(),
    };
  }

  generalProtectionFault(reason: string): GeneralProtectionFault | undefined {
    if (reason !== 'general-protection') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    return {
      ...this.info<Omit<GeneralProtectionFault, 'instructionPc'>>(),
      instructionPc: this.diagnosticPc(),
    };
  }

  protectionFault(reason: string): ProtectionFault | undefined {
    if (reason !== 'protection-fault') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    return {
      ...this.info<Omit<ProtectionFault, 'instructionPc'>>(),
      instructionPc: this.diagnosticPc(),
    };
  }

  debugException(reason: string): DebugException | undefined {
    if (reason !== 'debug-exception') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    const raw = this.info<
      Omit<DebugException, 'instructionPc' | 'resumePc' | 'resumeFlags' | 'deferredOriginPc'> & {
        deferredOriginPc?: string;
        instructionPc: string;
        resumePc: string;
        resumeFlags: string;
      }
    >();
    const { deferredOriginPc, ...diagnostic } = raw;
    return {
      ...diagnostic,
      instructionPc: BigInt(raw.instructionPc),
      resumePc: BigInt(raw.resumePc),
      resumeFlags: BigInt(raw.resumeFlags),
      ...(deferredOriginPc === undefined ? {} : { deferredOriginPc: BigInt(deferredOriginPc) }),
    };
  }

  systemMemoryFault(reason: string): SystemMemoryFault | undefined {
    if (reason !== 'system-memory-fault') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    const raw = this.info<
      Omit<SystemMemoryAccess, 'instructionPc' | 'address' | 'failingAddress'> &
        SystemMemoryReference & {
          address: string;
          failingAddress: string;
        }
    >();
    return {
      ...raw,
      instructionPc: this.diagnosticPc(),
      address: BigInt(raw.address),
      failingAddress: BigInt(raw.failingAddress),
    };
  }

  pageFault(reason: string): PageFault | undefined {
    if (reason !== 'page-fault') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    const raw = this.info<Omit<PageFault, 'instructionPc' | 'address'> & { address: string }>();
    return { ...raw, instructionPc: this.diagnosticPc(), address: BigInt(raw.address) };
  }

  physicalMemoryFault(reason: string): PhysicalMemoryFault | undefined {
    if (reason !== 'physical-memory-fault') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    const raw = this.info<Record<string, unknown>>();
    if (raw.phase === 'pdpte-load') {
      if (
        !['cr0', 'cr3', 'cr4'].includes(raw.control as string) ||
        !Number.isInteger(raw.slot) ||
        (raw.slot as number) < 0 ||
        (raw.slot as number) > 3 ||
        raw.width !== 8 ||
        raw.physicalAccess !== 'read' ||
        [
          'address',
          'access',
          'segment',
          'table',
          'systemPhase',
          'selector',
          'referenceVector',
        ].some((key) => key in raw) ||
        !['unmapped', 'permission', 'address-range'].includes(raw.cause as string)
      )
        throw new Error('core returned invalid PDPTE-load fault');
      return {
        ...raw,
        instructionPc: this.diagnosticPc(),
        physicalAddress: BigInt(raw.physicalAddress as string),
        failingPhysicalAddress: BigInt(raw.failingPhysicalAddress as string),
      } as PdpteLoadFault;
    }
    return {
      ...raw,
      instructionPc: this.diagnosticPc(),
      address: BigInt(raw.address as string),
      physicalAddress: BigInt(raw.physicalAddress as string),
      failingPhysicalAddress: BigInt(raw.failingPhysicalAddress as string),
    } as PhysicalAccessFault;
  }

  numericFault(reason: string): NumericFault | undefined {
    if (
      !['invalid-encoding', 'device-not-available'].includes(reason) ||
      !this.#exports.core_numeric_fault_info
    )
      return undefined;
    this.#exports.core_numeric_fault_info();
    const raw = this.info<(Record<string, unknown> & { instructionPc: string }) | null>();
    if (!raw) return undefined;
    const fault = captureNumericFault({ ...raw, instructionPc: BigInt(raw.instructionPc) });
    if ((fault.vector === 7) !== (reason === 'device-not-available'))
      throw new Error('numeric reason differs');
    return fault;
  }

  x87Exception(reason: string): X87Exception | undefined {
    if (reason !== 'x87-exception') return undefined;
    this.#exports.core_fault_info(this.#layout.reasons.indexOf(reason));
    const raw = this.info<
      Omit<X87Exception, 'instructionPc' | 'instructionPointer' | 'dataPointer'> & {
        instructionPointer: string;
        dataPointer: string;
      }
    >();
    return {
      ...raw,
      instructionPc: this.diagnosticPc(),
      instructionPointer: BigInt(raw.instructionPointer),
      dataPointer: BigInt(raw.dataPointer),
    };
  }

  readMemory(address: bigint, length: number): Uint8Array {
    unsigned64(address, 'address');
    byteLength(length, 'length');
    if (this.#exports.core_read_memory(address, length) !== 0)
      throw new RangeError(JSON.stringify(this.info()));
    return this.output();
  }

  writeOwnedMemory(address: bigint, input: Uint8Array): void {
    // The host copies caller bytes before its final lifecycle check.
    unsigned64(address, 'address');
    const pointer = this.#exports.core_input(input.length) >>> 0;
    new Uint8Array(this.#exports.memory.buffer, pointer, input.length).set(input);
    if (this.#exports.core_write_memory(address) !== 0)
      throw new RangeError(JSON.stringify(this.info()));
  }

  protected memoryResult(status: number): void {
    if (status === 0) return;
    const { error } = this.info<{ error: string }>();
    if (status === 1) throw new TypeError(error);
    if (status === 2) throw new RangeError(error);
    throw new Error('memory mutation returned an invalid status');
  }

  addBacking(input: MemoryBackingInput): void {
    const length = writeInput(this.#exports, { id: backingKey(input.id) }, [input.bytes]);
    this.memoryResult(this.#exports.core_add_backing(length));
  }

  removeBacking(id: string): void {
    writeInput(this.#exports, backingKey(id));
    this.memoryResult(this.#exports.core_remove_backing());
  }

  setMappings(mappings: MemoryMapping[]): void {
    writeInput(this.#exports, mappingInput(mappings));
    this.memoryResult(this.#exports.core_set_mappings());
  }

  valid(owner: bigint): boolean {
    return this.#exports.core_validity(owner) === 1;
  }

  activate(owner: bigint): boolean {
    return this.#exports.core_activate(owner) === 1;
  }

  publish(owner: bigint): boolean {
    return this.#exports.core_publish(owner) === 1;
  }

  release(owner: bigint): void {
    this.#exports.core_release(owner);
  }

  releaseIo(): void {
    this.io?.release();
    this.mmio?.release();
  }

  reset(): void {
    if (!this.physicalControls) throw new TypeError('reset requires physical memory');
    if (this.#exports.core_reset() !== 0) throw new TypeError(this.info<{ error: string }>().error);
    this.#taskRetry = undefined;
  }

  dispose(): void {
    this.releaseIo();
    this.#exports.core_dispose();
  }
}
