import { captureIo, type IoProvider } from '../embedding/io.js';
import { copyMemoryInput, unsigned64 } from '../embedding/input.js';
import { captureMmio, captureMmioRanges, type MmioProvider } from '../embedding/mmio.js';
import type { DeviceNotAvailable, NumericControlFault } from '../state/numeric/numeric-controls.js';
import { captureDebugState, type DebugState } from '../state/execution/debug.js';
import { capturePagingState, type PagingState } from '../state/system/paging.js';
import { captureTimeState, type TimeState } from '../state/system/time.js';
import {
  captureProtectionKeysState,
  type ProtectionKeysState,
} from '../state/system/protection-keys.js';
import {
  captureFastSystemState,
  zeroFastSystemState,
  type FastSystemState,
} from '../state/system/fast-system.js';
import {
  captureContext,
  type ExecutionContext,
  type ExecutionContextSnapshot,
  type SegmentFault,
} from '../state/system/context.js';
import { captureX87State, type X87State, type X87Exception } from '../state/numeric/x87.js';
import {
  Core,
  type CompileLimits,
  type CompilerSource,
  type Fault,
  type AlignmentFault,
  type AlignmentCheck,
  type SimdException,
  type GeneralProtectionFault,
  type ProtectionFault,
  type SystemMemoryFault,
  type DebugException,
  type PageFault,
  type PhysicalMemoryFault,
} from '../wasm/core.js';
import { Translations, type InstalledEntry } from './translations.js';
import {
  captureState,
  captureXmmState,
  captureMxcsr,
  type State,
  type StateSnapshot,
} from '../state/execution/registers.js';
import {
  backingName,
  captureBackings,
  captureInitialMemory,
  captureMappings,
  validateBackings,
  validateMappings,
  type MemoryBackingInput,
  type MemoryLimits,
  type MemoryMapping,
  type MemoryModel,
  type MemoryOptions,
} from '../embedding/memory.js';

export interface Limits extends CompileLimits {
  maxCachedArtifacts: number;
  maxCachedBytes: number;
  maxTranslationsPerRun: number;
}

interface CommonOptions {
  io?: IoProvider;
  mmio?: MmioProvider;
  compiler: CompilerSource;
  limits: Limits;
  context?: ExecutionContext;
}

export type Options = CommonOptions & MemoryOptions;

export type RunOptions = {
  budget: bigint;
  stop?: bigint | undefined;
  repeatBudget?: bigint | undefined;
  exceptionDelivery?: { budget: bigint } | undefined;
  eventDelivery?: { budget: bigint } | undefined;
};

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

function isRunReason(reason: string): reason is RunReason {
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
type DiagnosedRun = {
  [Reason in DiagnosedReason]: { reason: Reason } & Required<
    Pick<RunDiagnostics, (typeof runDiagnosticFields)[Reason]>
  >;
}[DiagnosedReason];

export type RunResult = RunResultBase &
  (DiagnosedRun | { reason: Exclude<RunReason, DiagnosedReason> });

function validateRunDiagnostic(
  result: RunResultBase & { reason: RunReason },
): asserts result is RunResult {
  const fields: Partial<Record<RunReason, keyof RunDiagnostics>> = runDiagnosticFields;
  const field = fields[result.reason];
  if (field !== undefined && result[field] === undefined)
    throw new Error(`core omitted ${field} for ${result.reason}`);
}

export function checkedLimits(limits: Limits): Limits {
  const names = [
    'maxInstructions',
    'maxSourceBytes',
    'maxArtifactBytes',
    'maxCachedArtifacts',
    'maxCachedBytes',
    'maxTranslationsPerRun',
  ] as const;
  const result = { ...limits };
  for (const name of names) {
    const value = result[name];
    if (!Number.isSafeInteger(value) || value <= 0 || value > 0xffffffff) {
      throw new TypeError(`${name} must be an explicit positive wasm32 integer`);
    }
  }
  return result;
}

export const CONSTRUCTION = Symbol('createCpu');
export const BEGIN_RUN = Symbol('beginRun');
export const FINISH_RUN = Symbol('finishRun');
export const FAIL_RUN = Symbol('failRun');
export const LEAVE_RUN = Symbol('leaveRun');
// [] -> [] with v128.const followed by drop: fixed SIMD, no relaxed operations.
const SIMD_PROBE = Uint8Array.of(
  0,
  97,
  115,
  109,
  1,
  0,
  0,
  0,
  1,
  4,
  1,
  96,
  0,
  0,
  3,
  2,
  1,
  0,
  10,
  23,
  1,
  21,
  0,
  253,
  12,
  ...Array<number>(16).fill(0),
  26,
  11,
);

export function requireSimd(): void {
  if (!WebAssembly.validate(SIMD_PROBE)) throw new TypeError('WebAssembly SIMD is required');
}

export function captureInitialContext(
  contextInput: ExecutionContext | undefined,
  memoryModel: MemoryModel | undefined,
): ExecutionContext | undefined {
  const context =
    contextInput === undefined
      ? undefined
      : captureContext(contextInput, memoryModel === 'physical');
  if (memoryModel === 'physical' && context === undefined)
    throw new TypeError('physical memory requires a non-undefined context');
  if (
    memoryModel === 'ia32e-paged' &&
    context &&
    context.mode !== 'long64' &&
    context.mode !== 'compatibility'
  )
    throw new TypeError('paged memory requires a Long64 or compatibility context');
  return context;
}

export async function createCpu(options: Options): Promise<Cpu> {
  const began = performance.now();
  requireSimd();
  const limits = checkedLimits(options.limits);
  const io = captureIo(options);
  const mmioInput = captureMmio(options);
  const memory = captureInitialMemory(options, mmioInput?.count);
  const mmio = mmioInput && captureMmioRanges(mmioInput, memory.memoryModel ?? 'linear');
  if (memory.memoryModel !== 'linear') {
    memory.paging = capturePagingState(options.paging!);
    memory.fastSystem =
      'fastSystem' in options ? captureFastSystemState(options.fastSystem!) : zeroFastSystemState();
  }
  const context = captureInitialContext(options.context, memory.memoryModel);
  const core =
    mmio === undefined
      ? await Core.create(options.compiler, memory, context, io)
      : await Core.create(options.compiler, memory, context, io, mmio);
  return new Cpu(
    CONSTRUCTION,
    core,
    limits,
    performance.now() - began,
    mmio
      ? {
          ...memory.memoryLimits,
          maxMappings: memory.memoryLimits.maxMappings - mmio.ranges.length,
        }
      : memory.memoryLimits,
    new Map(memory.backings.map(({ id, bytes }) => [id, bytes.length])),
    memory.memoryModel,
  );
}

export class Cpu {
  #lifecycle: 'idle' | 'running' | 'failed' | 'disposed' = 'idle';
  #runGeneration = 0;
  #captureToken: { readonly reset: object } = { reset: {} };
  readonly #cache: Translations;
  #core: Core | undefined;
  readonly #limits: Limits;
  readonly #memoryLimits: MemoryLimits;
  readonly #memoryModel: MemoryModel;
  #backings: Map<string, number>;

  constructor(
    token: typeof CONSTRUCTION,
    core: Core,
    limits: Limits,
    readonly creationMs: number,
    memoryLimits: MemoryLimits,
    backings: Map<string, number>,
    memoryModel: MemoryModel = 'linear',
  ) {
    if (token !== CONSTRUCTION) throw new TypeError('use the createCpu factory');
    this.#core = core;
    this.#limits = {
      ...limits,
      maxArtifactBytes: Math.min(limits.maxArtifactBytes, limits.maxCachedBytes),
    };
    this.#memoryLimits = { ...memoryLimits };
    this.#memoryModel = memoryModel;
    this.#backings = backings;
    this.#cache = new Translations(limits.maxCachedArtifacts, limits.maxCachedBytes, (artifact) =>
      this.#core?.release(artifact.metadata.owner),
    );
  }

  private requireIdle(
    captureToken?: { readonly reset: object },
    allowCompletedRun = false,
  ): { readonly reset: object } {
    if (this.#lifecycle !== 'idle') throw new Error(`cpu is ${this.#lifecycle}`);
    if (
      captureToken !== undefined &&
      captureToken !== this.#captureToken &&
      (!allowCompletedRun || captureToken.reset !== this.#captureToken.reset)
    )
      throw new Error('cpu run or reset started during input capture');
    return this.#captureToken;
  }

  private requirePhysicalMemory(): void {
    if (this.#memoryModel === 'linear')
      throw new TypeError('method requires physical or ia32e-paged memory');
  }

  private requireMemoryModel(model: MemoryModel): void {
    if (this.#memoryModel !== model) throw new TypeError(`method requires ${model} memory`);
  }

  get #engine(): Core {
    if (!this.#core) throw new Error('cpu is disposed');
    return this.#core;
  }

  protected pruneTranslations(): void {
    try {
      const engine = this.#engine;
      if (engine.takeSourceInvalidation?.() !== false)
        this.#cache.prune((artifact) => engine.valid(artifact.metadata.owner));
    } catch (cause) {
      this.#lifecycle = 'failed';
      throw new Error('cpu engine failed', { cause });
    }
  }

  private engineCall<T>(action: () => T): T {
    try {
      return action();
    } catch (cause) {
      if (cause instanceof TypeError || cause instanceof RangeError) throw cause;
      this.#lifecycle = 'failed';
      throw new Error('cpu engine failed', { cause });
    }
  }

  addBacking(input: MemoryBackingInput): void {
    const generation = this.requireIdle();
    const captured = captureBackings([input], this.#memoryLimits, this.#backings);
    this.requireIdle(generation, true);
    const lengths = validateBackings(captured, this.#backings, this.#memoryLimits);
    this.engineCall(() => this.#engine.addBacking(captured[0]!));
    this.#backings = lengths;
  }

  removeBacking(id: string): void {
    this.requireIdle();
    backingName(id);
    if (!this.#backings.has(id)) throw new TypeError('unknown backing name');
    this.engineCall(() => this.#engine.removeBacking(id));
    this.#backings.delete(id);
  }

  setMappings(mappings: MemoryMapping[]): void {
    const generation = this.requireIdle();
    const captured = captureMappings(mappings, this.#memoryLimits, this.#memoryModel);
    this.requireIdle(generation, true);
    validateMappings(captured, this.#backings);
    this.engineCall(() => this.#engine.setMappings(captured));
    this.pruneTranslations();
  }

  readContext(): ExecutionContextSnapshot {
    this.requireIdle();
    return this.engineCall(() => this.#engine.readContext());
  }

  setContext(context: ExecutionContext): void {
    const generation = this.requireIdle();
    const input = captureContext(context, this.#memoryModel === 'physical');
    this.requireIdle(generation, true);
    if (
      this.#memoryModel === 'ia32e-paged' &&
      input.mode !== 'long64' &&
      input.mode !== 'compatibility'
    )
      throw new TypeError('paged memory requires a Long64 or compatibility context');
    this.engineCall(() => this.#engine.setContext(input));
    this.pruneTranslations();
  }

  setState(state: State): void {
    const generation = this.requireIdle();
    const input = captureState(state);
    // Input accessors can start a run or dispose the CPU.
    this.requireIdle(generation);
    this.engineCall(() => this.#engine.setState(input));
  }
  readState(): StateSnapshot {
    this.requireIdle();
    return this.engineCall(() => this.#engine.snapshot());
  }
  requestInterrupt(vector: number): boolean {
    this.requireIdle();
    if (typeof vector !== 'number' || !Number.isInteger(vector) || vector < 0 || vector > 255)
      throw new TypeError('IRQ vector must be an unsigned 8-bit integer number');
    return this.engineCall(() => this.#engine.requestInterrupt(vector === 0 ? 0 : vector));
  }
  requestNmi(): void {
    this.requireIdle();
    this.engineCall(() => this.#engine.requestNmi());
  }
  setXmmState(state: bigint[]): void {
    const generation = this.requireIdle();
    const bank = captureXmmState(state);
    this.requireIdle(generation, true);
    this.engineCall(() => this.#engine.setXmmState(bank));
  }
  readDebugState(): DebugState {
    this.requireIdle();
    return this.engineCall(() => this.#engine.readDebugState());
  }
  setDebugState(state: DebugState): void {
    const generation = this.requireIdle();
    const input = captureDebugState(state);
    this.requireIdle(generation);
    this.engineCall(() => this.#engine.setDebugState(input));
    this.pruneTranslations();
  }
  readPagingState(): PagingState {
    this.requireIdle();
    this.requirePhysicalMemory();
    return this.engineCall(() => this.#engine.readPagingState());
  }
  setPagingState(state: PagingState): void {
    const generation = this.requireIdle();
    this.requirePhysicalMemory();
    const input = capturePagingState(state);
    this.requireIdle(generation);
    this.engineCall(() => this.#engine.setPagingState(input));
    this.pruneTranslations();
  }
  readFastSystemState(): FastSystemState {
    this.requireIdle();
    this.requirePhysicalMemory();
    return this.engineCall(() => this.#engine.readFastSystemState());
  }
  setFastSystemState(state: FastSystemState): void {
    const generation = this.requireIdle();
    this.requirePhysicalMemory();
    const input = captureFastSystemState(state);
    this.requireIdle(generation);
    this.engineCall(() => this.#engine.setFastSystemState(input));
    this.pruneTranslations();
  }
  readTimeState(): TimeState {
    this.requireIdle();
    return this.engineCall(() => this.#engine.readTimeState());
  }
  setTimeState(state: TimeState): void {
    const generation = this.requireIdle();
    const input = captureTimeState(state);
    this.requireIdle(generation);
    this.engineCall(() => this.#engine.setTimeState(input));
  }
  readXcr0(): bigint {
    this.requireIdle();
    return this.engineCall(() => this.#engine.readXcr0());
  }
  setXcr0(value: bigint): void {
    this.requireIdle();
    this.engineCall(() => this.#engine.setXcr0(value));
  }
  readProtectionKeysState(): ProtectionKeysState {
    this.requireIdle();
    return this.engineCall(() => this.#engine.readProtectionKeysState());
  }
  setProtectionKeysState(state: ProtectionKeysState): void {
    const generation = this.requireIdle();
    const input = captureProtectionKeysState(state);
    this.requireIdle(generation);
    this.engineCall(() => this.#engine.setProtectionKeysState(input));
  }
  readXmmState(): bigint[] {
    this.requireIdle();
    return this.engineCall(() => this.#engine.readXmmState());
  }
  readMxcsr(): number {
    this.requireIdle();
    return this.engineCall(() => this.#engine.readMxcsr());
  }
  setMxcsr(value: number): void {
    const generation = this.requireIdle();
    const input = captureMxcsr(value);
    this.requireIdle(generation, true);
    this.engineCall(() => this.#engine.setMxcsr(input));
  }
  readX87State(): X87State {
    this.requireIdle();
    return this.engineCall(() => this.#engine.readX87State());
  }
  setX87State(value: X87State): void {
    const generation = this.requireIdle();
    const input = captureX87State(value);
    this.requireIdle(generation, true);
    this.engineCall(() => this.#engine.setX87State(input));
  }
  readMemory(address: bigint, length: number): Uint8Array {
    this.requireIdle();
    this.requireMemoryModel('linear');
    return this.engineCall(() => this.#engine.readMemory(address, length));
  }
  writeMemory(address: bigint, bytes: Uint8Array): void {
    const generation = this.requireIdle();
    this.requireMemoryModel('linear');
    unsigned64(address, 'address');
    const input = copyMemoryInput(bytes);
    this.requireIdle(generation, true);
    this.engineCall(() => this.#engine.writeOwnedMemory(address, input));
    this.pruneTranslations();
  }
  readPhysicalMemory(address: bigint, length: number): Uint8Array {
    this.requireIdle();
    this.requirePhysicalMemory();
    return this.engineCall(() => this.#engine.readMemory(address, length));
  }
  writePhysicalMemory(address: bigint, bytes: Uint8Array): void {
    const generation = this.requireIdle();
    this.requirePhysicalMemory();
    unsigned64(address, 'address');
    const input = copyMemoryInput(bytes);
    this.requireIdle(generation);
    this.engineCall(() => this.#engine.writeOwnedMemory(address, input));
    this.pruneTranslations();
  }
  reset(): void {
    this.requireIdle();
    this.requireMemoryModel('physical');
    this.engineCall(() => {
      this.#engine.reset();
      this.#cache.clear();
    });
    this.#captureToken = { reset: {} };
  }

  inspectTranslations() {
    this.requireIdle();
    return this.#cache.inspect();
  }

  protected [BEGIN_RUN](options: RunOptions) {
    const generation = this.requireIdle();
    const {
      budget,
      stop,
      repeatBudget: suppliedRepeatBudget,
      exceptionDelivery,
      eventDelivery,
    } = options;
    if (exceptionDelivery !== undefined && eventDelivery !== undefined)
      throw new TypeError('eventDelivery and exceptionDelivery are mutually exclusive');
    const selectedDelivery = eventDelivery === undefined ? exceptionDelivery : eventDelivery;
    let deliveryBudget: bigint | undefined;
    if (selectedDelivery !== undefined) {
      if (
        selectedDelivery === null ||
        typeof selectedDelivery !== 'object' ||
        Array.isArray(selectedDelivery)
      ) {
        throw new TypeError('event delivery must contain a budget');
      }
      deliveryBudget = selectedDelivery.budget;
    }
    this.requireIdle(generation);
    unsigned64(budget, 'budget');
    if (stop !== undefined) unsigned64(stop, 'stop');
    const repeatBudget = suppliedRepeatBudget === undefined ? budget : suppliedRepeatBudget;
    unsigned64(repeatBudget, 'repeatBudget');
    if (selectedDelivery !== undefined) unsigned64(deliveryBudget, 'event delivery budget');
    if (this.#runGeneration === Number.MAX_SAFE_INTEGER)
      throw new Error('cpu run generation exhausted');
    this.#runGeneration++;
    this.#captureToken = { reset: this.#captureToken.reset };
    this.#lifecycle = 'running';
    const began = performance.now();
    const resources = {
      ...(this.#memoryModel !== 'linear' ? { pagingRestarts: 0 } : {}),
      compilations: 0,
      cacheHits: 0,
      nativeEntries: 0,
      dispatches: 0,
      generatedBytes: 0,
    };
    const timings = {
      translationMs: 0,
      browserCompileMs: 0,
      instantiationMs: 0,
      executionMs: 0,
      totalMs: 0,
    };
    return {
      budget,
      stop,
      repeatBudget,
      deliveryBudget,
      resources,
      timings,
      began,
      cache: this.#cache,
      limits: this.#limits,
    };
  }

  protected [FINISH_RUN](
    run: ReturnType<Cpu[typeof BEGIN_RUN]>,
    reason: string,
    remaining: bigint,
    remainingRepeatBudget: bigint,
    remainingDeliveryBudget: bigint | undefined,
  ): RunResult {
    if (!isRunReason(reason)) throw new Error(`core returned an invalid run reason: ${reason}`);
    const { budget, repeatBudget, deliveryBudget, resources, timings, began } = run;
    this.pruneTranslations();
    const numeric = this.#engine.numericFault?.(reason);
    const fault = this.#engine.fault(reason);
    const segmentFault = this.#engine.segmentFault(reason);
    const alignmentFault = this.#engine.alignmentFault(reason);
    const alignmentCheck =
      reason === 'alignment-check' ? this.#engine.alignmentCheck(reason) : undefined;
    const simdException =
      reason === 'simd-exception' ? this.#engine.simdException(reason) : undefined;
    const generalProtectionFault =
      reason === 'general-protection' ? this.#engine.generalProtectionFault(reason) : undefined;
    const x87Exception = reason === 'x87-exception' ? this.#engine.x87Exception(reason) : undefined;
    const protectionFault =
      reason === 'protection-fault' ? this.#engine.protectionFault(reason) : undefined;
    const systemMemoryFault =
      reason === 'system-memory-fault' ? this.#engine.systemMemoryFault(reason) : undefined;
    const debugException =
      reason === 'debug-exception' ? this.#engine.debugException(reason) : undefined;
    const pageFault = reason === 'page-fault' ? this.#engine.pageFault(reason) : undefined;
    const physicalMemoryFault =
      reason === 'physical-memory-fault' ? this.#engine.physicalMemoryFault(reason) : undefined;
    const result = {
      reason,
      retired: budget - remaining,
      remaining,
      repeatIterations: repeatBudget - remainingRepeatBudget,
      remainingRepeatBudget,
      ...(deliveryBudget === undefined
        ? {}
        : {
            deliveryAttempts: deliveryBudget - remainingDeliveryBudget!,
            remainingDeliveryBudget: remainingDeliveryBudget!,
          }),
      state: this.#engine.snapshot(),
      ...(numeric?.vector === 7
        ? { deviceNotAvailable: numeric }
        : numeric?.vector === 6
          ? { numericControlFault: numeric }
          : {}),
      ...(fault ? { fault } : {}),
      ...(segmentFault ? { segmentFault } : {}),
      ...(alignmentFault ? { alignmentFault } : {}),
      ...(alignmentCheck ? { alignmentCheck } : {}),
      ...(simdException ? { simdException } : {}),
      ...(generalProtectionFault ? { generalProtectionFault } : {}),
      ...(x87Exception ? { x87Exception } : {}),
      ...(protectionFault ? { protectionFault } : {}),
      ...(systemMemoryFault ? { systemMemoryFault } : {}),
      ...(debugException ? { debugException } : {}),
      ...(pageFault ? { pageFault } : {}),
      ...(physicalMemoryFault ? { physicalMemoryFault } : {}),
      resources: { ...resources, ...this.#cache.resources() },
      timings,
    };
    validateRunDiagnostic(result);
    timings.totalMs = performance.now() - began;
    return result;
  }

  protected [FAIL_RUN](cause: unknown): never {
    this.#lifecycle = 'failed';
    throw new Error('cpu engine failed', { cause });
  }

  protected [LEAVE_RUN](deliveryBudget: bigint | undefined): void {
    if (this.#lifecycle === 'running') {
      if (deliveryBudget !== undefined) this.#engine.configureDelivery(false);
      this.#lifecycle = 'idle';
    }
  }

  async run(options: RunOptions): Promise<RunResult> {
    const run = this[BEGIN_RUN](options);
    const { budget, stop, repeatBudget, deliveryBudget, resources, timings } = run;
    let remaining = budget;
    let remainingRepeatBudget = repeatBudget;
    let remainingDeliveryBudget = deliveryBudget;
    let admissionRestarts = 0;
    const restartAdmission = () => {
      if (admissionRestarts >= this.#limits.maxTranslationsPerRun) return false;
      admissionRestarts++;
      return true;
    };
    const finish = (reason: string) =>
      this[FINISH_RUN](run, reason, remaining, remainingRepeatBudget, remainingDeliveryBudget);
    try {
      if (deliveryBudget !== undefined) this.#engine.configureDelivery(true);
      let servicingEvent = false;
      // Idle-only intake keeps queues stable except at explicit acceptance boundaries.
      let eventKind = this.#engine.eventKind();
      let queuedInterrupts = this.#engine.hasQueuedInterrupts();
      if (this.#engine.taskRetry) {
        if (remaining === 0n) return finish('budget');
        if (stop !== undefined && this.#engine.current().rip === stop) return finish('stop');
        this.#engine.prepareEntry();
        const at = performance.now();
        const resumed = this.#engine.resumeTask();
        timings.executionMs += performance.now() - at;
        this.pruneTranslations();
        eventKind = this.#engine.eventKind();
        queuedInterrupts = this.#engine.hasQueuedInterrupts();
        if (typeof resumed !== 'string') {
          if (deliveryBudget === undefined) return finish(resumed.taskEvent);
        } else if (
          resumed !== 'dispatch' &&
          !(resumed === 'pending-interrupt' && deliveryBudget !== undefined)
        ) {
          return finish(resumed);
        }
        servicingEvent = eventKind === 'pending' || eventKind === 'pending-interrupt';
      }
      let dispatchedPc: bigint | undefined;
      execution: for (;;) {
        if (
          eventKind === 'shutdown' &&
          (deliveryBudget === undefined || !this.#engine.shutdownCanWake())
        )
          return finish('shutdown');
        const boundaryObserved = eventKind === 'ready' && this.#engine.prepareEntry();
        // boundary preparation clears diagnostics without changing rip.
        const pc = dispatchedPc ?? this.#engine.current().rip;
        dispatchedPc = undefined;
        if (!servicingEvent) {
          if (remaining === 0n) return finish('budget');
          if (stop !== undefined && pc === stop) return finish('stop');
        }
        if (eventKind === 'pending' || eventKind === 'pending-interrupt') {
          if (remainingDeliveryBudget === undefined)
            return finish(eventKind === 'pending' ? 'pending-exception' : 'pending-interrupt');
          if (remainingDeliveryBudget === 0n) return finish('event-budget');
          remainingDeliveryBudget--;
          const at = performance.now();
          const reason = this.#engine.deliverException();
          timings.executionMs += performance.now() - at;
          if (reason === 'shutdown') {
            eventKind = 'shutdown';
            servicingEvent = false;
            continue;
          }
          if (reason !== 'dispatch') return finish(reason);
          eventKind = this.#engine.eventKind();
          servicingEvent = eventKind === 'pending' || eventKind === 'pending-interrupt';
          this.pruneTranslations();
          resources.dispatches++;
          continue;
        }
        servicingEvent = false;
        if (queuedInterrupts || eventKind === 'halted' || eventKind === 'shutdown') {
          const reason = this.#engine.externalBoundary();
          if (reason === 'pending-interrupt') {
            eventKind = this.#engine.eventKind();
            queuedInterrupts = this.#engine.hasQueuedInterrupts();
            if (deliveryBudget === undefined) return finish(reason);
            if (eventKind !== 'pending-interrupt')
              throw new Error('core failed to accept eligible interrupt');
            servicingEvent = true;
            continue;
          }
          if (reason !== 'dispatch') return finish(reason);
        }
        let entry: InstalledEntry | undefined;
        let cached = false;
        let executionBegan = 0;
        let refusedEpoch: bigint | undefined;
        for (const candidate of this.#cache.candidates(pc, boundaryObserved)) {
          executionBegan = performance.now();
          if (this.#engine.activate(candidate.artifact.metadata.owner)) {
            entry = candidate;
            cached = true;
            break;
          }
          timings.executionMs += performance.now() - executionBegan;
          if (!this.#engine.valid(candidate.artifact.metadata.owner))
            this.#cache.remove(candidate.artifact);
          const epoch = this.#engine.contextVersion();
          if (
            this.#engine.boundaryObserved() !== boundaryObserved ||
            (refusedEpoch !== undefined && refusedEpoch !== epoch)
          ) {
            if (!restartAdmission()) return finish('resource-limit');
            continue execution;
          }
          refusedEpoch = epoch;
        }
        if (!entry) {
          const probe = this.#engine.probe(pc);
          if (probe === 'paging-restart') {
            resources.pagingRestarts = (resources.pagingRestarts ?? 0) + 1;
            if (resources.pagingRestarts >= this.#limits.maxTranslationsPerRun)
              return finish('resource-limit');
            this.pruneTranslations();
            continue;
          }
          if (probe !== 'dispatch') {
            if (
              (deliveryBudget !== undefined || boundaryObserved) &&
              this.#engine.stageException(probe)
            ) {
              eventKind = 'pending';
              servicingEvent = true;
              continue;
            }
            return finish(probe);
          }
          if (resources.compilations >= this.#limits.maxTranslationsPerRun)
            return finish('resource-limit');
          resources.compilations++;
          let at = performance.now();
          const output = this.#engine.compile(pc, this.#limits);
          timings.translationMs += performance.now() - at;
          if (!output) return finish('resource-limit');
          resources.generatedBytes += output.bytes.length;
          if (!this.#cache.canFit(output.bytes.length)) {
            this.#engine.release(output.metadata.owner);
            return finish('resource-limit');
          }
          let installed = false;
          try {
            at = performance.now();
            const module = await WebAssembly.compile(new Uint8Array(output.bytes));
            timings.browserCompileMs += performance.now() - at;
            const artifact = this.#cache.installCompiled(
              this.#engine,
              module,
              output.metadata,
              timings,
            );
            if (!artifact) continue;
            if (artifact === 'resource-limit') return finish(artifact);
            installed = true;
            entry = this.#cache
              .candidates(pc, output.metadata.boundaryObserved)
              .find((candidate) => candidate.artifact === artifact);
            if (!entry) throw new Error('installed artifact does not own its requested entry');
          } finally {
            if (!installed) this.#engine.release(output.metadata.owner);
          }
          executionBegan = performance.now();
          if (!this.#engine.activate(entry.artifact.metadata.owner)) {
            timings.executionMs += performance.now() - executionBegan;
            if (!this.#engine.valid(entry.artifact.metadata.owner))
              this.#cache.remove(entry.artifact);
            if (!restartAdmission()) return finish('resource-limit');
            continue;
          }
        }
        const before = remaining;
        const repeatBefore = remainingRepeatBudget;
        const reason = this.#engine.execute(
          entry.artifact.run,
          entry.index,
          remaining,
          stop,
          remainingRepeatBudget,
        );
        timings.executionMs += performance.now() - executionBegan;
        this.#cache.touch(entry.artifact);
        if (cached) resources.cacheHits++;
        resources.nativeEntries++;
        const current = this.#engine.current();
        remaining = current.remaining;
        remainingRepeatBudget = current.repeatRemaining;
        if (
          remaining > before ||
          remainingRepeatBudget > repeatBefore ||
          (reason === 'dispatch' &&
            remaining === before &&
            remainingRepeatBudget === repeatBefore) ||
          (reason === 'budget' && remaining !== 0n) ||
          (reason === 'repeat-budget' && (remainingRepeatBudget !== 0n || remaining === 0n))
        ) {
          throw new Error('generated execution violated its progress contract');
        }
        if (reason === 'paging-restart') {
          resources.pagingRestarts = (resources.pagingRestarts ?? 0) + 1;
          if (resources.pagingRestarts >= this.#limits.maxTranslationsPerRun)
            return finish('resource-limit');
          this.pruneTranslations();
          continue;
        }
        if (reason !== 'dispatch') {
          if (typeof reason !== 'string') {
            if (remaining === before) throw new Error('committed task switch did not retire');
            this.pruneTranslations();
            if (deliveryBudget === undefined) return finish(reason.taskEvent);
            eventKind = 'pending';
            servicingEvent = true;
            continue;
          }
          if (reason === 'pending-interrupt' || reason === 'halted') {
            eventKind = this.#engine.eventKind();
            queuedInterrupts = this.#engine.hasQueuedInterrupts();
            if (reason === 'pending-interrupt') {
              if (deliveryBudget === undefined) return finish(reason);
              if (eventKind !== 'pending-interrupt')
                throw new Error('core failed to accept eligible interrupt');
              servicingEvent = true;
              continue;
            }
            if (queuedInterrupts && this.#engine.externalBoundary() === 'pending-interrupt') {
              eventKind = this.#engine.eventKind();
              queuedInterrupts = this.#engine.hasQueuedInterrupts();
              if (deliveryBudget === undefined) return finish('pending-interrupt');
              if (eventKind !== 'pending-interrupt')
                throw new Error('core failed to accept eligible interrupt');
              servicingEvent = true;
            }
            continue;
          }
          if (
            (deliveryBudget !== undefined || boundaryObserved) &&
            this.#engine.stageException(reason)
          ) {
            eventKind = 'pending';
            servicingEvent = true;
            continue;
          }
          return finish(reason);
        }
        this.pruneTranslations();
        resources.dispatches++;
        dispatchedPc = current.rip;
      }
    } catch (cause) {
      return this[FAIL_RUN](cause);
    } finally {
      this[LEAVE_RUN](deliveryBudget);
    }
  }

  dispose(): void {
    if (this.#lifecycle === 'running') throw new Error('cpu is running');
    if (this.#lifecycle === 'disposed') return;
    const core = this.#core;
    const failed = this.#lifecycle === 'failed';
    this.#lifecycle = 'disposed';
    this.#core = undefined;
    this.#cache.clear();
    core?.releaseIo();
    if (!failed) core?.dispose();
  }
}
