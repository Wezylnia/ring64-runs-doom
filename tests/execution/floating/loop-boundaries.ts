import { CODE, DATA, fault } from '../../support/fixture.js';
import {
  LONG,
  banks,
  changed,
  input,
  mxcsr,
  noFault,
  outcome,
  simdFault,
  using,
  xmm,
  type Scenario,
} from './fixture.js';

const code =
  'f30f100486' + // MOVSS xmm0,[rsi+rax*4]
  'f30f58448604' + // ADDSS xmm0,[rsi+rax*4+4]
  'f30f59448608' + // MULSS xmm0,[rsi+rax*4+8]
  'f30f5844860c' + // ADDSS xmm0,[rsi+rax*4+12]
  '4883c0044839c875e0' + // ADD rax,4; CMP rax,rcx; JNE head
  '90';
const data = '0000803f0000004000004040000080400000a0400000c0400000e04000000041';

export function loopBoundaryScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'guarded floating loop preserves exact budget and stop boundaries',
      async run() {
        await using(
          compiler,
          code,
          LONG,
          async (cpu) => {
            const before = input(CODE, { 0: 0n, 1: 8n, 6: DATA }, 2n);
            const raw = banks();
            for (const [budget, stop, reason, retired, rip, index, flags, low] of [
              [7n, undefined, 'budget', 7n, 0n, 4n, 0x97n, 0x41500000n],
              [13n, undefined, 'budget', 13n, 30n, 8n, 0x46n, 0x42aa0000n],
              [2n, CODE + 11n, 'budget', 2n, 11n, 0n, 2n, 0x40400000n],
              [7n, CODE + 11n, 'stop', 2n, 11n, 0n, 2n, 0x40400000n],
              [15n, CODE + 32n, 'stop', 14n, 32n, 8n, 0x46n, 0x42aa0000n],
            ] as const) {
              cpu.setState(before);
              cpu.setXmmState(raw);
              const result = await cpu.run({ budget, stop, repeatBudget: 0n });
              outcome(result, reason, budget, retired, 0n, 0n, {
                ...changed(before, CODE + rip, { 0: index }),
                flags,
              });
              const expected = raw.slice();
              expected[0] = low;
              xmm(cpu, expected);
              mxcsr(cpu, 0x1f80);
              noFault(result);
            }

            const entry = changed(before, CODE + 11n);
            const operands = raw.slice();
            operands[0] = (raw[0]! & ~0xffffffffn) | 0x3f800000n;
            cpu.setState(entry);
            cpu.setXmmState(operands);
            const result = await cpu.run({ budget: 1n, repeatBudget: 0n });
            outcome(result, 'budget', 1n, 1n, 0n, 0n, changed(entry, CODE + 17n));
            operands[0] = (raw[0]! & ~0xffffffffn) | 0x40400000n;
            xmm(cpu, operands);
          },
          { data: [{ base: DATA, bytes: data }] },
        );
      },
    },
    {
      name: 'guarded floating loop publishes completed laps and precise fault progress',
      async run() {
        for (const numeric of [true, false]) {
          const operands = numeric
            ? data.slice(0, 48) + '0100807f' + data.slice(56)
            : data.slice(0, 56);
          await using(
            compiler,
            code,
            LONG,
            async (cpu) => {
              const before = input(CODE, { 0: 0n, 1: 8n, 6: DATA }, 2n);
              const raw = banks();
              cpu.setState(before);
              cpu.setXmmState(raw);
              cpu.setMxcsr(numeric ? 0x1f00 : 0x1f80);
              const result = await cpu.run({ budget: 14n, repeatBudget: 0n });
              const at = CODE + (numeric ? 11n : 17n);
              outcome(
                result,
                numeric ? 'simd-exception' : 'data-fault',
                14n,
                numeric ? 9n : 10n,
                0n,
                0n,
                { ...changed(before, at, { 0: 4n }), flags: 0x97n },
              );
              if (numeric) {
                raw[0] = 0x41300000n;
                simdFault(result, 0x1f00, 0x1f01, 1, 1, 'pre-computation', at);
              } else {
                raw[0] = 0x429a0000n;
                fault(result, {
                  instructionPc: at,
                  address: DATA + 28n,
                  width: 4,
                  access: 'read',
                  cause: 'unmapped',
                  failingAddress: DATA + 28n,
                });
              }
              xmm(cpu, raw);
            },
            { data: [{ base: DATA, bytes: operands }] },
          );
        }
      },
    },
  ];
}
