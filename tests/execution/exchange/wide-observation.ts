import { createCpu } from '../../../host/index.js';
import { changed, check, equal, exit, hex, initial, type Scenario } from '../../support/fixture.js';
import { fixture, same, using, word } from '../supervisor-access/fixture.js';
import { little } from '../paging/fixture.js';
import { LONG, using as usingLinear } from './fixture.js';

export function wideObservationScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { compiler, createCpu };
  return [
    {
      name: 'CMPXCHG16B captures the address in RAX before mismatch writes back both live GPRs',
      async run() {
        for (const lock of ['', 'f0']) {
          const code = lock + '480fc708'; // CMPXCHG16B [RAX].
          await usingLinear(
            compiler,
            code,
            LONG,
            async (cpu) => {
              const before = initial(
                0x1000n,
                { 0: 0x6000n, 2: 0x8877665544332211n, 3: 0xfeedn, 1: 0xface000000000000n },
                0xed7n,
              );
              cpu.setState(before);
              const first = await cpu.run({ budget: 1n });
              exit(
                first,
                'budget',
                1n,
                1n,
                changed(
                  before,
                  before.rip + BigInt(code.length / 2),
                  { 0: 0x8000000000000001n, 2: 0x8877665544332211n },
                  before.flags & ~64n,
                ),
              );
              same(
                cpu.readMemory(0x6000n, 16),
                little([0x8000000000000001n, 0x8877665544332211n]),
                'writeback uses original RAX address',
              );
            },
            { data: [{ base: 0x6000n, bytes: '01000000000000801122334455667788' }] },
          );
        }
        for (const [code, reg, vector] of [
          ['480fc70e', 6, 13],
          ['480fc74d00', 5, 12],
        ] as const) {
          await usingLinear(compiler, code, LONG, async (cpu) => {
            const before = initial(0x1000n, { [reg]: 0x800000000000n }, 0xed7n);
            cpu.setState(before);
            const r = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
            check(
              r.state.event.kind === 'pending',
              'aligned noncanonical address selects a guest fault',
            );
            equal(r.state.event.exception.vector, vector, 'selected DS/SS address class');
            equal(r.state.event.exception.errorCode, 0, 'noncanonical address');
            equal(r.retired, 0n, 'pair did not retire');
          });
        }
      },
    },
    {
      name: 'CMPXCHG16B writes invalidate an executable alias confined to the high qword, including identical data on a miss',
      async run() {
        const original = 0x90feeb11111111b8n,
          replacement = 0x90feeb22222222b8n;
        for (const hit of [false, true]) {
          const f = fixture(side, 'ia32e', 'f0480fc70f');
          f.leaf(0x4000, 0xa063n); // Executable alias for the end of the data page.
          f.leaf(0x6000, 0xa063n);
          f.leaf(0x7000, 0n); // The last aligned pair in the preceding page must still succeed.
          f.bytes.set(little([9n, original]), 0xaff0);
          await using(side, f, async (cpu) => {
            const alias = 0x4ff8n;
            for (const compilations of [1, 0]) {
              cpu.setState(initial(alias));
              const warm = await cpu.run({ budget: 1n });
              equal(warm.state.registers[0], 0x11111111n, 'executable high half');
              equal(warm.resources.compilations, compilations, 'retained alias');
            }
            const before = initial(
              0x1000n,
              { 0: 9n, 1: replacement, 2: original ^ BigInt(!hit), 3: 9n, 7: 0x6ff0n },
              0xed7n,
            );
            cpu.setState(before);
            const cas = await cpu.run({ budget: 1n });
            exit(
              cas,
              'budget',
              1n,
              1n,
              changed(
                before,
                0x1005n,
                hit ? {} : { 0: 9n, 2: original },
                (before.flags & ~64n) | (hit ? 64n : 0n),
              ),
            );
            equal(word(cpu, 0xaff8), hit ? replacement : original, 'upper half written');
            cpu.setState(initial(alias));
            const replaced = await cpu.run({ budget: 1n });
            equal(
              replaced.state.registers[0],
              hit ? 0x22222222n : 0x11111111n,
              'current high half',
            );
            equal(
              replaced.resources.compilations,
              1,
              'even an identity write expired the old alias',
            );
          });
        }
      },
    },
    {
      name: 'CMPXCHG16B changes the live successor through a physical alias before it can execute',
      async run() {
        const f = fixture(side, 'ia32e', '90');
        f.leaf(0x6000, 0x8063n);
        // [0x6ff0] aliases the last pair of the code page. Its upper qword starts at next RIP.
        f.bytes.set(hex('909090f0480fc70fb811111111ebfe90'), 0x8ff0);
        await using(side, f, async (cpu) => {
          const lower = word(cpu, 0x8ff0),
            upper = word(cpu, 0x8ff8);
          cpu.setState(
            initial(0x1ff3n, { 0: lower, 2: upper, 3: lower, 1: 0x90feeb22222222b8n, 7: 0x6ff0n }),
          );
          const r = await cpu.run({ budget: 2n });
          equal(r.reason, 'budget', 'CAS and its actual successor');
          equal(r.retired, 2n, 'two distinct retired instructions');
          equal(r.state.rip, 0x1ffdn, 'just after the new MOV');
          equal(
            r.state.registers[0],
            0x22222222n,
            'successor was fetched after changing source bytes',
          );
        });
      },
    },
  ];
}
