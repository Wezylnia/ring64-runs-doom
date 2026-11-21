import { byteLength, unsigned64 } from './input.js';
import type { IoBridge } from './io.js';
import { validateMappedSpan, type MemoryModel } from './memory.js';

export type MmioWidth = 8 | 16 | 32 | 64;

export interface MmioRange {
  base: bigint;
  length: number;
}

export interface MmioProvider {
  ranges: readonly MmioRange[];
  read(address: bigint, width: MmioWidth): bigint;
  write(address: bigint, width: MmioWidth, value: bigint): void;
}

type MmioInput = MmioBinding & { count: number };

export function captureMmio(options: { mmio?: MmioProvider }): MmioInput | undefined {
  if (!('mmio' in options)) return undefined;
  const receiver = options.mmio;
  if (typeof receiver !== 'object' || receiver === null || Array.isArray(receiver))
    throw new TypeError('mmio must be a synchronous provider object');
  const ranges = receiver.ranges;
  if (!Array.isArray(ranges)) throw new TypeError('mmio requires an array of ranges');
  const count = ranges.length;
  byteLength(count, 'MMIO range count');
  if (count === 0) throw new TypeError('MMIO ranges must be nonempty');
  const read = receiver.read;
  const write = receiver.write;
  if (typeof read !== 'function' || typeof write !== 'function')
    throw new TypeError('mmio requires read and write methods');
  return { receiver, ranges, count, read, write };
}

export function captureMmioRanges(input: MmioInput, memoryModel: MemoryModel): MmioBinding {
  const ranges: MmioRange[] = [];
  for (let index = 0; index < input.count; index++) {
    const range = input.ranges[index];
    if (typeof range !== 'object' || range === null || Array.isArray(range))
      throw new TypeError('invalid MMIO range');
    const base = range.base;
    const length = range.length;
    unsigned64(base, 'MMIO range base');
    byteLength(length, 'MMIO range length');
    if (length === 0) throw new TypeError('MMIO ranges must be nonempty');
    validateMappedSpan(base, length, memoryModel, 'MMIO range');
    ranges.push({ base, length });
  }
  return { receiver: input.receiver, ranges, read: input.read, write: input.write };
}

export interface MmioBinding {
  receiver: MmioProvider;
  ranges: readonly MmioRange[];
  read: MmioProvider['read'];
  write: MmioProvider['write'];
}

function unconfiguredMmio(): never {
  throw new Error('MMIO provider is not configured');
}

export const unconfiguredMmioImports = Object.freeze({
  read: unconfiguredMmio,
  write: unconfiguredMmio,
});

export class MmioBridge {
  effectActive: (() => number) | undefined;
  constructor(
    private binding: MmioBinding | undefined,
    private readonly execution: IoBridge,
  ) {}

  private admitted(width: number): MmioBinding {
    if (!this.execution.executing || this.effectActive?.() !== 1 || !this.binding)
      throw new Error('MMIO provider called outside a reached memory effect');
    if (width !== 8 && width !== 16 && width !== 32 && width !== 64)
      throw new Error('invalid MMIO transaction width');
    return this.binding;
  }

  readonly imports = {
    read: (address: bigint, width: number): bigint => {
      const binding = this.admitted(width);
      const value = Reflect.apply(binding.read, binding.receiver, [
        BigInt.asUintN(64, address),
        width,
      ]);
      if (typeof value !== 'bigint' || value < 0n || BigInt.asUintN(width, value) !== value)
        throw new TypeError('mmio.read must return an unsigned bigint fitting its width');
      return value;
    },
    write: (address: bigint, width: number, value: bigint): void => {
      const binding = this.admitted(width);
      if (
        Reflect.apply(binding.write, binding.receiver, [
          BigInt.asUintN(64, address),
          width,
          BigInt.asUintN(width, value),
        ]) !== undefined
      )
        throw new TypeError('mmio.write must return undefined');
    },
  };

  release(): void {
    this.binding = undefined;
    this.effectActive = undefined;
  }
}
