import { longSnapshot } from '../../../support/context.js';
import {
  BANK,
  PC,
  check,
  equal,
  initial,
  little,
  usingPaged,
  type Scenario,
} from '../../memory/paging/fixture.js';

export function siblingEntryScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'a changed WRGSBASE fences a scalar load discovered through another edge in the active unit',
    async run() {
      const a = 0x402000n,
        b = 0x403000n;
      await usingPaged(
        compiler,
        {
          // JZ load; WRGSBASE R8; load: MOV RAX,GS:[RBX]
          code: '7405f3490faed865488b03',
          bank: { ...BANK, cr4: BANK.cr4 | 0x10000n },
          context: longSnapshot(0n, a),
          words: [
            [0x4010n, 0xb007n],
            [0x4018n, 0xc007n],
          ],
          spans: [
            { physical: 0xb000n, bytes: little([0xaaaa1111n]) },
            { physical: 0xc000n, bytes: little([0xbbbb2222n]) },
          ],
        },
        async (cpu) => {
          const write = PC + 2n,
            load = PC + 7n;
          cpu.setState(initial(PC, { 3: 0n }, 0x42n));
          const first = await cpu.run({ budget: 2n });
          equal(first.state.registers[0], 0xaaaa1111n, 'the taken edge reads base A');
          check(
            cpu
              .inspectTranslations()
              .some((u) => u.entries.includes(write) && u.entries.includes(load)),
            'both edges share a generated unit',
          );
          for (const [base, value, compilations] of [
            [a, 0xaaaa1111n, 0],
            [b, 0xbbbb2222n, 1],
          ] as const) {
            cpu.setState(initial(write, { 3: 0n, 8: base }));
            const result = await cpu.run({ budget: 2n });
            equal(result.reason, 'budget', 'WR and load completed');
            equal(result.retired, 2n, 'exactly one write and one scalar load');
            equal(result.state.rip, PC + 11n, 'retired to the end of the load');
            equal(result.state.registers[0], value, 'scalar load uses the current base');
            equal(
              result.resources.compilations,
              compilations,
              'only a changed context needs a different load owner',
            );
          }
        },
      );
    },
  };
}
