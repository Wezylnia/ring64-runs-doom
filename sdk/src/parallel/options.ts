import type { ExecutionContext } from '../state/system/context.js';
import { copyMemoryInput, unsigned64 } from '../embedding/input.js';
import { captureInitialContext, captureLimits, type Limits } from '../cpu/options.js';
import type { CompilerSource } from '../wasm/core.js';
import {
  captureFastSystemState,
  zeroFastSystemState,
  type FastSystemState,
} from '../state/system/fast-system.js';
import { captureIo, type IoProvider } from '../embedding/io.js';
import {
  captureBackings,
  captureMappings,
  captureMemoryLimits,
  validateBackings,
  validateMappings,
  type MemoryBackingInput,
  type MemoryLimits,
  type MemoryMapping,
} from '../embedding/memory.js';
import { capturePagingState, type PagingState } from '../state/system/paging.js';
import { sharedLayout } from './control.js';
import { backingAllocationBytes } from './memory/backings.js';
import { usesParallelMemory64 } from './memory/ram.js';

interface Common {
  compiler: CompilerSource;
  limits: Limits;
  sharedRamCapacity: number;
  memory: { backings: MemoryBackingInput[]; mappings: MemoryMapping[] };
  memoryLimits: MemoryLimits;
  io?: IoProvider;
  mmio?: never;
}

interface LinearCpu {
  entry: bigint;
  context?: ExecutionContext;
  paging?: never;
  fastSystem?: never;
}

interface PagedCpu {
  entry: bigint;
  context?: ExecutionContext & { mode: 'long64' | 'compatibility' };
  paging: PagingState;
  fastSystem?: FastSystemState;
}

interface PhysicalCpu {
  entry: bigint;
  context: ExecutionContext;
  paging: PagingState;
  fastSystem?: FastSystemState;
}

export type MachineOptions = Common &
  (
    | { memoryModel: 'linear'; cpus: readonly LinearCpu[] }
    | { memoryModel: 'ia32e-paged'; cpus: readonly PagedCpu[] }
    | { memoryModel: 'physical'; cpus: readonly PhysicalCpu[] }
  );

export type CpuInput = {
  entry: bigint;
  context?: ExecutionContext;
  paging?: PagingState;
  fastSystem?: FastSystemState;
};

export function captureMachine(options: MachineOptions) {
  if ('mmio' in options) throw new TypeError('MMIO requires a standalone CPU');
  const compiler = options.compiler;
  if (!(compiler instanceof WebAssembly.Module) && !(compiler instanceof Uint8Array))
    throw new TypeError('compiler must be bytes or a WebAssembly.Module');
  // Capture all input before awaiting a byte-supplied compiler's actual profile.
  const memory64 = compiler instanceof WebAssembly.Module ? usesParallelMemory64(compiler) : true;
  const memoryModel = options.memoryModel;
  if (memoryModel !== 'linear' && memoryModel !== 'ia32e-paged' && memoryModel !== 'physical')
    throw new TypeError('memoryModel must be linear, ia32e-paged or physical');
  const memoryLimits = captureMemoryLimits(options.memoryLimits, memory64);
  const memory = options.memory;
  const mappingInput = memory.mappings;
  if (!Array.isArray(mappingInput) || mappingInput.length > memoryLimits.maxMappings)
    throw new TypeError('invalid machine maps');
  const backings = captureBackings(memory.backings, memoryLimits, undefined, memory64);
  const mappings = captureMappings(mappingInput, memoryLimits, memoryModel, memory64);
  validateMappings(mappings, validateBackings(backings, new Map(), memoryLimits));

  const limits = captureLimits(options.limits);
  const list = options.cpus;
  if (!Array.isArray(list) || list.length < 2 || list.length > 0xffffffff / 128)
    throw new TypeError('parallel machines require at least two vCPUs');
  const count = list.length;
  const layout = sharedLayout(
    count,
    options.sharedRamCapacity,
    limits.maxSourceBytes,
    limits.maxCachedArtifacts,
    memory64,
  );
  const arenaBytes = backings.reduce(
    (used, { bytes, byteLength }) => used + backingAllocationBytes(byteLength ?? bytes.length),
    0,
  );
  if (arenaBytes > layout.ramCapacity)
    throw new RangeError('parallel backing arena capacity exceeded');
  const cpus: CpuInput[] = [];
  for (let i = 0; i < count; i++) {
    const cpu = list[i] as LinearCpu | PagedCpu | PhysicalCpu;
    if (!cpu || typeof cpu !== 'object') throw new TypeError('invalid parallel CPU record');
    const entry = cpu.entry;
    unsigned64(entry, 'entry');
    const context = captureInitialContext(cpu.context, memoryModel);
    const settings: CpuInput = { entry, ...(context === undefined ? {} : { context }) };
    if (memoryModel === 'linear') {
      if ('paging' in cpu || 'fastSystem' in cpu)
        throw new TypeError('linear memory has no paging or fast-system bank');
    } else {
      settings.paging = capturePagingState(cpu.paging!);
      settings.fastSystem =
        'fastSystem' in cpu ? captureFastSystemState(cpu.fastSystem!) : zeroFastSystemState();
    }
    cpus.push(settings);
  }
  const io = captureIo(options);
  return {
    memoryModel,
    memoryLimits,
    backings,
    mappings,
    limits,
    layout,
    cpus,
    io,
    compiler: compiler instanceof WebAssembly.Module ? compiler : copyMemoryInput(compiler),
  };
}

export function memory32Layout(input: ReturnType<typeof captureMachine>) {
  if (!input.layout.memory64) return input.layout;
  captureMemoryLimits(input.memoryLimits);
  if (input.backings.some(({ byteLength }) => byteLength !== undefined))
    throw new TypeError('extended backing length requires memory64');
  // The captured mapping offsets already fit the checked backing lengths.
  return sharedLayout(
    input.layout.cpus,
    input.layout.ramCapacity,
    input.limits.maxSourceBytes,
    input.limits.maxCachedArtifacts,
  );
}
