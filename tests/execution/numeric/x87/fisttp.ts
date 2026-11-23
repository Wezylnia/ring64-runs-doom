import {
  BANK,
  PC,
  initial,
  little,
  same,
  usingPaged,
  type Scenario,
} from '../../paging/fixture.js';
import { NUMERIC_STATUS, environment, equal, pending, pop, raw, stack, x87 } from './fixture.js';

export function fisttpScenarios(compiler: WebAssembly.Module): Scenario[] {
  const pc = PC + 1n;
  const destination = PC + 0x100n;
  const physical = 0x9100n;
  return [
    {
      name: 'FISTTP stores and pops inexact results before deferred precision and preserves failed invalid operands',
      async run() {
        for (const [code, width, fop] of [
          ['df0b', 2, 0x70b],
          ['db0b', 4, 0x30b],
          ['dd0b', 8, 0x50b],
        ] as const) {
          await usingPaged(compiler, { code: '90' + code + '9b' }, async (cpu) => {
            cpu.setState(initial(PC));
            await cpu.run({ budget: 1n });
            for (const [value, cw, sw, stored] of [
              [raw(0xbfff, 0xe000000000000000n), 0x77f, 0x20, -1n],
              [raw(0x3fff, 0xe000000000000000n), 0xb5f, 0x80a0, 1n],
              [raw(0x7fff), 0x37e, 0x8081, null],
              [undefined, 0x37f, 0x41, 1n << BigInt(width * 8 - 1)],
            ] as const) {
              const before = stack(value === undefined ? {} : { 0: value }, 5, cw, 0x200);
              const expected = environment(before, pc, destination, sw & 0x80 ? fop : undefined);
              expected.statusWord = (5 << 11) | sw;
              if (stored !== null) pop(expected);
              const poison = new Uint8Array(10).fill(0xa5);
              cpu.writePhysicalMemory(physical - 1n, poison);
              cpu.setX87State(before);
              cpu.setState(initial(pc, { 3: destination }, 0xcd7n));
              const result = await cpu.run({ budget: 1n });
              same(
                [result.reason, result.retired, result.state.rip, result.state.flags],
                ['budget', 1n, pc + 2n, 0xcd7n],
                'FISTTP retirement',
              );
              x87(cpu, expected, NUMERIC_STATUS);
              if (stored !== null) poison.set(little([stored], width), 1);
              same(cpu.readPhysicalMemory(physical - 1n, 10), poison, 'exact store or suppression');
              if (sw & 0x80) {
                const wait = await cpu.run({ budget: 1n });
                equal(wait.reason, 'x87-exception', 'next WAIT reports deferred exception');
                pending(wait, expected, pc + 2n);
              }
            }
          });
        }
      },
    },
    {
      name: 'FISTTP uses x87 enable and pending admission before its entire destination write',
      async run() {
        const last = PC + 0xffcn;
        await usingPaged(compiler, { code: 'dd0b' }, async (cpu) => {
          const clean = stack({ 0: raw(0x3fff) }, 4);
          const old = stack({ 0: raw(0x3fff) }, 4, 0x37e, 0x8081);
          const bank = { ...BANK, cr4: BANK.cr4 & ~0x600n };
          cpu.setDebugState({ addresses: [last, 0n, 0n, 0n], dr6: 0n, dr7: 0x10401n });
          for (const [cr0, prior, reason] of [
            [bank.cr0 | 4n, old, 'device-not-available'],
            [bank.cr0 | 8n, old, 'device-not-available'],
            [bank.cr0, old, 'x87-exception'],
            [bank.cr0, clean, 'page-fault'],
          ] as const) {
            cpu.setPagingState({ ...bank, cr0 });
            cpu.setX87State(prior);
            cpu.setState(initial(PC, { 3: last }));
            const prefix = cpu.readPhysicalMemory(0x9ffcn, 4);
            const result = await cpu.run({ budget: 1n });
            same(
              [result.reason, result.retired, result.state.rip],
              [reason, 0n, PC],
              'fault priority',
            );
            if (reason === 'page-fault')
              same(
                [result.pageFault?.address, result.pageFault?.access],
                [PC + 0x1000n, 'write'],
                'first denied byte is a write',
              );
            x87(cpu, prior);
            same(cpu.readPhysicalMemory(0x9ffcn, 4), prefix, 'no partial destination');
            equal(cpu.readDebugState().dr6 & 1n, 0n, 'failed store contributes no watchpoint');
          }
        });
      },
    },
  ];
}
