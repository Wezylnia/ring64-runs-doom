import { createCpu, type Cpu, type RunResult } from '../../../../sdk/src/index.js';
import { fixture as pagedFixture, same } from '../supervisor-access/fixture.js';
export {
  AC,
  AUX,
  DATA,
  PC,
  hex,
  memory,
  preserved,
  put,
  same,
  state,
  using,
  word,
  type Family,
  type Scenario,
} from '../supervisor-access/fixture.js';
export { createCpu };
export type { Cpu, RunResult };
export const PKE = 0x400000n;
export const PKS = 0x1000000n;
export const WP = 0x10000n;

export function key(number: number): bigint {
  return BigInt(number) << 59n;
}

export function pair(number: number, rights: number): number {
  return Number(BigInt(rights) << BigInt(number * 2));
}

export function fixture(
  compiler: WebAssembly.Module,
  code: string,
  setup: { family?: 'ia32e' | 'fixed' | 'non-pae' | 'pae'; cpl?: number; bits?: 16 | 32 | 64 } = {},
) {
  const f = pagedFixture({ createCpu, compiler }, setup.family ?? 'ia32e', code, setup);
  f.paging.cr4 |= PKE | PKS;
  f.paging.cr0 |= WP;
  return f;
}

export function pageFault(
  result: RunResult,
  address: bigint,
  access: 'read' | 'write' | 'execute',
  errorCode: number,
  instructionPc = 0x1000n,
): void {
  if (result.reason !== 'page-fault') throw Error(`expected page-fault, received ${result.reason}`);
  same(
    result.pageFault,
    { vector: 14, address, access, errorCode, instructionPc },
    'literal page-fault packet',
  );
}
