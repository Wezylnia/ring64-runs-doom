import { BACKING_PAGE_BYTES } from './memory/backings.js';
import { MAX_SHARED_MEMORY64_PAGES } from './memory/ram.js';

const HEADER = 64;
const RECORD = 128;
const U64_MAX = (1n << 64n) - 1n;

export const STARTUP_WORD = 8;
export const SLOT = {
  active: 0,
  acknowledgement: 8,
  run: 16,
  pause: 24,
  irq: 32,
  nmi: 40,
  grant: 48,
  invalidations: 56,
  signal: 64,
  wake: 68,
  ioStatus: 72,
  ioResult: 76,
  ready: 80,
  round: 88,
  roundAck: 96,
} as const;

export const SIGNAL = { pause: 1, phase: 2, ingress: 4 } as const;

export interface SharedLayout {
  memory64: boolean;
  narrowRam?: true;
  cpus: number;
  ramBase: number;
  ramCapacity: number;
  guardBase: number;
  watchBase: number;
  pages: number;
}

export function sharedLayout(
  cpus: number,
  ramCapacity: number,
  maxSourceBytes: number,
  maxCachedArtifacts: number,
  memory64 = false,
): SharedLayout {
  if (
    !Number.isSafeInteger(ramCapacity) ||
    ramCapacity < BACKING_PAGE_BYTES ||
    ramCapacity % BACKING_PAGE_BYTES !== 0
  )
    throw new TypeError('sharedRamCapacity must be a positive backing page multiple');
  const guardBase = HEADER + cpus * RECORD;
  const watchBase = BigInt(guardBase) + BigInt(ramCapacity / 64) * 4n;
  const afterWatches = watchBase + BigInt(ramCapacity / 8);
  // A source byte can consume five paging entries and one executable byte range.
  const proofCapacity =
    BigInt(cpus) * (BigInt(maxCachedArtifacts) + 1n) * BigInt(maxSourceBytes) * 6n;
  const ramBase = ((afterWatches + proofCapacity * 24n + 65535n) / 65536n) * 65536n;
  const end = ramBase + BigInt(ramCapacity);
  if (memory64) {
    if (end > BigInt(MAX_SHARED_MEMORY64_PAGES) * 65536n)
      throw new RangeError('parallel RAM and metadata exceed the 16 GiB memory64 arena');
  } else if (end >= 1n << 32n) {
    throw new RangeError('parallel RAM and metadata exceed memory32');
  }
  return {
    memory64,
    cpus,
    ramBase: Number(ramBase),
    ramCapacity,
    guardBase,
    watchBase: Number(watchBase),
    pages: Number((end + 65535n) / 65536n),
  };
}

export function nextIdentity(previous: bigint): bigint {
  if (previous >= U64_MAX) throw new RangeError('parallel identity capacity exhausted');
  return previous + 1n;
}

export function cpuOffset(cpu: number): number {
  return HEADER + cpu * RECORD;
}

export class SharedControl {
  private readonly wide: BigUint64Array<SharedArrayBuffer>;
  private readonly words: Int32Array<SharedArrayBuffer>;

  constructor(memory: WebAssembly.Memory, cpus = 1) {
    const buffer: ArrayBuffer | SharedArrayBuffer = memory.buffer;
    if (!(buffer instanceof SharedArrayBuffer))
      throw new TypeError('parallel memory must be shared');
    const controlBytes = cpuOffset(cpus);
    this.wide = new BigUint64Array(buffer, 0, controlBytes / 8);
    this.words = new Int32Array(buffer, 0, controlBytes / 4);
  }

  get phase(): bigint {
    return Atomics.load(this.wide, 0);
  }

  get startups(): bigint {
    return Atomics.load(this.wide, STARTUP_WORD / 8);
  }

  initialize(): void {
    Atomics.store(this.wide, 0, 2n);
  }

  transition(closing: boolean): bigint {
    const previous = this.phase;
    if (Boolean(previous & 1n) === closing) throw new Error('invalid parallel phase transition');
    const next = nextIdentity(previous);
    Atomics.store(this.wide, 0, next);
    return next;
  }

  load(cpu: number, field: number): bigint {
    return Atomics.load(this.wide, (cpuOffset(cpu) + field) / 8);
  }

  store(cpu: number, field: number, value: bigint): void {
    Atomics.store(this.wide, (cpuOffset(cpu) + field) / 8, value);
  }

  compareExchange(cpu: number, field: number, previous: bigint, next: bigint): bigint {
    return Atomics.compareExchange(this.wide, (cpuOffset(cpu) + field) / 8, previous, next);
  }

  loadWord(cpu: number, field: number): number {
    return Atomics.load(this.words, (cpuOffset(cpu) + field) / 4);
  }

  storeWord(cpu: number, field: number, value: number): void {
    Atomics.store(this.words, (cpuOffset(cpu) + field) / 4, value);
  }

  signal(cpu: number, bits: number): void {
    Atomics.or(this.words, (cpuOffset(cpu) + SLOT.signal) / 4, bits);
    this.wake(cpu);
  }

  clearSignal(cpu: number, bits: number): void {
    Atomics.and(this.words, (cpuOffset(cpu) + SLOT.signal) / 4, ~bits);
  }

  wake(cpu: number): void {
    const index = (cpuOffset(cpu) + SLOT.wake) / 4;
    Atomics.add(this.words, index, 1);
    Atomics.notify(this.words, index);
  }

  wait(cpu: number, observed: number): void {
    Atomics.wait(this.words, (cpuOffset(cpu) + SLOT.wake) / 4, observed);
  }

  acquire(cpu: number, invalidation: bigint): boolean {
    const phase = this.phase;
    if (phase & 1n) return false;
    this.store(cpu, SLOT.active, phase);
    if (this.phase === phase && this.load(cpu, SLOT.invalidations) === invalidation) return true;
    this.store(cpu, SLOT.active, 0n);
    return false;
  }

  leave(cpu: number): void {
    this.store(cpu, SLOT.active, 0n);
  }
}
