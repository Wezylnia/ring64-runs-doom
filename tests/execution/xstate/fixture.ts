import { createCpu, type Cpu } from '../../../sdk/src/index.js';
import { PC, fixture, hex, same, state, using } from '../protection-keys/fixture.js';
import { context } from '../supervisor-access/fixture.js';
export {
  DATA,
  PC,
  hex,
  key,
  pageFault,
  pair,
  put,
  same,
  state,
  word,
  type Scenario,
} from '../protection-keys/fixture.js';
export { createCpu, type Cpu };
export const OSXSAVE = 0x40000n;
export const FULL = 0x203n;
export const PHYS = 0xa000;
export const CONTROL = PC + 0x800n;
export const XSET = CONTROL + 3n;
export const XGET = CONTROL + 6n;
export const CPUID = CONTROL + 9n;

export async function withImage(
  compiler: WebAssembly.Module,
  code: string,
  action: (cpu: Cpu) => Promise<void>,
  configure?: (setup: ReturnType<typeof fixture>) => void,
) {
  const f = fixture(compiler, code);
  f.bytes.set(hex('0f22e00f01d10f01d00fa2'), 0x8800);
  configure?.(f);
  await using({ createCpu, compiler }, f, action);
}

export function position(cpu: Cpu, rip: bigint, regs: Record<number, bigint> = {}) {
  const before = state(0x202n, rip);
  for (const [i, value] of Object.entries(regs)) before.registers[Number(i)] = value;
  cpu.setState(before);
}

export async function run(cpu: Cpu, budget: number) {
  const result = await cpu.run({ budget: BigInt(budget) });
  same(
    [result.reason, result.retired, result.remaining],
    ['budget', BigInt(budget), 0n],
    'guest completion at ' + result.state.rip.toString(16),
  );
  return result;
}

export async function enable(cpu: Cpu, mask = FULL) {
  position(cpu, CONTROL, { 0: cpu.readPagingState().cr4 | OSXSAVE });
  await run(cpu, 1);
  position(cpu, XSET, { 0: mask, 1: 0n, 2: 0n });
  await run(cpu, 1);
}

export function enterUser(cpu: Cpu) {
  cpu.setContext(context('ia32e', 3));
}
