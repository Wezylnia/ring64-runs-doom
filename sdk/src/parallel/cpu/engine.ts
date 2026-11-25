import {
  BEGIN_RUN,
  CONSTRUCTION,
  FAIL_RUN,
  FINISH_RUN,
  LEAVE_RUN,
  Cpu,
  requireSimd,
  type RunOptions,
  type RunResult,
} from '../../cpu/cpu.js';
import { captureInitialContext, captureLimits, type Limits } from '../../cpu/options.js';
import { observeInterruptExit, taskResumeExit } from '../../cpu/run-events.js';
import { validateNativeProgress } from '../../cpu/run-progress.js';
import type { InstalledEntry } from '../../cpu/translations.js';
import { captureIo, type IoProvider } from '../../embedding/io.js';
import type { InitialMemory, MemoryMapping, MemoryModel } from '../../embedding/memory.js';
import type { ExecutionContext } from '../../state/system/context.js';
import { ParallelCore } from './core.js';
import type { BackingRecord } from '../memory/backings.js';
import type { Binding, Command } from '../protocol.js';

interface EngineOptions {
  compiler: WebAssembly.Module;
  memory: Omit<InitialMemory, 'backings'>;
  context?: ExecutionContext;
  limits: Limits;
  io?: IoProvider;
}

export async function createParallelEngine(
  options: EngineOptions,
  binding: Binding,
): Promise<ParallelCpu> {
  const began = performance.now();
  requireSimd();
  const limits = captureLimits(options.limits);
  const io = captureIo(options);
  const memory = options.memory;
  const context = captureInitialContext(options.context, memory.memoryModel);
  const core = await ParallelCore.createWorker(options.compiler, memory, context, io, binding);
  return new ParallelCpu(core, limits, performance.now() - began, memory.memoryModel);
}

export class ParallelCpu extends Cpu {
  readonly #core: ParallelCore;

  constructor(
    core: ParallelCore,
    limits: Limits,
    creationMs: number,
    memoryModel: MemoryModel | undefined,
  ) {
    super(CONSTRUCTION, core, limits, creationMs, memoryModel);
    this.#core = core;
  }

  get parallelOwner(): ParallelCore {
    return this.#core;
  }

  parallelAdmin(method: Command['method'], args: unknown[]): unknown {
    const core = this.parallelOwner;
    switch (method) {
      case 'addBacking':
        core.parallelAddBacking(args[0] as BackingRecord);
        break;
      case 'removeBacking':
        core.removeBacking(args[0] as string);
        break;
      case 'setMappings':
        core.setMappings(args[0] as MemoryMapping[]);
        this.pruneTranslations();
        break;
      case 'readMemory':
      case 'readPhysicalMemory':
        return core.readMemory(args[0] as bigint, args[1] as number);
      case 'writeMemory':
      case 'writePhysicalMemory':
        core.writeOwnedMemory(args[0] as bigint, args[1] as Uint8Array);
        this.pruneTranslations();
        break;
      case 'hostMemoryChanged':
        core.parallelHostMemoryChanged();
        this.pruneTranslations();
        break;
      default:
        throw new Error('unknown parallel administration command');
    }
  }

  async run(options: RunOptions): Promise<RunResult> {
    const run = this[BEGIN_RUN](options);
    const core = this.parallelOwner;
    const binding = core.parallelBinding;
    const { budget, stop, repeatBudget, deliveryBudget, resources, timings, cache, limits } = run;
    let remaining = budget;
    let remainingRepeatBudget = repeatBudget;
    let remainingDeliveryBudget = deliveryBudget;
    let admissionRestarts = 0;
    const restartAdmission = () => {
      if (admissionRestarts >= limits.maxTranslationsPerRun) return false;
      admissionRestarts++;
      return true;
    };
    const finish = (reason: string) =>
      this[FINISH_RUN](run, reason, remaining, remainingRepeatBudget, remainingDeliveryBudget);
    try {
      if (deliveryBudget !== undefined) core.configureDelivery(true);
      let servicingEvent = false;
      let eventKind = core.eventKind();
      let queuedInterrupts = core.hasQueuedInterrupts();
      if (core.taskRetry) {
        if (remaining === 0n) return finish('budget');
        if (stop !== undefined && core.current().rip === stop) return finish('stop');
        core.prepareEntry();
        const at = performance.now();
        const resumed = core.resumeTask();
        core.parallelFinishSlice();
        timings.executionMs += performance.now() - at;
        this.pruneTranslations();
        eventKind = core.eventKind();
        queuedInterrupts = core.hasQueuedInterrupts();
        const exit = taskResumeExit(resumed, deliveryBudget !== undefined);
        if (exit !== undefined) return finish(exit);
        servicingEvent = eventKind === 'pending' || eventKind === 'pending-interrupt';
      }
      execution: for (;;) {
        if (await binding.checkpoint(core)) {
          if (!servicingEvent && remaining === 0n) return finish('budget');
          if (!servicingEvent && stop !== undefined && core.current().rip === stop)
            return finish('stop');
          return finish('paused');
        }
        queuedInterrupts = core.hasQueuedInterrupts();
        if (eventKind === 'shutdown' && (deliveryBudget === undefined || !core.shutdownCanWake()))
          return finish('shutdown');
        const boundaryObserved = eventKind === 'ready' && core.prepareEntry();
        const pc = core.current().rip;
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
          const reason = core.deliverException();
          core.parallelFinishSlice();
          timings.executionMs += performance.now() - at;
          if (reason === 'shutdown') {
            eventKind = 'shutdown';
            servicingEvent = false;
            continue;
          }
          if (reason !== 'dispatch') return finish(reason);
          eventKind = core.eventKind();
          servicingEvent = eventKind === 'pending' || eventKind === 'pending-interrupt';
          this.pruneTranslations();
          resources.dispatches++;
          continue;
        }
        servicingEvent = false;
        if (queuedInterrupts || eventKind === 'halted' || eventKind === 'shutdown') {
          const reason = core.externalBoundary();
          if (reason === 'pending-interrupt') {
            const event = observeInterruptExit(core, reason, deliveryBudget !== undefined);
            if (event === 'pending-interrupt') return finish(event);
            ({ eventKind, queuedInterrupts, servicingEvent } = event);
            continue;
          }
          if (reason !== 'dispatch') return finish(reason);
        }
        let entry: InstalledEntry | undefined;
        let cached = false;
        let executionBegan = 0;
        let refusedEpoch: bigint | undefined;
        for (const candidate of cache.candidates(pc, boundaryObserved)) {
          executionBegan = performance.now();
          if (core.activate(candidate.artifact.metadata.owner)) {
            entry = candidate;
            cached = true;
            break;
          }
          timings.executionMs += performance.now() - executionBegan;
          if (!core.valid(candidate.artifact.metadata.owner)) cache.remove(candidate.artifact);
          const epoch = core.contextVersion();
          if (
            core.boundaryObserved() !== boundaryObserved ||
            (refusedEpoch !== undefined && refusedEpoch !== epoch)
          ) {
            if (!restartAdmission()) return finish('resource-limit');
            continue execution;
          }
          refusedEpoch = epoch;
        }
        if (!entry) {
          const probe = core.probe(pc);
          if (probe === 'paging-restart') {
            resources.pagingRestarts = (resources.pagingRestarts ?? 0) + 1;
            if (resources.pagingRestarts >= limits.maxTranslationsPerRun)
              return finish('resource-limit');
            this.pruneTranslations();
            continue;
          }
          if (probe !== 'dispatch') {
            if ((deliveryBudget !== undefined || boundaryObserved) && core.stageException(probe)) {
              eventKind = 'pending';
              servicingEvent = true;
              continue;
            }
            return finish(probe);
          }
          if (resources.compilations >= limits.maxTranslationsPerRun)
            return finish('resource-limit');
          resources.compilations++;
          let at = performance.now();
          const token = binding.imports.enter_exclusive();
          let output;
          try {
            output = core.compile(pc, limits);
            if (output) binding.watch(core, output.metadata.owner);
          } finally {
            binding.imports.leave_exclusive(token);
          }
          timings.translationMs += performance.now() - at;
          if (!output) return finish('resource-limit');
          resources.generatedBytes += output.bytes.length;
          if (!cache.canFit(output.bytes.length)) {
            core.release(output.metadata.owner);
            return finish('resource-limit');
          }
          let installed = false;
          try {
            at = performance.now();
            const module = await binding.compile(output.bytes);
            if (module === undefined || (await binding.checkpoint(core))) return finish('paused');
            queuedInterrupts = core.hasQueuedInterrupts();
            timings.browserCompileMs += performance.now() - at;
            const artifact = cache.installCompiled(core, module, output.metadata, timings);
            if (!artifact) continue;
            if (artifact === 'resource-limit') return finish(artifact);
            installed = true;
            entry = cache
              .candidates(pc, output.metadata.boundaryObserved)
              .find((candidate) => candidate.artifact === artifact);
            if (!entry) throw new Error('installed artifact does not own its requested entry');
          } finally {
            if (!installed) core.release(output.metadata.owner);
          }
          executionBegan = performance.now();
          if (!core.activate(entry.artifact.metadata.owner)) {
            timings.executionMs += performance.now() - executionBegan;
            if (!core.valid(entry.artifact.metadata.owner)) cache.remove(entry.artifact);
            if (!restartAdmission()) return finish('resource-limit');
            continue;
          }
        }
        const before = remaining;
        const repeatBefore = remainingRepeatBudget;
        const sliceBudget = binding.sliceLimit(remaining);
        const sliceRepeats = binding.sliceLimit(remainingRepeatBudget);
        const reason = core.execute(
          entry.artifact.run,
          entry.index,
          sliceBudget,
          stop,
          sliceRepeats,
        );
        core.parallelFinishSlice();
        timings.executionMs += performance.now() - executionBegan;
        cache.touch(entry.artifact);
        if (cached) resources.cacheHits++;
        resources.nativeEntries++;
        const current = core.current();
        validateNativeProgress(reason, sliceBudget, sliceRepeats, current);
        remaining = current.remaining + (before - sliceBudget);
        remainingRepeatBudget = current.repeatRemaining + (repeatBefore - sliceRepeats);
        if (
          reason === 'parallel-boundary' ||
          (reason === 'budget' && remaining !== 0n) ||
          (reason === 'repeat-budget' && remainingRepeatBudget !== 0n)
        )
          continue execution;
        if (reason === 'paging-restart') {
          resources.pagingRestarts = (resources.pagingRestarts ?? 0) + 1;
          if (resources.pagingRestarts >= limits.maxTranslationsPerRun)
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
            const event = observeInterruptExit(core, reason, deliveryBudget !== undefined);
            if (event === 'pending-interrupt') return finish(event);
            ({ eventKind, queuedInterrupts, servicingEvent } = event);
            continue;
          }
          if ((deliveryBudget !== undefined || boundaryObserved) && core.stageException(reason)) {
            eventKind = 'pending';
            servicingEvent = true;
            continue;
          }
          return finish(reason);
        }
        this.pruneTranslations();
        resources.dispatches++;
      }
    } catch (cause) {
      return this[FAIL_RUN](cause);
    } finally {
      this[LEAVE_RUN](deliveryBudget);
    }
  }
}
