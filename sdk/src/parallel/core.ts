import { IoBridge, type IoBinding } from '../embedding/io.js';
import type { InitialMemory } from '../embedding/memory.js';
import { readInterrupts, writeInterrupts, type InterruptState } from '../state/execution/events.js';
import type { ExecutionContext } from '../state/system/context.js';
import type { GeneratedRun, ParallelCoreExports } from '../wasm/abi.js';
import {
  Core,
  backingKey,
  captureMemoryProfile,
  instantiateArtifact,
  mappingInput,
  writeInput,
} from '../wasm/core.js';
import type { BackingRecord } from './options.js';
import type { Binding, Range } from './protocol.js';

export class ParallelCore extends Core {
  readonly #parallelExports: ParallelCoreExports;

  private constructor(
    exports: ParallelCoreExports,
    isPaged: boolean,
    physicalControls: boolean,
    io: IoBridge,
    readonly parallelBinding: Binding,
  ) {
    super(exports, isPaged, physicalControls, io);
    this.#parallelExports = exports;
    // The parallel Wasm emitter extends the ordinary exits with its slice boundary.
    this.setExitReason(29, 'parallel-boundary');
  }

  static async createWorker(
    compiler: WebAssembly.Module,
    memory: Omit<InitialMemory, 'backings'>,
    context: ExecutionContext | undefined,
    ioBinding: IoBinding | undefined,
    binding: Binding,
  ): Promise<ParallelCore> {
    const { isPaged, physicalControls, metadata } = captureMemoryProfile(memory, context);
    const io = new IoBridge(ioBinding);
    try {
      const instance = await WebAssembly.instantiate(compiler, {
        io: io.imports,
        env: { memory: binding.privateMemory },
        ring64_ram: { ...binding.ramImports, ...binding.imports },
      });
      const exports = instance.exports as ParallelCoreExports;
      if (
        typeof exports.core_parallel_abi !== 'function' ||
        ![1, 2, 3, 4, 5, 6].includes(exports.core_parallel_abi()) ||
        exports.memory !== binding.privateMemory
      )
        throw new Error('parallel core ABI mismatch');

      ParallelCore.validateExports(exports, isPaged, physicalControls);
      const metadataLength = writeInput(exports, {
        ...metadata,
        memoryLimits: memory.memoryLimits,
        shared: binding.layout,
        backings: binding.backings.map(({ id, length, offset, incarnation }) => ({
          id: backingKey(id),
          length,
          offset,
          incarnation: incarnation.toString(),
        })),
        mappings: mappingInput(memory.mappings),
      });
      return ParallelCore.initialize(
        exports,
        memory.entry,
        metadataLength,
        io,
        physicalControls ? undefined : context,
        () => new ParallelCore(exports, isPaged, physicalControls, io, binding),
      );
    } catch (error) {
      io.release();
      throw error;
    }
  }

  override instantiate(module: WebAssembly.Module, observed = false): GeneratedRun {
    const binding = this.parallelBinding;
    const exports = this.#parallelExports;
    const env = this.artifactImports(observed);
    // Older supported parallel ABI versions omit newer imports.
    Object.assign(env, {
      shared_ram: binding.shared,
      parallel_signal: binding.signal,
      parallel_begin: binding.imports.enter_exclusive,
      parallel_end: binding.imports.leave_exclusive,
      parallel_resolve: exports.core_parallel_resolve,
      parallel_vector_dword: exports.core_parallel_vector_dword,
      parallel_vector_dword_span: exports.core_parallel_vector_dword_span,
      parallel_dword_window: exports.core_parallel_dword_window,
      parallel_byte_store_span: exports.core_parallel_byte_store_span,
      parallel_guard: exports.core_parallel_guard,
      parallel_unguard: exports.core_parallel_unguard,
    } satisfies Record<string, WebAssembly.ImportValue | undefined>);
    return instantiateArtifact(module, env);
  }

  override release(owner: bigint): void {
    this.parallelBinding.unwatch(owner);
    super.release(owner);
  }

  parallelGetIngress(): InterruptState {
    const layout = this.interruptLayout;
    return readInterrupts(this.stateView(), layout, layout.offset);
  }

  parallelSetIngress(irq: number | null, nmiPending: boolean): void {
    const view = this.stateView();
    const layout = this.interruptLayout;
    const previous = this.parallelGetIngress();
    writeInterrupts(
      new DataView(view.buffer, view.byteOffset + layout.offset, layout.bytes),
      layout,
      { ...previous, irq, nmiPending },
    );
  }

  parallelProof(owner: bigint): Range[] {
    if (this.#parallelExports.core_parallel_proof(owner) !== 0)
      throw new Error('parallel core refused source proof');
    const bytes = this.output();
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.length);
    if (view.byteLength % 8 !== 0) throw new Error('malformed parallel source proof');
    const ranges: Range[] = [];
    for (let at = 0; at < view.byteLength; at += 8)
      ranges.push({ offset: view.getUint32(at, true), length: view.getUint32(at + 4, true) });
    return ranges;
  }

  parallelInvalidate(offset: number, length: number): void {
    this.#parallelExports.core_parallel_invalidate(offset, length);
  }

  parallelAddBacking({ id, length, offset, incarnation }: BackingRecord): void {
    writeInput(this.#parallelExports, backingKey(id));
    this.memoryResult(this.#parallelExports.core_parallel_add_backing(length, offset, incarnation));
  }

  parallelHostMemoryChanged(): void {
    this.#parallelExports.core_parallel_admin_changed();
  }

  parallelFinishSlice(): void {
    if (this.isPaged) this.#parallelExports.core_parallel_finish_slice();
    this.parallelBinding.finishSlice();
  }
}
