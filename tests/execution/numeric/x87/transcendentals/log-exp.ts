import { BANK, PC, initial, little, same, usingPaged } from '../../../paging/fixture.js';
import {
  INDEFINITE,
  LONG,
  ONE,
  environment,
  equal,
  input,
  other,
  pending,
  pop,
  seed,
  slot,
  stack,
  using,
  x87,
  type Scenario,
} from '../fixture.js';
const sign = 1n << 79n,
  half = 0x3ffe8000000000000000n;

export function logExpScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'binary80 log and exp distinguish suppressed operands from completed masked and pending results',
      async run() {
        for (const [code, x, y, cw, exceptions, answer] of [
          ['d9f0', sign, undefined, 0x37f, 0, sign],
          ['d9f0', undefined, undefined, 0x37f, 0x41, INDEFINITE],
          ['d9f0', 0x40000000000000000011n, undefined, 0x37e, 0x8081, undefined],
          ['d9f1', 0n, ONE, 0x37b, 0x8084, undefined],
          ['d9f1', sign, ONE, 0x37f, 4, 0xffff8000000000000000n],
          ['d9f1', 0n, 0n, 0x37f, 1, INDEFINITE],
          ['d9f1', ONE, sign | ONE, 0x37f, 0, sign],
          ['d9f9', undefined, ONE, 0x37e, 0x80c1, undefined],
          ['d9f9', 0n, undefined, 0x37f, 0x41, INDEFINITE],
          ['d9f9', 1n, ONE, 0x37d, 0x8082, undefined],
          ['d9f9', sign, sign | ONE, 0x37f, 0, 0n],
        ] as const) {
          // TOP=7 makes ST(1) wrap to physical R0. A third live value guards against removing both.
          const values: Record<number, bigint> = { 2: 0x4007cafe12345678abcdn };
          if (x !== undefined) values[0] = x;
          if (y !== undefined) values[1] = y;
          const before = stack(values, 7, cw);
          const pc = 0x1001n,
            fop = 0x100 | Number.parseInt(code.slice(2), 16);
          await using(compiler, '90' + code + '9b', LONG, async (cpu) => {
            cpu.setState(input());
            seed(cpu, before);
            const warm = await cpu.run({ budget: 1n });
            equal(warm.state.rip, pc, 'resident middle entry');
            const transfer = await cpu.run({ budget: 1n });
            equal(transfer.reason, 'budget', 'producer completed');
            equal(transfer.retired, 1n, 'one producer');
            equal(transfer.state.flags, 0x8d7n, 'integer flags preserved');
            const expected = environment(
              before,
              pc,
              undefined,
              exceptions & 0x80 ? fop : undefined,
            );
            expected.statusWord |= exceptions;
            if (answer !== undefined) {
              if (code === 'd9f0') slot(expected, 7, answer);
              else {
                slot(expected, 0, answer);
                pop(expected);
              }
            }
            x87(cpu, expected, 0xb8ff);
            other(cpu);
            if (exceptions & 0x80) {
              const wait = await cpu.run({ budget: 1n });
              equal(wait.reason, 'x87-exception', 'following WAIT reports pending exception');
              equal(wait.retired, 0n, 'WAIT restarts');
              pending(wait, cpu.readX87State(), pc + 2n);
            }
          });
        }
      },
    },
    {
      name: 'unmasked transcendental precision publishes a full-width result and stack pop before deferred #MF',
      async run() {
        for (const [code, x, y, near] of [
          ['d9f0', half, ONE, 0x3ffdd413cccfe7799211n], // sqrt(2)-1
          ['d9f1', 0x3fffc000000000000000n, ONE, 0x3ffe95c01a39fbd687a0n], // log2(1.5)
          ['d9f9', 0x3ffc8000000000000000n, ONE, 0x3ffcae00d1cfdeb43cfdn], // log2(1.125)
        ] as const) {
          await using(compiler, code + '9b', LONG, async (cpu) => {
            const cw = 0x05f; // PC=24, nearest, precision unmasked
            const before = stack({ 0: x, 1: y, 2: sign | ONE }, 7, cw);
            cpu.setState(input());
            seed(cpu, before);
            const r = await cpu.run({ budget: 2n });
            equal(r.reason, 'x87-exception', 'next WAIT detects the precision exception');
            equal(r.retired, 1n, 'producer retired once');
            equal(r.state.rip, 0x1002n, 'WAIT is restartable');
            const state = cpu.readX87State(),
              top = code === 'd9f0' ? 7 : 0;
            equal((state.statusWord >>> 11) & 7, top, 'precision commits the specified pop');
            equal(state.statusWord & 0x80ff, 0x80a0, 'only precision pending');
            equal(
              state.tagWord,
              code === 'd9f0' ? before.tagWord : before.tagWord | (3 << 14),
              'physical tags',
            );
            const result = state.registers[top]!;
            equal(
              result >= near - 1n && result <= near + 1n,
              true,
              'full-precision positive raw80 ' + result.toString(16),
            );
            equal(state.registers[1], sign | ONE, 'unrelated logical register');
            equal(state.instructionPointer, 0x1000n, 'FIP is the producer');
            equal(state.dataPointer, before.dataPointer, 'register-only instruction retains FDP');
            equal(state.opcode, 0x100 | Number.parseInt(code.slice(2), 16), 'producer FOP');
            pending(r, state, 0x1002n);
            other(cpu);
          });
        }
      },
    },
    {
      name: 'a missing x87 opcode tail precedes device and pending exceptions and the repaired instruction retries once',
      async run() {
        const entry = PC + 0xfffn;
        for (const tail of [0xf0, 0xf1, 0xf9]) {
          const successor = new Uint8Array(4096);
          successor[0] = tail;
          await usingPaged(
            compiler,
            { spans: [{ physical: 0xc000n, bytes: successor, execute: true }] },
            async (cpu) => {
              cpu.writePhysicalMemory(0x9fffn, Uint8Array.of(0xd9));
              const old = stack({ 0: 0n, 1: ONE }, 7, 0x37e, 0x8081);
              cpu.setState(initial(entry));
              seed(cpu, old);
              cpu.setPagingState({ ...BANK, cr0: BANK.cr0 | 8n });
              const fetch = await cpu.run({ budget: 1n });
              equal(fetch.reason, 'page-fault', 'instruction fetch wins over TS');
              same(
                [fetch.pageFault?.address, fetch.pageFault?.access],
                [entry + 1n, 'execute'],
                'missing second opcode byte',
              );
              equal(fetch.retired, 0n, 'fetch consumes no retirement');
              x87(cpu, old);
              cpu.writePhysicalMemory(0x4008n, little([0xc007n]));
              for (const bit of [8n, 4n, 0n]) {
                cpu.setPagingState({ ...BANK, cr0: BANK.cr0 | bit });
                const fault = await cpu.run({ budget: 1n });
                equal(
                  fault.reason,
                  bit ? 'device-not-available' : 'x87-exception',
                  'numeric admission',
                );
                equal(fault.state.rip, entry, 'same restart PC');
                equal(fault.retired, 0n, 'no instruction retired');
                x87(cpu, old);
              }
              cpu.setX87State(stack({ 0: 0n, 1: ONE }, 7));
              const done = await cpu.run({ budget: 1n });
              equal(done.reason, 'budget', 'repaired register instruction');
              equal(done.retired, 1n, 'single retirement');
              equal(done.state.rip, entry + 2n, 'full page-crossing encoding');
            },
          );
        }
      },
    },
  ];
}
