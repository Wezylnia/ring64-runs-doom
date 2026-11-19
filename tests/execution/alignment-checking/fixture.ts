import { createCpu, type Cpu, type RunResult, type State } from '../../../sdk/src/index.js';
import { equal, hex, initial, type Scenario } from '../../support/fixture.js';
import {
  fixture as pagedFixture,
  same,
  put,
  memory,
  preserved,
  using,
  type Family,
} from '../supervisor-access/fixture.js';

export { createCpu, equal, hex, initial, same, put, memory, preserved, using };
export type { Cpu, RunResult, State, Scenario, Family };
export const PC = 0x1000n;
export const AC = 0x40000n;
export const AUX = 0x89abcdef;
export const families: Family[] = ['non-pae', 'pae', 'ia32e', 'fixed'];
export const payload = '102030405060708090a0b0c0d0e0f000';

export function bytes(actual: Uint8Array, expected: Uint8Array, label: string): void {
  equal(actual.length, expected.length, `${label} length`);
  for (let i = 0; i < expected.length; i++)
    if (actual[i] !== expected[i])
      throw Error(`${label} byte ${i.toString(16)}: ${actual[i]} != ${expected[i]}`);
}

export function fixture(compiler: WebAssembly.Module, code: string, family: Family = 'non-pae') {
  const f = pagedFixture({ createCpu, compiler }, family, code, {
    cpl: 3,
    bits: family === 'non-pae' || family === 'pae' ? 32 : 64,
  });
  f.paging.cr0 |= AC;
  for (const [linear, physical] of [
    [0x4000, 0x9000],
    [0x5000, 0x15000],
    [0x6000, 0xa000],
    [0x7000, 0xb000],
    [0x9000, 0xd000],
  ] as const)
    f.leaf(linear, BigInt(physical) | 0x67n);
  f.bytes.set(hex(payload), 0xa000);
  return f;
}

export function state(registers: Record<number, bigint> = {}, flags = AC | 2n): State {
  return initial(
    PC,
    {
      0: 0x11223344cafebaben,
      3: 0x6001n,
      4: 0x9000n,
      5: 0x6001n,
      6: 0x6001n,
      7: 0x7001n,
      ...registers,
    },
    flags,
  );
}

export function scalar(
  result: RunResult,
  before: State,
  retired = 0n,
  bytes = 0,
  changes: Record<number, bigint> = {},
  flags = before.flags,
): void {
  const registers = [...before.registers];
  for (const [index, value] of Object.entries(changes)) registers[Number(index)] = value;
  same(
    result.state,
    { ...before, registers, flags, rip: before.rip + BigInt(bytes) },
    'complete alignment scalar state',
  );
  equal(result.retired, retired, 'alignment retirement');
  equal(result.remaining, 1n - retired, 'alignment instruction budget');
  equal(result.repeatIterations, 0n, 'no repeat progress');
  equal(result.remainingRepeatBudget, 7n, 'repeat allowance untouched');
}

export function checkFault(
  result: RunResult,
  address: bigint,
  width: number,
  access: 'read' | 'write',
  alignment: number,
  segment = 'ds',
  offset = address,
  errorCode = 0,
): void {
  equal(result.reason, 'alignment-check', 'distinct architectural alignment reason');
  same(
    result.alignmentCheck,
    {
      instructionPc: PC,
      vector: 17,
      errorCode,
      segment,
      offset,
      address,
      width,
      access,
      alignment,
    },
    'literal architectural alignment packet',
  );
  noOtherFault(result);
}

export function noOtherFault(result: RunResult): void {
  for (const field of [
    'fault',
    'segmentFault',
    'alignmentFault',
    'simdException',
    'generalProtectionFault',
    'x87Exception',
    'deviceNotAvailable',
    'numericControlFault',
    'protectionFault',
    'systemMemoryFault',
    'debugException',
    'pageFault',
    'physicalMemoryFault',
  ] as const)
    equal(result[field], undefined, `no unrelated ${field}`);
}

export function clock(cpu: Cpu, tsc = 100n): void {
  same(cpu.readTimeState(), { tsc, tscAux: AUX }, 'alignment virtual time');
}

export async function runOne(cpu: Cpu, before: State): Promise<RunResult> {
  cpu.setState(before);
  cpu.setTimeState({ tsc: 100n, tscAux: AUX });
  return cpu.run({ budget: 1n, repeatBudget: 7n });
}
