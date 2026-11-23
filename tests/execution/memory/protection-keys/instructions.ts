import {
  input,
  using as usingProfile,
  type Profile,
} from '../../processor/identity-time/oracle.js';
import { createFixture, initial } from '../../../support/fixture.js';
import {
  AUX,
  PC,
  PKE,
  PKS,
  createCpu,
  fixture,
  same,
  state,
  using,
  type Scenario,
} from './fixture.js';

export function instructionScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'protection keys every REX bit pattern preserves fixed32 PKRU operands and poisoned high register halves',
      async run() {
        for (let rex = 0x40; rex <= 0x4f; rex++)
          for (const opcode of ['0f01ee', '0f01ef']) {
            const f = fixture(compiler, rex.toString(16) + opcode);
            await using(side, f, async (cpu) => {
              cpu.setProtectionKeysState({ pkru: 0x89abcdef, pkrs: 0x76543210 });
              const before = state();
              before.registers[0] = 0x1122334480000001n;
              before.registers[1] = 0x5566778800000000n;
              before.registers[2] = 0x99aabbcc00000000n;
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              same(result.reason, 'budget', `REX${rex.toString(16)} ${opcode} admission`);
              const expected = structuredClone(before);
              expected.rip += 4n;
              if (opcode === '0f01ee') {
                expected.registers[0] = 0x89abcdefn;
                expected.registers[2] = 0n;
              }
              same(
                result.state,
                expected,
                'REX does not extend fixed input or destination registers',
              );
              same(
                cpu.readProtectionKeysState(),
                { pkru: opcode === '0f01ee' ? 0x89abcdef : 0x80000001, pkrs: 0x76543210 },
                'REX fixed32 bank publication',
              );
            });
          }
      },
    },
    {
      name: 'protection keys Some paging PKE0 reaches UD before invalid register inputs in report and delivery',
      async run() {
        for (const opcode of ['0f01ee', '0f01ef']) {
          const f = fixture(compiler, opcode);
          f.paging.cr4 &= ~PKE;
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: 0x11223344, pkrs: 0x55667788 });
            const before = state(0x10202n);
            before.registers[1] = 1n;
            before.registers[2] = 1n;
            cpu.setState(before);
            const report = await cpu.run({ budget: 1n });
            same(report.reason, 'invalid-encoding', 'Some PKE0 reports supported instruction UD');
            same(report.retired, 0n, 'Some PKE0 no retirement');
            same(
              report.state,
              { ...before, flags: 0x202n },
              'reached feature gate consumes RF only',
            );
            cpu.setState(before);
            const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
            same(staged.reason, 'event-budget', 'Some PKE0 stages UD');
            same(
              staged.state.event,
              {
                kind: 'pending',
                exception: { vector: 6, instructionPc: PC, returnPc: PC, savedFlags: 0x10202n },
              },
              'Some PKE0 no error-code slot',
            );
            same(
              cpu.readProtectionKeysState(),
              { pkru: 0x11223344, pkrs: 0x55667788 },
              'feature failure preserves canonical bank',
            );
          });
        }
      },
    },
    {
      name: 'protection keys RDPKRU WRPKRU fixed32 publications across real v86 protected compatibility and long modes',
      async run() {
        const profiles: Profile[] = [
          { mode: 'real', bits: 16, family: 'pg0' },
          { mode: 'real', bits: 32, family: 'pg0' },
          { mode: 'virtual8086', bits: 16, cpl: 3, family: 'pg0' },
          ...([16, 32] as const).map((bits) => ({
            mode: 'protected' as const,
            bits,
            family: 'pg0' as const,
          })),
          ...([16, 32] as const).map((bits) => ({
            mode: 'compatibility' as const,
            bits,
            family: 'ia32e' as const,
          })),
          { mode: 'long64', bits: 64, family: 'ia32e' },
        ];
        for (const profile of profiles)
          for (const code of ['0f01ee', '0f01ef'])
            await usingProfile(side, code, profile, async (cpu) => {
              cpu.setPagingState({
                ...cpu.readPagingState(),
                cr4: cpu.readPagingState().cr4 | PKE,
              });
              cpu.setProtectionKeysState({ pkru: 0x89abcdef, pkrs: 0x76543210 });
              const before = input(profile);
              before.registers[0] = 0xaabbccdd80000001n;
              before.registers[1] = 0x1122334400000000n;
              before.registers[2] = 0x5566778800000000n;
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              same(result.reason, 'budget', 'mode-neutral PKRU operation');
              same(result.retired, 1n, 'one PKRU retirement');
              const expected = structuredClone(before);
              expected.rip += 3n;
              if (code === '0f01ee') {
                expected.registers[0] = 0x89abcdefn;
                expected.registers[2] = 0n;
              }
              same(result.state, expected, 'fixed32 destinations and ignored high input halves');
              same(
                cpu.readProtectionKeysState(),
                { pkru: code === '0f01ee' ? 0x89abcdef : 0x80000001, pkrs: 0x76543210 },
                'only selected bank publication',
              );
            });
      },
    },
    {
      name: 'protection keys all CPLs admit PKRU and bad low ECX or EDX gives GP0 atomically',
      async run() {
        for (const cpl of [0, 1, 2, 3])
          for (const [code, ecx, edx, denied] of [
            ['0f01ee', 0n, 5n, false],
            ['0f01ee', 1n, 0n, true],
            ['0f01ef', 0n, 0n, false],
            ['0f01ef', 1n, 0n, true],
            ['0f01ef', 0n, 1n, true],
          ] as const) {
            const f = fixture(compiler, code, { cpl });
            await using(side, f, async (cpu) => {
              cpu.setProtectionKeysState({ pkru: 0x13579bdf, pkrs: 0x2468ace0 });
              const before = state(0x202n);
              before.registers[1] = ecx;
              before.registers[2] = edx;
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              same(
                result.reason,
                denied ? 'protection-fault' : 'budget',
                'PKRU low input legality',
              );
              same(result.retired, denied ? 0n : 1n, 'PKRU admission retirement');
              if (denied) {
                same(result.state, before, 'bad input scalar atomicity');
                same(
                  cpu.readProtectionKeysState(),
                  { pkru: 0x13579bdf, pkrs: 0x2468ace0 },
                  'bad input bank atomicity',
                );
                const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
                same(
                  staged.state.event,
                  {
                    kind: 'pending',
                    exception: {
                      vector: 13,
                      instructionPc: PC,
                      returnPc: PC,
                      savedFlags: 0x10202n,
                      errorCode: 0,
                    },
                  },
                  'GP0 event source',
                );
              }
            });
          }
      },
    },
    {
      name: 'protection keys NP prefixes length boundaries and required-byte fetch retain decoded fault ownership',
      async run() {
        for (const opcode of ['0f01ee', '0f01ef'])
          for (const prefix of ['66', 'f2', 'f3', 'f0']) {
            const f = fixture(compiler, prefix + opcode);
            await using(side, f, async (cpu) => {
              const before = state(0x10202n);
              cpu.setState(before);
              same((await cpu.run({ budget: 1n })).reason, 'invalid-encoding', 'NP prefix UD');
              same(cpu.readState(), before, 'decoded report does not begin');
              const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
              same(
                staged.state.event,
                {
                  kind: 'pending',
                  exception: { vector: 6, instructionPc: PC, returnPc: PC, savedFlags: 0x10202n },
                },
                'prefix UD without error slot',
              );
            });
          }
        for (const prefix of ['67', '2e', '48', '67'.repeat(12)]) {
          const f = fixture(compiler, prefix + '0f01ee');
          await using(side, f, async (cpu) => {
            const before = state();
            before.registers[1] = 0n;
            cpu.setState(before);
            cpu.setProtectionKeysState({ pkru: 0xffffffff, pkrs: 0 });
            const result = await cpu.run({ budget: 1n });
            same(result.reason, 'budget', 'ignored prefix or exact fifteen-byte instruction');
            same(result.state.registers[0], 0xffffffffn, 'prefix fixed32 output');
            same(
              result.state.rip,
              PC + BigInt(prefix.length / 2 + 3),
              'prefix exact instruction length',
            );
          });
        }
        const usingPrivate = createFixture(compiler);
        for (const code of ['0f01', '660f01'])
          await usingPrivate(code, async (cpu) => {
            cpu.setState(initial());
            const result = await cpu.run({ budget: 1n });
            same(result.reason, 'fetch-fault', 'required ModRM fetch precedes feature and NP gate');
          });
        await usingPrivate('67'.repeat(13) + '0f01', async (cpu) => {
          cpu.setState(initial());
          const result = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
          same(
            result.state.event,
            {
              kind: 'pending',
              exception: {
                vector: 13,
                instructionPc: PC,
                returnPc: PC,
                savedFlags: 0x10002n,
                errorCode: 0,
              },
            },
            'byte sixteen is not fetched',
          );
        });
      },
    },
    {
      name: 'protection keys PKRS MSR works with PKS clear outside IA32e and rejects reserved upper word',
      async run() {
        for (const family of ['non-pae', 'pae', 'ia32e', 'fixed'] as const)
          for (const [code, upper, allowed] of [
            ['0f32', 0n, true],
            ['0f30', 0n, true],
            ['0f30', 1n, false],
          ] as const) {
            const f = fixture(compiler, code, { family });
            f.paging.cr4 &= ~PKS;
            await using(side, f, async (cpu) => {
              cpu.setProtectionKeysState({ pkru: 0x11223344, pkrs: 0x89abcdef });
              const before = state();
              before.registers[0] = 0x1234567880000001n;
              before.registers[1] = 0xabcdef00000006e1n;
              before.registers[2] = 0xaabbccdd00000000n | upper;
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              same(
                result.reason,
                allowed ? 'budget' : 'protection-fault',
                'PKRS independent enable/reserved word',
              );
              const expected = structuredClone(before);
              if (allowed) {
                expected.rip += 2n;
                if (code === '0f32') {
                  expected.registers[0] = 0x89abcdefn;
                  expected.registers[2] = 0n;
                }
              }
              same(result.state, expected, 'MSR zero extension and no-input clobber');
              same(
                cpu.readProtectionKeysState(),
                { pkru: 0x11223344, pkrs: code === '0f30' && allowed ? 0x80000001 : 0x89abcdef },
                'MSR selected bank atomicity',
              );
            });
          }
        const usingPrivate = createFixture(compiler);
        for (const code of ['0f32', '0f30'])
          await usingPrivate(code, async (cpu) => {
            const before = initial(PC, { 1: 0x6e1n }, 0x10202n);
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(result.reason, 'unsupported', 'private generic MSR frontier remains');
            same(result.state, before, 'private unsupported frontier does not begin');
          });
      },
    },
    {
      name: 'protection keys CPUID OSPKE is live for guest toggles and dormant PG0 controls with exact leaf words',
      async run() {
        for (const profile of [
          { mode: 'real', bits: 16, family: 'pg0' },
          { mode: 'long64', bits: 64, family: 'ia32e' },
        ] as const)
          await usingProfile(side, '0fa20f22e00fa2', profile, async (cpu) => {
            for (const enabled of [false, true, false]) {
              const paging = cpu.readPagingState();
              cpu.setPagingState({
                ...paging,
                cr4: enabled ? paging.cr4 | PKE : paging.cr4 & ~PKE,
              });
              for (const subleaf of [0n, 1n]) {
                const before = input(profile);
                before.registers[0] = 7n;
                before.registers[1] = subleaf;
                cpu.setState(before);
                const result = await cpu.run({ budget: 1n });
                same(result.reason, 'budget', 'CPUID executes');
                same(
                  [0, 3, 1, 2].map((index) => result.state.registers[index]),
                  subleaf
                    ? [0n, 0n, 0n, 0n]
                    : [0n, 0x00102489n, enabled ? 0x80000018n : 0x80000008n, 0n],
                  'literal leaf7 words',
                );
              }
            }
          });
        const f = fixture(compiler, '0f22e00fa2');
        f.paging.cr4 &= ~PKE;
        await using(side, f, async (cpu) => {
          const before = state();
          before.registers[0] = f.paging.cr4 | PKE;
          cpu.setState(before);
          same((await cpu.run({ budget: 1n })).reason, 'budget', 'guest enables PKE');
          const cpuid = cpu.readState();
          cpuid.registers[0] = 7n;
          cpuid.registers[1] = 0n;
          cpu.setState(cpuid);
          same(
            (await cpu.run({ budget: 1n })).state.registers[1],
            0x80000018n,
            'cached CPUID reads live control',
          );
        });
      },
    },
    {
      name: 'protection keys WRPKRU retires before TF and zero budget skips all guest bank writes',
      async run() {
        const f = fixture(compiler, '0f01ef');
        await using(side, f, async (cpu) => {
          const before = state(0x102n);
          before.registers[0] = 0x80000001n;
          before.registers[1] = 0n;
          before.registers[2] = 0n;
          cpu.setState(before);
          cpu.setTimeState({ tsc: 100n, tscAux: AUX });
          same((await cpu.run({ budget: 0n })).reason, 'budget', 'no-work skip');
          same(cpu.readProtectionKeysState(), { pkru: 0, pkrs: 0 }, 'zero budget no write');
          const result = await cpu.run({ budget: 1n });
          same(result.reason, 'debug-exception', 'TF after successful bank write');
          same(result.retired, 1n, 'TF observes completed retirement');
          same(
            cpu.readProtectionKeysState(),
            { pkru: 0x80000001, pkrs: 0 },
            'TF does not roll back key write',
          );
          same(cpu.readTimeState(), { tsc: 101n, tscAux: AUX }, 'one key instruction tick');
        });
      },
    },
  ];
}
