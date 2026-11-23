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
  type MemoryModel,
} from '../embedding/memory.js';
import { capturePagingState, type PagingState } from '../state/system/paging.js';
import { sharedLayout } from './control.js';

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
  const memoryModel = options.memoryModel;
  if (memoryModel !== 'linear' && memoryModel !== 'ia32e-paged' && memoryModel !== 'physical')
    throw new TypeError('memoryModel must be linear, ia32e-paged or physical');
  const memoryLimits = captureMemoryLimits(options.memoryLimits);
  const memory = options.memory;
  const mappingInput = memory.mappings;
  if (!Array.isArray(mappingInput) || mappingInput.length > memoryLimits.maxMappings)
    throw new TypeError('invalid machine maps');
  const backings = captureBackings(memory.backings, memoryLimits);
  const mappings = captureMappings(mappingInput, memoryLimits, memoryModel);
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
  );
  const arenaBytes = backings.reduce(
    (used, { bytes }) => used + Math.ceil(bytes.length / BACKING_PAGE) * BACKING_PAGE,
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
  const compiler = options.compiler;
  if (!(compiler instanceof WebAssembly.Module) && !(compiler instanceof Uint8Array))
    throw new TypeError('compiler must be bytes or a WebAssembly.Module');
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

export interface BackingRecord {
  id: string;
  length: number;
  offset: number;
  incarnation: bigint;
}

const BACKING_PAGE = 4096;

export function findOffset(
  layout: { ramBase: number; ramCapacity: number },
  records: Iterable<BackingRecord>,
  length: number,
): number {
  let offset = layout.ramBase;
  const pageBytes = Math.ceil(length / BACKING_PAGE) * BACKING_PAGE;
  for (const entry of [...records].sort((a, b) => a.offset - b.offset)) {
    if (offset + pageBytes <= entry.offset) return offset;
    offset = entry.offset + Math.ceil(entry.length / BACKING_PAGE) * BACKING_PAGE;
  }
  if (offset + pageBytes > layout.ramBase + layout.ramCapacity)
    throw new RangeError('parallel backing arena capacity exceeded');
  return offset;
}

export function seedBacking(
  storeLane: (offset: number, value: bigint) => void,
  offset: number,
  bytes: Uint8Array,
): void {
  const input = new DataView(bytes.buffer, bytes.byteOffset, bytes.length);
  let i = 0;
  for (; i + 8 <= bytes.length; i += 8) storeLane(offset + i, input.getBigUint64(i, true));
  if (i === bytes.length) return;
  let tail = 0n;
  for (let j = i; j < bytes.length; j++) tail |= BigInt(bytes[j]!) << BigInt((j - i) * 8);
  storeLane(offset + i, tail);
}

export function requirePlane(model: MemoryModel, physical: boolean): void {
  if (physical ? model === 'linear' : model !== 'linear')
    throw new TypeError(
      physical ? 'physical or ia32e-paged memory required' : 'linear memory required',
    );
}
