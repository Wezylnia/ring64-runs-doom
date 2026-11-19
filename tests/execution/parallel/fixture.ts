import type {
  Cpu,
  ExecutionContext,
  MemoryBackingInput,
  MemoryLimits,
  MemoryMapping,
  PagingState,
  RunResult,
  State,
} from '../../../sdk/src/index.js';
import type { IoProvider } from '../../../sdk/src/embedding/io.js';
import { LIMITS, equal, hex, initial, same } from '../../support/fixture.js';

export { equal, hex, initial, same };
export const ENTRY = 0x1000n;
export const RAM = 0x4000n;

type AsyncMethods<K extends keyof Cpu> = {
  [P in K]: Cpu[P] extends (...args: infer A) => infer R ? (...args: A) => Promise<R> : never;
};
type RunOptions = Parameters<Cpu['run']>[0];

export type TestCpu = AsyncMethods<
  | 'readState'
  | 'setState'
  | 'readContext'
  | 'readDebugState'
  | 'setDebugState'
  | 'inspectTranslations'
  | 'readTimeState'
  | 'readPagingState'
  | 'setPagingState'
  | 'setX87State'
  | 'readX87State'
  | 'reset'
> & {
  readonly id: number;
  run(options: RunOptions): Promise<RunResult>;
  pause(): Promise<void>;
  requestInterrupt(vector: number): Promise<boolean>;
  requestNmi(): Promise<void>;
};

export type TestMachine = AsyncMethods<
  | 'readMemory'
  | 'writeMemory'
  | 'readPhysicalMemory'
  | 'writePhysicalMemory'
  | 'setMappings'
  | 'addBacking'
  | 'removeBacking'
  | 'reset'
  | 'dispose'
> & {
  readonly cpus: readonly TestCpu[];
  pause(): Promise<void>;
};

interface CpuOptions {
  entry: bigint;
  context?: ExecutionContext;
  paging?: PagingState;
}
export interface TestOptions {
  compiler: Uint8Array | WebAssembly.Module;
  memoryModel: 'linear' | 'ia32e-paged' | 'physical';
  memory: { backings: MemoryBackingInput[]; mappings: MemoryMapping[] };
  memoryLimits: MemoryLimits;
  limits: typeof LIMITS;
  cpus: CpuOptions[];
  sharedRamCapacity: number;
  io?: IoProvider;
}

// Runtime callers import the compiled API so dedicated workers can resolve their emitted modules.
export type MachineFactory = (options: TestOptions) => Promise<TestMachine>;

export function span(
  base: bigint,
  backing: string,
  offset: number,
  length: number,
  permissions: 'rx' | 'rw' | 'rwx' | 'r' = 'rw',
): MemoryMapping {
  return {
    base,
    backing,
    offset,
    length,
    permissions: {
      read: true,
      write: permissions.includes('w'),
      execute: permissions.includes('x'),
    },
  };
}

export function linearMachine(
  compiler: Uint8Array | WebAssembly.Module,
  code: string | Uint8Array,
  count = 2,
): TestOptions {
  const program = typeof code === 'string' ? hex(code) : code;
  return {
    compiler,
    memoryModel: 'linear',
    memory: {
      backings: [
        { id: 'code', bytes: program },
        { id: 'data', bytes: new Uint8Array(4096) },
      ],
      mappings: [span(ENTRY, 'code', 0, program.length, 'rx'), span(RAM, 'data', 0, 4096)],
    },
    memoryLimits: { maxBackings: 8, maxMappings: 16, maxBackingBytes: 0x20000 },
    limits: { ...LIMITS, maxTranslationsPerRun: 128 },
    sharedRamCapacity: 0x24000,
    cpus: Array.from({ length: count }, () => ({ entry: ENTRY })),
  };
}

export async function withMachine(
  factory: MachineFactory,
  options: TestOptions,
  run: (machine: TestMachine) => Promise<void>,
) {
  const machine = await factory(options);
  try {
    await run(machine);
  } finally {
    await machine.dispose();
  }
}

export function done(result: RunResult, stop: bigint, retired?: bigint): void {
  equal(result.reason, 'stop', 'reached stop');
  equal(result.state.rip, stop, 'intended guest endpoint');
  if (retired !== undefined) equal(result.retired, retired, 'exact instructions retired');
}

export function asBytes(...values: bigint[]): Uint8Array {
  const a = new Uint8Array(values.length * 8);
  const view = new DataView(a.buffer);
  for (const [i, value] of values.entries()) view.setBigUint64(8 * i, value, true);
  return a;
}

export async function words(machine: TestMachine, base: bigint, count: number): Promise<bigint[]> {
  const a = await machine.readMemory(base, count * 8);
  const view = new DataView(a.buffer, a.byteOffset, a.byteLength);
  return Array.from({ length: count }, (_, i) => view.getBigUint64(8 * i, true));
}

export function cpuState(ip = ENTRY, registers: Record<number, bigint> = {}): State {
  return initial(ip, registers, 2n);
}
