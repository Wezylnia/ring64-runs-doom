import { readFile } from 'node:fs/promises';
import type { MemoryMapping } from '../../../../src/index.js';

export const compiler = await WebAssembly.compile(
  new Uint8Array(await readFile(new URL('../../../../../build/ring64.wasm', import.meta.url))),
);

export const head = 0x3b1000n;
export const row = 0x55f0209n;
export const column = 0x72d0ffdn;
export const readOnly = { read: true, write: false, execute: false };

export function mapping(base: bigint, backing: string, length: number): MemoryMapping {
  return { backing, base, length, offset: 0, permissions: readOnly };
}
