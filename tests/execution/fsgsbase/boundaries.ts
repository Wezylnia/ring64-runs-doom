import { createCpu } from '../../../sdk/src/index.js';
import { changed, check, equal, exit, initial, type Scenario } from '../../support/fixture.js';
import { preserveNumeric, same } from '../far-control/fixture.js';
import {
  controls,
  ENTRY,
  input,
  using as withProfile,
  type Profile,
} from '../identity-time/oracle.js';
import { fixture, using } from '../supervisor-access/fixture.js';
import { LONG, using as linear } from '../modes/fixture.js';

export function boundaryScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'user FSGSBASE qword and dword forms preserve null segment caches, flags and the inactive GS partner',
      async run() {
        for (const segment of ['fs', 'gs'] as const)
          for (const wide of [false, true])
            for (const reg of [4, 12]) {
              const group = segment === 'fs' ? 0 : 1,
                rex = 0x44 | (wide ? 8 : 0) | (reg === 12 ? 1 : 0); // REX.R ignored for the group, B extends the GPR.
              const form = (op: number) =>
                'f3' + rex.toString(16) + '0fae' + (0xc4 + 8 * op).toString(16);
              const f = fixture(side, 'ia32e', form(group + 2) + form(group), { cpl: 3 });
              f.paging.cr4 = (f.paging.cr4 | 0x10000n) & ~0x40600n;
              f.paging.cr0 |= 0xcn;
              await using(side, f, async (cpu) => {
                const context = cpu.readContext();
                context.segments[segment].selector = 0;
                context.segments[segment].usable = false;
                context.segments[segment].base = 0x4100n;
                cpu.setContext(context);
                const numeric = preserveNumeric(cpu);
                cpu.setFastSystemState({
                  ...cpu.readFastSystemState(),
                  kernelGsBase: 0xffff800000005000n,
                });
                const partner = cpu.readFastSystemState();
                for (const value of [
                  0n,
                  0xffffffffn,
                  0x00007fffffffffffn,
                  0xffff800000000000n,
                  0xffffffffffffffffn,
                  ...(wide ? [] : [0x123456789abcdef0n]),
                ]) {
                  cpu.setContext(context);
                  const base = wide ? value : BigInt.asUintN(32, value);
                  const before = initial(0x1000n, { [reg]: value }, 0xed7n);
                  cpu.setDebugState({ addresses: [base, 0n, 0n, 0n], dr6: 0n, dr7: 0x30401n });
                  cpu.setState(before);
                  const write = await cpu.run({ budget: 1n });
                  exit(write, 'budget', 1n, 1n, changed(before, 0x1005n));
                  const expected = structuredClone(context);
                  expected.segments[segment].base = base;
                  same(cpu.readContext(), expected, 'only the hidden base changed');
                  const read = initial(0x1005n, { [reg]: 0xdeadc0de12345678n }, 0xed7n);
                  cpu.setState(read);
                  exit(
                    await cpu.run({ budget: 1n }),
                    'budget',
                    1n,
                    1n,
                    changed(read, 0x100an, { [reg]: base }),
                  );
                  equal(
                    cpu.readDebugState().dr6 & 1n,
                    0n,
                    'register-only base access did not touch its value as memory',
                  );
                  same(cpu.readFastSystemState(), partner, 'inactive SWAPGS partner preserved');
                  numeric();
                }
              });
            }
      },
    },
    {
      name: 'FSGSBASE admission precedes RF consumption and a noncanonical value fault follows admission on a retained middle entry',
      async run() {
        for (const family of ['ia32e', 'fixed'] as const) {
          const f = fixture(side, family, '90f3480faed090', { cpl: 3 });
          f.paging.cr4 |= 0x10000n;
          await using(side, f, async (cpu) => {
            const original = cpu.readContext(),
              partner = cpu.readFastSystemState();
            const entry = 0x1001n;
            cpu.setDebugState({ addresses: [0n, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
            cpu.setState(initial(0x1000n));
            const w = await cpu.run({ budget: 1n });
            equal(w.reason, 'budget', 'warm source before WRFSBASE');
            check(
              cpu
                .inspectTranslations()
                .some((u) => u.entries.includes(0x1000n) && u.entries.includes(entry)),
              'compiled middle entry',
            );
            const bank = cpu.readPagingState();
            cpu.setPagingState({ ...bank, cr4: bank.cr4 & ~0x10000n });
            cpu.setDebugState({ addresses: [entry, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
            const before = initial(entry, { 0: 0xffff7ffffffffffen }, 0xed7n);
            cpu.setState(before);
            const execute = await cpu.run({ budget: 1n });
            equal(execute.reason, 'debug-exception', 'execute watch beats gate');
            for (const [enabled, vector, flags] of [
              [false, 6, 0x10ed7n],
              [true, 13, 0xed7n],
            ] as const) {
              if (enabled) cpu.setPagingState(bank);
              const resumed = { ...before, flags: 0x10ed7n };
              cpu.setState(resumed);
              const report = await cpu.run({ budget: 1n });
              equal(report.retired, 0n, 'no retirement before raw fault report');
              equal(report.state.flags, flags, 'RF before guest exception staging');
              cpu.setState(resumed);
              const r = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
              check(r.state.event.kind === 'pending', 'architectural cause staged');
              equal(r.state.event.exception.vector, vector, 'selected vector');
              equal(r.state.event.exception.errorCode, enabled ? 0 : undefined, 'selected error');
              equal(r.state.rip, entry, 'faulting PC');
              equal(r.retired, 0n, 'failed write');
              same(r.state.registers, before.registers, 'refused operand unchanged');
              same(cpu.readContext(), original, 'no base or attributes published');
              same(cpu.readFastSystemState(), partner, 'parked base unaffected');
            }
            cpu.setState(initial(entry, { 0: 0x6000n }, 0x10ed7n));
            const fixed = await cpu.run({ budget: 1n });
            equal(fixed.reason, 'budget', 'repaired input bypasses execute watch with RF');
            equal(fixed.state.flags, 0xed7n, 'successful begin consumed RF');
            equal(cpu.readContext().segments.fs.base, 0x6000n, 'correct recovery');
          });
        }
      },
    },
    {
      name: 'nonlong FSGSBASE is undefined with the control bit held set and linear long64 keeps its absent-control frontier',
      async run() {
        const profiles: Profile[] = [
          ...controls.filter((p) => p.mode !== 'long64'),
          { mode: 'virtual8086', bits: 16, stack: 16, cpl: 3, family: 'pg0' },
          { mode: 'long64', bits: 64, family: 'linear' },
        ];
        for (const p of profiles)
          for (const op of ['c0', 'c8', 'd0', 'd8'])
            await withProfile(side, 'f30fae' + op, p, async (cpu) => {
              if (p.family !== 'linear') {
                const bank = cpu.readPagingState();
                cpu.setPagingState({ ...bank, cr4: bank.cr4 | 0x10000n });
              }
              const before = input(p);
              cpu.setState(before);
              const r = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
              check(r.state.event.kind === 'pending', 'nonlong or absent gate #UD');
              equal(r.state.event.exception.vector, 6, 'mode/control vector');
              equal(r.state.rip, ENTRY, 'selected form PC');
              equal(r.retired, 0n, 'no retired work');
              same(r.state.registers, before.registers, 'all GPRs preserved');
            });
      },
    },
    {
      name: 'FSGSBASE fetches complete selected encodings before #UD and leaves neighboring register opcodes private',
      async run() {
        for (const [tail, kind] of [
          ['f30fae', 'fetch-fault'],
          ['f30fae84', 'fetch-fault'],
          ['f30fae8400010203', 'fetch-fault'],
          ['f0f30fae8400010203', 'fetch-fault'],
          ['f30fae00', 'invalid-encoding'],
          ['f0f3480faec0', 'invalid-encoding'],
          ['f30faee0', 'unsupported'],
        ] as const) {
          await linear(compiler, tail, LONG, async (cpu) => {
            const before = initial(0x1000n, {}, 0xed7n);
            cpu.setState(before);
            const r = await cpu.run({ budget: 1n });
            exit(r, kind, 1n, 0n, before);
            if (kind === 'fetch-fault')
              equal(
                r.fault?.failingAddress,
                before.rip + BigInt(tail.length / 2),
                'next required encoding byte',
              );
          });
        }
      },
    },
  ];
}
