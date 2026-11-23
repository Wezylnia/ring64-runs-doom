import { captureIo, type IoProvider } from '../embedding/io.js';
import { copyMemoryInput, unsigned64 } from '../embedding/input.js';
import { captureMmio, captureMmioRanges, type MmioProvider } from '../embedding/mmio.js';
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
} from '../state/system/context.js';
import { captureX87State, type X87State } from '../state/numeric/x87.js';
import { Core, usesMemory64, type CompilerSource } from '../wasm/core.js';
import type { GeneratedRun } from '../wasm/abi.js';
import { captureInitialContext, captureLimits, type Limits } from './options.js';
import {
  isRunReason,
  readRunDiagnostics,
  validateRunDiagnostic,
  type RunResult,
} from './run-result.js';
import { OwnerLinkProfile } from './owner-links.js';
import { LinearRegionTrace, PagedCorridor, RegionTrace } from './regions.js';
import { Translations, type Artifact, type InstalledEntry } from './translations.js';
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

export type { RunReason, RunResult } from './run-result.js';
export type { Limits } from './options.js';

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

export async function createCpu(options: Options): Promise<Cpu> {
  const began = performance.now();
  requireSimd();
  const limits = captureLimits(options.limits);
  const io = captureIo(options);
  const mmioInput = captureMmio(options);
  const compiler = options.compiler;
  // A byte-supplied compiler is identified after compilation in Core.create.
  const wide = compiler instanceof WebAssembly.Module ? usesMemory64(compiler) : true;
  const memory = captureInitialMemory(options, mmioInput?.count, wide);
  const mmio = mmioInput && captureMmioRanges(mmioInput, memory.memoryModel ?? 'linear');
  if (memory.memoryModel !== 'linear') {
    memory.paging = capturePagingState(options.paging!);
    memory.fastSystem =
      'fastSystem' in options ? captureFastSystemState(options.fastSystem!) : zeroFastSystemState();
  }
  const context = captureInitialContext(options.context, memory.memoryModel);
  const core =
    mmio === undefined
      ? await Core.create(compiler, memory, context, io)
      : await Core.create(compiler, memory, context, io, mmio);
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
    new Map(memory.backings.map(({ id, bytes, byteLength }) => [id, byteLength ?? bytes.length])),
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

  protected pruneTranslations(force = false): void {
    try {
      const engine = this.#engine;
      if (engine.takeSourceInvalidation?.() !== false || force)
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
    const captured = captureBackings(
      [input],
      this.#memoryLimits,
      this.#backings,
      this.#engine.memory64,
    );
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
    const captured = captureMappings(
      mappings,
      this.#memoryLimits,
      this.#memoryModel,
      this.#engine.memory64,
    );
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
    const diagnostics = readRunDiagnostics(this.#engine, reason);
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
      ...diagnostics,
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

  async #compileFlatTrace(
    artifact: Artifact,
    pc: bigint,
    maxArtifactBytes: number,
    resources: Pick<RunResult['resources'], 'compilations' | 'generatedBytes'>,
    timings: RunResult['timings'],
  ): Promise<{ module: WebAssembly.Module; bytes: number } | undefined> {
    if (maxArtifactBytes <= 0 || resources.compilations >= this.#limits.maxTranslationsPerRun)
      return;
    const engine = this.#engine;
    const started = performance.now();
    resources.compilations++;
    const limits = { ...this.#limits, maxArtifactBytes };
    const { entries, dependencies, contextVersion } = artifact.metadata;
    const output = engine.compile(pc, limits, entries, 'flat-trace');
    timings.translationMs += performance.now() - started;
    if (!output) return;
    resources.generatedBytes += output.bytes.length;
    try {
      if (
        output.metadata.entries.length !== entries.length ||
        output.metadata.entries.some((point, index) => point !== entries[index]) ||
        output.metadata.dependencies.length !== dependencies.length ||
        output.metadata.dependencies.some(
          ({ start, length }, index) =>
            start !== dependencies[index]!.start || length !== dependencies[index]!.length,
        ) ||
        output.metadata.contextVersion !== contextVersion
      )
        return;
      const began = performance.now();
      const module = await Core.compileArtifact(output.bytes);
      timings.browserCompileMs += performance.now() - began;
      return { module, bytes: output.bytes.length };
    } finally {
      // The trace clone has the installed owner's exact entries and source
      // context. Its execution uses that owner, which keeps source authority.
      engine.release(output.metadata.owner);
    }
  }

  async #tryRegion(
    region:
      | RegionTrace
      | LinearRegionTrace
      | {
          boundaryObserved: boolean;
          points: bigint[];
          entrances: Set<bigint>;
          flat: boolean;
        },
    resources: Pick<RunResult['resources'], 'compilations' | 'generatedBytes'>,
    timings: RunResult['timings'],
  ): Promise<void> {
    const engine = this.#engine;
    const cache = this.#cache.resources();
    const maxArtifactBytes = Math.min(
      this.#limits.maxArtifactBytes,
      this.#limits.maxCachedBytes - cache.cachedBytes,
    );
    if (
      resources.compilations >= this.#limits.maxTranslationsPerRun ||
      cache.cachedArtifacts >= this.#limits.maxCachedArtifacts ||
      maxArtifactBytes <= 0 ||
      engine.boundaryObserved() !== region.boundaryObserved ||
      engine.hasQueuedInterrupts()
    )
      return;

    // Owners bound the sampling metadata. The compiler decodes each selected
    // target again against its current instruction-fetch authority.
    const limits = { ...this.#limits, maxArtifactBytes };
    const tracked = 'points' in region;
    const anchor = tracked ? region.points.find((point) => region.entrances.has(point)) : undefined;
    const selected = tracked
      ? anchor === undefined
        ? undefined
        : { entry: anchor, seeds: region.points }
      : region.choose(limits, (owner) => engine.valid(owner));
    if (!selected) return;
    resources.compilations++;
    const started = performance.now();
    const output = engine.compile(selected.entry, limits, selected.seeds, tracked);
    timings.translationMs += performance.now() - started;
    if (!output) return;
    resources.generatedBytes += output.bytes.length;
    let installed = false;
    try {
      let preferred: Set<bigint> | undefined;
      if (tracked) {
        const entries = new Set(output.metadata.entries);
        preferred = new Set(
          output.metadata.entries.filter((point) => {
            if (!region.entrances.has(point)) return false;
            if (!region.flat) return true;
            // A short capture can miss parts of an already learned call region.
            const current = this.#cache.preferred(point, region.boundaryObserved);
            return !current || current.metadata.entries.every((entry) => entries.has(entry));
          }),
        );
      } else if (region instanceof LinearRegionTrace) {
        preferred = new Set([selected.entry]);
      }
      if (tracked && preferred!.size < 2) return;
      const began = performance.now();
      const module = await Core.compileArtifact(output.bytes);
      timings.browserCompileMs += performance.now() - began;
      const ready = this.#cache.installCompiled(
        engine,
        module,
        output.metadata,
        timings,
        preferred,
      );
      installed = ready !== undefined && ready !== 'resource-limit';
      if (tracked && ready && ready !== 'resource-limit') this.#sampledPagedCorridors.add(ready);
    } finally {
      if (!installed) engine.release(output.metadata.owner);
    }
  }

  readonly #sampledLinearOwners = new WeakSet<Artifact>();
  #hasLinearCallOwner = false;
  #flatLinksReady = false;
  // Short frames can interrupt capture. An owner pays for one attempt per lifetime.
  readonly #sampledPagedCorridors = new WeakSet<Artifact>();
  #flatTraceReady = false;

  async run(options: RunOptions): Promise<RunResult> {
    const run = this[BEGIN_RUN](options);
    const { budget, stop, repeatBudget, deliveryBudget, resources, timings } = run;
    let remaining = budget;
    let remainingRepeatBudget = repeatBudget;
    let remainingDeliveryBudget = deliveryBudget;
    let cancelPagedTrace: (() => void) | undefined;
    let admissionRestarts = 0;
    const restartAdmission = () => {
      if (admissionRestarts >= this.#limits.maxTranslationsPerRun) return false;
      admissionRestarts++;
      return true;
    };
    let rememberFlatPressure = false;
    let linkProfile: OwnerLinkProfile | undefined;
    const finish = (reason: string) => {
      if (
        linkProfile &&
        !this.#cache.links &&
        resources.compilations === 0 &&
        (reason === 'stop' || reason === 'budget')
      ) {
        const { cachedArtifacts, cachedBytes } = this.#cache.resources();
        if (cachedArtifacts < this.#limits.maxCachedArtifacts) {
          const bytes = linkProfile.install(
            this.#engine,
            this.#cache,
            Math.min(this.#limits.maxArtifactBytes, this.#limits.maxCachedBytes - cachedBytes),
            timings,
          );
          if (bytes !== 0) {
            resources.compilations++;
            resources.generatedBytes += bytes;
          }
        }
      }
      const result = this[FINISH_RUN](
        run,
        reason,
        remaining,
        remainingRepeatBudget,
        remainingDeliveryBudget,
      );
      const entries = result.resources.nativeEntries;
      this.#flatLinksReady =
        rememberFlatPressure &&
        (reason === 'stop' || reason === 'budget') &&
        result.resources.cacheHits >= 256 &&
        result.resources.dispatches >= 256;
      this.#flatTraceReady =
        rememberFlatPressure &&
        (reason === 'stop' || reason === 'budget') &&
        entries >= 64 &&
        result.retired < 2n * BigInt(entries) * BigInt(this.#limits.maxInstructions);
      return result;
    };
    try {
      const engine = this.#engine;
      if (deliveryBudget !== undefined) engine.configureDelivery(true);
      let servicingEvent = false;
      // Idle-only intake keeps queues stable except at explicit acceptance boundaries.
      let eventKind = engine.eventKind();
      let queuedInterrupts = engine.hasQueuedInterrupts();
      if (engine.taskRetry) {
        if (remaining === 0n) return finish('budget');
        if (stop !== undefined && engine.current().rip === stop) return finish('stop');
        engine.prepareEntry();
        const at = performance.now();
        const resumed = engine.resumeTask();
        timings.executionMs += performance.now() - at;
        this.pruneTranslations();
        eventKind = engine.eventKind();
        queuedInterrupts = engine.hasQueuedInterrupts();
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
      const pagedRegionsSupported =
        deliveryBudget === undefined && !queuedInterrupts && engine.supportsRegionCompilation;
      const linearRegionsSupported =
        deliveryBudget === undefined && !queuedInterrupts && engine.supportsLinearRegionCompilation;
      rememberFlatPressure = linearRegionsSupported && engine.supportsFlatRegionTrace;
      // Up to four temporary owners and one promotion can require compilation.
      const flatTracing =
        rememberFlatPressure && this.#flatTraceReady && this.#limits.maxTranslationsPerRun >= 6;
      let region: RegionTrace | undefined;
      let corridor =
        this.#limits.maxInstructions >= 32 &&
        (flatTracing || (pagedRegionsSupported && engine.supportsPagedRegionTrace))
          ? new PagedCorridor()
          : undefined;
      let capture:
        | {
            page: bigint;
            version: bigint;
            retired: bigint;
            entrances: Set<bigint>;
            aliases: Map<Artifact, GeneratedRun>;
            traceBytes: number;
          }
        | undefined;
      const abortCapture = () => {
        cancelPagedTrace?.();
        cancelPagedTrace = undefined;
        capture = undefined;
      };
      let sampling = pagedRegionsSupported
        ? 'paged'
        : linearRegionsSupported && this.#hasLinearCallOwner
          ? 'linear'
          : undefined;
      let linearRegion: LinearRegionTrace | undefined;
      let saturatedDispatches = 0;
      const linkEligible =
        linearRegionsSupported &&
        (this.#hasLinearCallOwner || this.#flatLinksReady) &&
        !flatTracing &&
        engine.supportsOwnerLinks;
      if (linkEligible && !this.#cache.links && this.#cache.resources().cachedArtifacts > 0)
        linkProfile = new OwnerLinkProfile();
      const corridorDispatchLimit = 512;
      execution: for (;;) {
        if (eventKind === 'shutdown' && (deliveryBudget === undefined || !engine.shutdownCanWake()))
          return finish('shutdown');
        const boundaryObserved = eventKind === 'ready' && engine.prepareEntry();
        // boundary preparation clears diagnostics without changing rip.
        const pc = dispatchedPc ?? engine.current().rip;
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
          const reason = engine.deliverException();
          timings.executionMs += performance.now() - at;
          if (reason === 'shutdown') {
            eventKind = 'shutdown';
            servicingEvent = false;
            continue;
          }
          if (reason !== 'dispatch') return finish(reason);
          eventKind = engine.eventKind();
          servicingEvent = eventKind === 'pending' || eventKind === 'pending-interrupt';
          this.pruneTranslations();
          resources.dispatches++;
          continue;
        }
        servicingEvent = false;
        if (queuedInterrupts || eventKind === 'halted' || eventKind === 'shutdown') {
          const reason = engine.externalBoundary();
          if (reason === 'pending-interrupt') {
            eventKind = engine.eventKind();
            queuedInterrupts = engine.hasQueuedInterrupts();
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
        // Let an armed flat trace learn the ordinary corridor before using covered entries.
        const useCoveredEntrance =
          linearRegionsSupported &&
          !boundaryObserved &&
          !capture &&
          resources.compilations === 0 &&
          (!flatTracing || resources.dispatches >= corridorDispatchLimit);
        for (const candidate of this.#cache.candidates(pc, boundaryObserved, useCoveredEntrance)) {
          executionBegan = performance.now();
          if (engine.activate(candidate.artifact.metadata.owner)) {
            entry = candidate;
            cached = true;
            break;
          }
          timings.executionMs += performance.now() - executionBegan;
          if (!engine.valid(candidate.artifact.metadata.owner))
            this.#cache.remove(candidate.artifact);
          const epoch = engine.contextVersion();
          if (
            engine.boundaryObserved() !== boundaryObserved ||
            (refusedEpoch !== undefined && refusedEpoch !== epoch)
          ) {
            if (!restartAdmission()) return finish('resource-limit');
            continue execution;
          }
          refusedEpoch = epoch;
        }
        if (!entry) {
          if (flatTracing && capture) abortCapture();
          const probe = engine.probe(pc);
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
              engine.stageException(probe)
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
          const output = engine.compile(pc, this.#limits);
          timings.translationMs += performance.now() - at;
          if (!output) return finish('resource-limit');
          if (
            linearRegionsSupported &&
            !this.#hasLinearCallOwner &&
            output.metadata.hasIndirectCall
          ) {
            this.#hasLinearCallOwner = true;
            sampling = 'linear';
          }
          resources.generatedBytes += output.bytes.length;
          if (!this.#cache.canFit(output.bytes.length)) {
            engine.release(output.metadata.owner);
            return finish('resource-limit');
          }
          let installed = false;
          try {
            at = performance.now();
            const module = await Core.compileArtifact(output.bytes);
            timings.browserCompileMs += performance.now() - at;
            const artifact = this.#cache.installCompiled(engine, module, output.metadata, timings);
            if (!artifact) continue;
            if (artifact === 'resource-limit') return finish(artifact);
            installed = true;
            entry = this.#cache
              .candidates(pc, output.metadata.boundaryObserved)
              .find((candidate) => candidate.artifact === artifact);
            if (!entry) throw new Error('installed artifact does not own its requested entry');
          } finally {
            if (!installed) engine.release(output.metadata.owner);
          }
          executionBegan = performance.now();
          if (!engine.activate(entry.artifact.metadata.owner)) {
            timings.executionMs += performance.now() - executionBegan;
            if (!engine.valid(entry.artifact.metadata.owner)) this.#cache.remove(entry.artifact);
            if (!restartAdmission()) return finish('resource-limit');
            continue;
          }
        }
        const before = remaining;
        const repeatBefore = remainingRepeatBudget;
        let nativeBudget = before;
        let withheld = 0n;
        let native = entry.artifact.run;
        if (capture) {
          const artifact = entry.artifact;
          let alias = capture.aliases.get(artifact);
          let module =
            !flatTracing && alias === undefined && capture.aliases.size < 8
              ? this.#cache.traceModule(artifact)
              : undefined;
          if (
            flatTracing &&
            !boundaryObserved &&
            !alias &&
            capture.aliases.size < 4 &&
            pc >> 12n === capture.page &&
            engine.contextVersion() === capture.version &&
            eventKind === 'ready'
          ) {
            const cache = this.#cache.resources();
            if (cache.cachedArtifacts + capture.aliases.size < this.#limits.maxCachedArtifacts) {
              const maxBytes = Math.min(
                this.#limits.maxArtifactBytes,
                96 * 1024 - capture.traceBytes,
                this.#limits.maxCachedBytes - cache.cachedBytes - capture.traceBytes,
              );
              const started = performance.now();
              const traced = await this.#compileFlatTrace(
                artifact,
                pc,
                maxBytes,
                resources,
                timings,
              );
              executionBegan += performance.now() - started;
              if (traced) {
                capture.traceBytes += traced.bytes;
                module = traced.module;
              }
              if (!engine.valid(artifact.metadata.owner)) {
                abortCapture();
                this.#cache.remove(artifact);
                if (!restartAdmission()) return finish('resource-limit');
                continue execution;
              }
            }
          }
          if (
            pc >> 12n !== capture.page ||
            engine.contextVersion() !== capture.version ||
            (!alias && !module) ||
            (flatTracing && boundaryObserved) ||
            eventKind !== 'ready'
          ) {
            abortCapture();
          } else {
            if (!alias) {
              const began = performance.now();
              alias = engine.instantiatePagedRegionTrace(module!, boundaryObserved);
              const elapsed = performance.now() - began;
              timings.instantiationMs += elapsed;
              executionBegan += elapsed;
              capture.aliases.set(artifact, alias);
              this.#sampledPagedCorridors.add(artifact);
            }
            capture.entrances.add(pc);
            native = alias;
            const sampleBudget = 2048n - capture.retired;
            if (sampleBudget < nativeBudget) {
              nativeBudget = sampleBudget;
              withheld = before - sampleBudget;
            }
          }
        }
        const eligible =
          linkEligible && eventKind === 'ready' && !capture && useCoveredEntrance && cached;
        if (eligible) linkProfile?.record(pc, entry);
        const links = eligible ? this.#cache.links : undefined;
        const linkedRun = links?.run(entry.artifact);
        const nativeReason = engine.execute(
          linkedRun ?? native,
          entry.index,
          nativeBudget,
          stop,
          remainingRepeatBudget,
        );
        timings.executionMs += performance.now() - executionBegan;
        this.#cache.touch(entry.artifact);
        if (cached) resources.cacheHits++;
        resources.nativeEntries++;
        const receipt = linkedRun && links!.receipt(this.#cache);
        if (receipt) {
          resources.nativeEntries += receipt.continuations;
          resources.cacheHits += receipt.continuations;
          resources.dispatches += receipt.continuations;
          // The original owner and final RIP do not describe the individual edges.
          if (receipt.continuations > 0) sampling = undefined;
        }
        const current = engine.current();
        remaining = withheld === 0n ? current.remaining : current.remaining + withheld;
        remainingRepeatBudget = current.repeatRemaining;
        // The alias may yield at the sample budget without ending the public run.
        const traceYield = nativeReason === 'budget' && withheld > 0n;
        const reason = traceYield ? 'dispatch' : nativeReason;
        if (
          current.remaining > nativeBudget ||
          remainingRepeatBudget > repeatBefore ||
          (reason === 'dispatch' &&
            remaining === before &&
            remainingRepeatBudget === repeatBefore) ||
          (nativeReason === 'budget' && current.remaining !== 0n) ||
          (reason === 'repeat-budget' && (remainingRepeatBudget !== 0n || remaining === 0n))
        ) {
          throw new Error('generated execution violated its progress contract');
        }
        if (capture && reason !== 'dispatch') abortCapture();
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
            eventKind = engine.eventKind();
            queuedInterrupts = engine.hasQueuedInterrupts();
            if (reason === 'pending-interrupt') {
              if (deliveryBudget === undefined) return finish(reason);
              if (eventKind !== 'pending-interrupt')
                throw new Error('core failed to accept eligible interrupt');
              servicingEvent = true;
              continue;
            }
            if (queuedInterrupts && engine.externalBoundary() === 'pending-interrupt') {
              eventKind = engine.eventKind();
              queuedInterrupts = engine.hasQueuedInterrupts();
              if (deliveryBudget === undefined) return finish('pending-interrupt');
              if (eventKind !== 'pending-interrupt')
                throw new Error('core failed to accept eligible interrupt');
              servicingEvent = true;
            }
            continue;
          }
          if ((deliveryBudget !== undefined || boundaryObserved) && engine.stageException(reason)) {
            eventKind = 'pending';
            servicingEvent = true;
            continue;
          }
          return finish(reason);
        }
        this.pruneTranslations(receipt ? receipt.invalidated : false);
        resources.dispatches++;
        dispatchedPc = current.rip;
        if (resources.dispatches >= corridorDispatchLimit) corridor = undefined;
        if (capture) {
          capture.retired += before - remaining;
          if (current.rip >> 12n !== capture.page || engine.contextVersion() !== capture.version) {
            abortCapture();
          } else if (capture.retired >= 2048n) {
            const { retired, points } = engine.finishPagedRegionTrace(this.#limits.maxInstructions);
            const { aliases, entrances } = capture;
            capture = undefined;
            cancelPagedTrace = undefined;
            if (
              retired === 2048 &&
              points.length >= 16 &&
              current.rip !== stop &&
              [...aliases.keys()].every((artifact) => engine.valid(artifact.metadata.owner))
            ) {
              entrances.add(current.rip);
              aliases.clear();
              await this.#tryRegion(
                { boundaryObserved, points, entrances, flat: flatTracing },
                resources,
                timings,
              );
            }
          }
        } else if (
          corridor &&
          // Leave newly discovered code on the ordinary first-run path.
          resources.compilations === 0 &&
          remaining >= 2048n &&
          !this.#sampledPagedCorridors.has(entry.artifact) &&
          (flatTracing || this.#cache.traceModule(entry.artifact)) &&
          corridor.sample(pc, current.rip, entry.artifact)
        ) {
          corridor = undefined;
          const cache = this.#cache.resources();
          if (
            cache.cachedArtifacts < this.#limits.maxCachedArtifacts &&
            cache.cachedBytes < this.#limits.maxCachedBytes &&
            current.rip !== stop &&
            engine.beginPagedRegionTrace(current.rip)
          ) {
            this.#sampledPagedCorridors.add(entry.artifact);
            cancelPagedTrace = () => {
              engine.finishPagedRegionTrace(0);
            };
            capture = {
              page: current.rip >> 12n,
              version: engine.contextVersion(),
              retired: 0n,
              entrances: new Set(),
              aliases: new Map(),
              traceBytes: 0,
            };
          }
        }
        if (traceYield) continue execution;
        if (sampling) {
          if (eventKind !== 'ready') {
            sampling = undefined;
          } else if (sampling === 'paged') {
            const metadata = entry.artifact.metadata;
            region ??= new RegionTrace(pc, metadata);
            if (region.sample(pc, current.rip, entry)) {
              sampling = undefined;
              if (!capture) await this.#tryRegion(region, resources, timings);
            }
          } else if (!capture && resources.dispatches >= 16) {
            if (boundaryObserved || this.#sampledLinearOwners.has(entry.artifact)) {
              sampling = undefined;
            } else if (entry.artifact.metadata.instructionCount >= this.#limits.maxInstructions) {
              // A run of full owners cannot supply a complete child under this cap.
              if (++saturatedDispatches === 16) sampling = undefined;
            } else {
              saturatedDispatches = 0;
              linearRegion ??= new LinearRegionTrace();
              if (linearRegion.sample(pc, current.rip, entry)) {
                sampling = undefined;
                linearRegion.checked(this.#sampledLinearOwners);
                await this.#tryRegion(linearRegion, resources, timings);
              }
            }
          }
        }
      }
    } catch (cause) {
      return this[FAIL_RUN](cause);
    } finally {
      cancelPagedTrace?.();
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
