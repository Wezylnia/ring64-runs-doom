import { supportsGuestMemory64 } from './wasm/guest-memory64.js';

export { createCpu } from './cpu/cpu.js';

/** Prefer memory64 on supported hosts. Reuse the compiled Core across CPUs. */
export async function loadCpuCompiler(
  guestMemory: 'memory32' | 'memory64' = supportsGuestMemory64() ? 'memory64' : 'memory32',
): Promise<WebAssembly.Module> {
  if (guestMemory !== 'memory32' && guestMemory !== 'memory64')
    throw new TypeError('guestMemory must be memory32 or memory64');
  const { loadCompiler } = await import('./wasm/load.js');
  return loadCompiler(
    guestMemory === 'memory64'
      ? new URL('./ring64-memory64.wasm', import.meta.url)
      : new URL('./ring64.wasm', import.meta.url),
  );
}

export type { Cpu, Limits, Options, RunOptions, RunReason, RunResult } from './cpu/cpu.js';
export type {
  State,
  StateSnapshot,
  Registers,
  Interruptibility,
} from './state/execution/registers.js';
export type { X87State, X87Exception } from './state/numeric/x87.js';
export type {
  EventState,
  ExceptionEvent,
  InterruptEvent,
  InterruptState,
} from './state/execution/events.js';
export type {
  CompilerSource,
  DebugException,
  Fault,
  AlignmentFault,
  AlignmentCheck,
  SimdException,
  GeneralProtectionFault,
  ProtectionFault,
  SystemMemoryFault,
  PageFault,
  PhysicalMemoryFault,
  UnitMetadata,
} from './wasm/core.js';
export type {
  Span,
  MemoryBackingInput,
  MemoryPermissions,
  MemoryMapping,
  MemoryLimits,
  MemoryModel,
} from './embedding/memory.js';
export type {
  ExecutionContext,
  ExecutionContextSnapshot,
  LegacyExecutionContext,
  ExecutionMode,
  SegmentSnapshot,
  TableRegister,
  LdtRegister,
  TaskRegister,
  LoadedSegment,
  SegmentName,
  SegmentType,
  SegmentFault,
} from './state/system/context.js';

export type { DebugState, DebugContinuation, DebugSource } from './state/execution/debug.js';
export type { PagingState } from './state/system/paging.js';
export type { FastSystemState } from './state/system/fast-system.js';
export type { TimeState } from './state/system/time.js';
export type { ProtectionKeysState } from './state/system/protection-keys.js';

export type { IoProvider, IoWidth } from './embedding/io.js';
export type { MmioProvider, MmioRange, MmioWidth } from './embedding/mmio.js';
export type {
  DeviceNotAvailable,
  NumericControlFault,
  NumericControls,
  NumericFamily,
} from './state/numeric/numeric-controls.js';
