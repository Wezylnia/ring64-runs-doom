import type { Cpu, Limits, RunResult } from '../cpu/cpu.js';
import type { IoWidth } from '../embedding/io.js';
import type { InitialMemory, MemoryModel } from '../embedding/memory.js';
import type { RamHelpers } from './memory/ram.js';
import type { SharedLayout } from './control.js';
import type { CpuInput } from './options.js';
import type { BackingRecord } from './memory/backings.js';

export const LOCAL_METHODS = [
  'readContext',
  'setContext',
  'readState',
  'setState',
  'setXmmState',
  'readXmmState',
  'readDebugState',
  'setDebugState',
  'readPagingState',
  'setPagingState',
  'readFastSystemState',
  'setFastSystemState',
  'readTimeState',
  'setTimeState',
  'readXcr0',
  'setXcr0',
  'readProtectionKeysState',
  'setProtectionKeysState',
  'readMxcsr',
  'setMxcsr',
  'readX87State',
  'setX87State',
  'inspectTranslations',
  'reset',
] as const;
export type LocalMethod = (typeof LOCAL_METHODS)[number];
export type RunOptions = Parameters<Cpu['run']>[0];
export type ParallelRunResult = RunResult;
export type Range = { offset: number; length: number };

export interface Start {
  kind: 'start';
  id: number;
  compiler: WebAssembly.Module;
  ramModule: WebAssembly.Module;
  shared: WebAssembly.Memory;
  layout: SharedLayout;
  model: MemoryModel;
  memory: Pick<InitialMemory, 'memoryLimits' | 'mappings'>;
  limits: Limits;
  state: CpuInput;
  backings: BackingRecord[];
  hasIo: boolean;
}

export interface Command {
  kind: 'command';
  sequence: bigint;
  method:
    | LocalMethod
    | 'run'
    | 'addBacking'
    | 'removeBacking'
    | 'setMappings'
    | 'readMemory'
    | 'writeMemory'
    | 'readPhysicalMemory'
    | 'writePhysicalMemory'
    | 'hostMemoryChanged';
  args: unknown[];
  permit?: bigint;
}

export type ToWorker =
  Start | Command | { kind: 'phase' } | { kind: 'invalidate'; sequence: bigint; ranges: Range[] };

export type FromWorker =
  | { kind: 'ready' }
  | { kind: 'ack'; phase: bigint }
  | { kind: 'round' }
  | { kind: 'result'; sequence: bigint; result: unknown }
  | { kind: 'rejected'; sequence: bigint; error: 'TypeError' | 'RangeError'; message: string }
  | { kind: 'fatal'; message: string }
  | { kind: 'exclusive'; request: bigint }
  | { kind: 'released'; request: bigint }
  | { kind: 'write'; range: Range }
  | { kind: 'watch'; owner: bigint; ranges: Range[] }
  | { kind: 'unwatch'; owner: bigint }
  | { kind: 'io'; sequence: bigint; ticket: bigint; port: number; width: IoWidth; value?: number };

export interface ParallelCore {
  parallelSetIngress(irq: number | null, nmi: boolean): void;
  parallelGetIngress(): { irq: number | null; nmiPending: boolean };
  parallelProof(owner: bigint): Range[];
  parallelInvalidate(offset: number, length: number): void;
  parallelAddBacking(backing: BackingRecord): void;
  parallelHostMemoryChanged(): void;
}

export interface Binding {
  readonly privateMemory: WebAssembly.Memory;
  readonly shared: WebAssembly.Memory;
  readonly ramImports: RamHelpers;
  readonly layout: SharedLayout;
  readonly backings: BackingRecord[];
  readonly signal: WebAssembly.Global;
  readonly imports: {
    enter_exclusive(): number;
    leave_exclusive(token: number): void;
    note_write(offset: number | bigint, length: number): void;
  };
  checkpoint(core: ParallelCore): Promise<boolean>;
  suspend(): void;
  compile(bytes: Uint8Array<ArrayBuffer>): Promise<WebAssembly.Module | undefined>;
  sliceLimit(budget: bigint): bigint;
  finishSlice(): void;
  activate(core: ParallelCore): Promise<void>;
  reconcile(core: ParallelCore): void;
  watch(core: ParallelCore, owner: bigint): void;
  unwatch(owner: bigint): void;
}
