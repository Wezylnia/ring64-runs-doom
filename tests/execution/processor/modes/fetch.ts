import { createCpu } from '../../../../sdk/src/index.js';
import { check, equal, exit, hex, memory } from '../../../support/fixture.js';
import { changed, context, input, using, segmentFault, type Scenario } from './fixture.js';

export function fetchScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'nonzero CS source owners follow host and guest writes through a linear alias',
      async run() {
        const ctx = context();
        ctx.segments.cs.base = 0x20000n;
        ctx.segments.ds.base = 0x30000n;
        await using(
          compiler,
          'c603bbb801000000',
          ctx,
          async (cpu) => {
            cpu.setMappings([
              {
                base: 0x21000n,
                backing: 'code',
                offset: 0,
                length: 8,
                permissions: { read: true, write: false, execute: true },
              },
              {
                base: 0x31000n,
                backing: 'code',
                offset: 0,
                length: 8,
                permissions: { read: true, write: true, execute: false },
              },
            ]);
            const target = input(0x1003n);
            cpu.setState(target);
            exit(
              await cpu.run({ budget: 1n }),
              'budget',
              1n,
              1n,
              changed(target, 0x1008n, { 0: 0x1122334400000001n }),
            );
            const owner = cpu.inspectTranslations()[0]!;
            check(owner.entries.includes(0x1003n), 'logical retained entry');
            check(
              owner.dependencies.some((range) => range.start === 0x21003n),
              'translated source range',
            );
            cpu.writeMemory(0x31004n, Uint8Array.of(9));
            equal(cpu.inspectTranslations().length, 0, 'host alias patch retires logical owner');
            cpu.setState(target);
            const patched = await cpu.run({ budget: 1n });
            exit(patched, 'budget', 1n, 1n, changed(target, 0x1008n, { 0: 0x1122334400000009n }));
            equal(patched.resources.compilations, 1, 'patched target recompiles');
            const writer = input(0x1000n, { 3: 0x1122334400001003n });
            cpu.setState(writer);
            const result = await cpu.run({ budget: 2n });
            exit(result, 'budget', 2n, 2n, changed(writer, 0x1008n, { 3: 0x1122334400000009n }));
            equal(
              result.resources.dispatches,
              1,
              'active source patch dispatches after retirement',
            );
            check(
              !cpu.inspectTranslations().some((unit) => unit.entries.includes(0x1000n)),
              'stale writer owner removed',
            );
            memory(cpu, 0x21000n, 'c603bbbb09000000');
            memory(cpu, 0x31000n, 'c603bbbb09000000');
          },
          { codeBase: 0x21000n },
        );
      },
    },
    {
      name: 'EIP completion wraps while required bytes and reached raw RIP stay in the offset domain',
      async run() {
        for (const code of ['f5', '66'])
          await using(
            compiler,
            code,
            context(),
            async (cpu) => {
              const before = input(0xffffffffn);
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              if (code === 'f5') {
                exit(result, 'budget', 1n, 1n, changed(before, 0n, {}, 0x8d6n));
                cpu.setState(before);
                exit(await cpu.run({ budget: 2n }), 'budget', 2n, 2n, changed(before, 1n));
              } else {
                exit(result, 'segment-fault', 1n, 0n, before);
                segmentFault(result, {
                  instructionPc: 0xffffffffn,
                  segment: 'cs',
                  offset: 0x100000000n,
                  width: 1,
                  access: 'execute',
                  cause: 'limit',
                  failingOffset: 0x100000000n,
                });
              }
              const raw = input(0x100000000n);
              cpu.setState(raw);
              exit(await cpu.run({ budget: 0n }), 'budget', 0n, 0n, raw);
              exit(await cpu.run({ budget: 1n, stop: raw.rip }), 'stop', 1n, 0n, raw);
              const reached = await cpu.run({ budget: 1n });
              exit(reached, 'segment-fault', 1n, 0n, raw);
              segmentFault(reached, {
                instructionPc: raw.rip,
                segment: 'cs',
                offset: raw.rip,
                width: 1,
                access: 'execute',
                cause: 'limit',
                failingOffset: raw.rip,
              });
              memory(cpu, 0xffffffffn, code);
              memory(cpu, 0n, 'f5');
            },
            { entry: 0xffffffffn, data: [{ base: 0n, bytes: 'f5', execute: true }] },
          );
      },
    },
    {
      name: 'required code bytes translate separately across the linear32 boundary',
      async run() {
        const ctx = context();
        ctx.segments.cs.base = 0xffffe000n;
        const cpu = await createCpu({
          compiler,
          context: ctx,
          entry: 0x1ffen,
          memory: {
            backings: [{ id: 'code', bytes: hex('b878563412') }],
            mappings: [
              {
                base: 0xfffffffen,
                backing: 'code',
                offset: 0,
                length: 2,
                permissions: { read: true, write: false, execute: true },
              },
              {
                base: 0n,
                backing: 'code',
                offset: 2,
                length: 3,
                permissions: { read: true, write: false, execute: true },
              },
              {
                base: 0x4000n,
                backing: 'code',
                offset: 0,
                length: 5,
                permissions: { read: true, write: true, execute: false },
              },
            ],
          },
          memoryLimits: { maxBackings: 1, maxMappings: 3, maxBackingBytes: 5 },
          limits: { ...LIMITS, maxInstructions: 1, maxSourceBytes: 5 },
        });
        try {
          const before = input(0x1ffen);
          cpu.setState(before);
          const result = await cpu.run({ budget: 1n });
          exit(result, 'budget', 1n, 1n, changed(before, 0x2003n, { 0: 0x1122334412345678n }));
          const ranges = cpu.inspectTranslations()[0]!.dependencies;
          check(
            ranges.some((range) => range.start === 0n),
            'wrapped low source range',
          );
          check(
            ranges.some((range) => range.start === 0xfffffffen),
            'high source range',
          );
          cpu.writeMemory(0x4001n, Uint8Array.of(9));
          equal(cpu.inspectTranslations().length, 0, 'wrapped-source alias patch retires owner');
          cpu.setState(before);
          exit(
            await cpu.run({ budget: 1n }),
            'budget',
            1n,
            1n,
            changed(before, 0x2003n, { 0: 0x1122334412345609n }),
          );
          memory(cpu, 0x4000n, 'b809563412');
        } finally {
          cpu.dispose();
        }
      },
    },
  ];
}
