export { createCpu } from './cpu/cpu.js';

/** Compile the packaged standalone Core for reuse across CPU instances. */
export async function loadCpuCompiler(): Promise<WebAssembly.Module> {
  const { loadCompiler } = await import('./wasm/load.js');
  return loadCompiler(new URL('./ring64.wasm', import.meta.url));
}

export type { Cpu, Limits, Options, RunResult } from './cpu/cpu.js';
export type { State, StateSnapshot, Interruptibility } from './state/execution/registers.js';
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
  Span,
  UnitMetadata,
} from './wasm/core.js';
export type {
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
export type {
  DeviceNotAvailable,
  NumericControlFault,
  NumericControls,
  NumericFamily,
} from './state/numeric/numeric-controls.js';
