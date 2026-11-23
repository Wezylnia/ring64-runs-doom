import type {
  Core,
  Fault,
  AlignmentFault,
  AlignmentCheck,
  SimdException,
  GeneralProtectionFault,
  ProtectionFault,
  SystemMemoryFault,
  DebugException,
  PageFault,
  PhysicalMemoryFault,
} from '../wasm/core.js';
import type { StateSnapshot } from '../state/execution/registers.js';
import type { SegmentFault } from '../state/system/context.js';
import type { X87Exception } from '../state/numeric/x87.js';
import type { DeviceNotAvailable, NumericControlFault } from '../state/numeric/numeric-controls.js';

const runReasonNames = [
  'budget',
  'stop',
  'unsupported',
  'fetch-fault',
  'target-fault',
  'data-fault',
  'invalid-encoding',
  'resource-limit',
  'divide-error',
  'segment-fault',
  'repeat-budget',
  'alignment-fault',
  'simd-exception',
  'general-protection',
  'x87-exception',
  'protection-fault',
  'system-memory-fault',
  'debug-exception',
  'event-budget',
  'pending-exception',
  'shutdown',
  'pending-interrupt',
  'halted',
  'page-fault',
  'physical-memory-fault',
  'device-not-available',
  'alignment-check',
  'paused',
] as const;

export type RunReason = (typeof runReasonNames)[number];
const runReasons: ReadonlySet<string> = new Set(runReasonNames);

export function isRunReason(reason: string): reason is RunReason {
  return runReasons.has(reason);
}

interface RunDiagnostics {
  fault?: Fault;
  segmentFault?: SegmentFault;
  alignmentFault?: AlignmentFault;
  alignmentCheck?: AlignmentCheck;
  simdException?: SimdException;
  generalProtectionFault?: GeneralProtectionFault;
  x87Exception?: X87Exception;
  deviceNotAvailable?: DeviceNotAvailable;
  numericControlFault?: NumericControlFault;
  protectionFault?: ProtectionFault;
  systemMemoryFault?: SystemMemoryFault;
  debugException?: DebugException;
  pageFault?: PageFault;
  physicalMemoryFault?: PhysicalMemoryFault;
}

interface RunResultBase extends RunDiagnostics {
  retired: bigint;
  remaining: bigint;
  repeatIterations: bigint;
  remainingRepeatBudget: bigint;
  deliveryAttempts?: bigint;
  remainingDeliveryBudget?: bigint;
  state: StateSnapshot;
  resources: {
    pagingRestarts?: number;
    compilations: number;
    cacheHits: number;
    nativeEntries: number;
    dispatches: number;
    generatedBytes: number;
    cachedArtifacts: number;
    cachedBytes: number;
  };
  timings: {
    translationMs: number;
    browserCompileMs: number;
    instantiationMs: number;
    executionMs: number;
    totalMs: number;
  };
}

// The optional numeric ABI can omit details even for a matching reason.
const runDiagnosticFields = {
  'fetch-fault': 'fault',
  'target-fault': 'fault',
  'data-fault': 'fault',
  'segment-fault': 'segmentFault',
  'alignment-fault': 'alignmentFault',
  'alignment-check': 'alignmentCheck',
  'simd-exception': 'simdException',
  'general-protection': 'generalProtectionFault',
  'x87-exception': 'x87Exception',
  'protection-fault': 'protectionFault',
  'system-memory-fault': 'systemMemoryFault',
  'debug-exception': 'debugException',
  'page-fault': 'pageFault',
  'physical-memory-fault': 'physicalMemoryFault',
} as const satisfies Partial<Record<RunReason, keyof RunDiagnostics>>;

type DiagnosedReason = keyof typeof runDiagnosticFields;
type DiagnosticField = (typeof runDiagnosticFields)[DiagnosedReason];
const diagnosticFields: Partial<Record<RunReason, DiagnosticField>> = runDiagnosticFields;

export function readRunDiagnostics(core: Core, reason: RunReason): RunDiagnostics {
  const field = diagnosticFields[reason];
  if (field !== undefined) return { [field]: core[field](reason) };
  if (reason === 'device-not-available' || reason === 'invalid-encoding') {
    const numeric = core.numericFault?.(reason);
    if (numeric?.vector === 7) return { deviceNotAvailable: numeric };
    if (numeric?.vector === 6) return { numericControlFault: numeric };
  }
  return {};
}

type DiagnosedRun = {
  [Reason in DiagnosedReason]: { reason: Reason } & Required<
    Pick<RunDiagnostics, (typeof runDiagnosticFields)[Reason]>
  >;
}[DiagnosedReason];

export type RunResult = RunResultBase &
  (DiagnosedRun | { reason: Exclude<RunReason, DiagnosedReason> });

export function validateRunDiagnostic(
  result: RunResultBase & { reason: RunReason },
): asserts result is RunResult {
  const field = diagnosticFields[result.reason];
  if (field !== undefined && result[field] === undefined)
    throw new Error(`core omitted ${field} for ${result.reason}`);
}
