import type { SharedLayout } from './control.js';
import type { Range } from './protocol.js';

function watchWords(memory: WebAssembly.Memory, layout: SharedLayout) {
  return new Int32Array(memory.buffer, layout.watchBase, layout.ramCapacity / 32);
}

export function markSourceWatches(
  memory: WebAssembly.Memory,
  layout: SharedLayout,
  ranges: readonly Range[],
): void {
  const words = watchWords(memory, layout);
  for (const { offset, length } of ranges) {
    const begin = offset - layout.ramBase;
    if (begin < 0 || length <= 0 || begin + length > layout.ramCapacity)
      throw new Error('invalid parallel source range');
    for (let i = begin; i < begin + length; i++)
      Atomics.or(words, Math.floor(i / 32), 1 << (i % 32));
  }
}

export class SourceWatches {
  readonly #owners: Array<Map<bigint, readonly Range[]>>;
  readonly #counts = new Map<number, number>();
  readonly #released: Array<readonly Range[]> = [];

  constructor(
    private readonly memory: WebAssembly.Memory,
    private readonly layout: SharedLayout,
  ) {
    this.#owners = Array.from({ length: layout.cpus }, () => new Map());
  }

  remember(cpu: number, owner: bigint, ranges: readonly Range[]): void {
    const owners = this.#owners[cpu]!;
    if (owners.has(owner)) throw new Error('duplicate parallel source watch');
    owners.set(owner, ranges);
    for (const { offset, length } of ranges)
      for (let i = offset; i < offset + length; i++)
        this.#counts.set(i, (this.#counts.get(i) ?? 0) + 1);
  }

  release(cpu: number, owner: bigint): boolean {
    const owners = this.#owners[cpu]!;
    const ranges = owners.get(owner);
    if (!ranges) return false;
    owners.delete(owner);
    this.#released.push(ranges);
    return true;
  }

  get hasReleases(): boolean {
    return this.#released.length > 0;
  }

  sweep(): void {
    const words = watchWords(this.memory, this.layout);
    for (const ranges of this.#released.splice(0)) {
      for (const { offset, length } of ranges) {
        for (let i = offset; i < offset + length; i++) {
          const count = this.#counts.get(i)! - 1;
          if (count === 0) {
            this.#counts.delete(i);
            const byte = i - this.layout.ramBase;
            Atomics.and(words, Math.floor(byte / 32), ~(1 << (byte % 32)));
          } else {
            this.#counts.set(i, count);
          }
        }
      }
    }
  }
}
