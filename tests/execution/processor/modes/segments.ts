import { check, equal, exit, fault, memory, state } from '../../../support/fixture.js';
import { changed, context, input, using, segmentFault, type Scenario } from './fixture.js';

export function segmentAccessScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'flat segment limits still apply after a scalar load warmed the mapping',
      async run() {
        const ctx = context();
        ctx.segments.ds.limit = 0x2000;
        await using(
          compiler,
          '8a038b03ebfa', // mov al,[ebx]; mov eax,[ebx]; jmp back
          ctx,
          async (cpu) => {
            const before = input(0x1000n, { 3: 0x2000n }, 2n);
            cpu.setState(before);
            const result = await cpu.run({ budget: 2n });
            exit(
              result,
              'segment-fault',
              2n,
              1n,
              changed(before, 0x1002n, { 0: 0x1122334455667711n }),
            );
            segmentFault(result, {
              instructionPc: 0x1002n,
              segment: 'ds',
              offset: 0x2000n,
              width: 4,
              access: 'read',
              cause: 'limit',
              failingOffset: 0x2001n,
            });
          },
          { data: [{ base: 0x2000n, bytes: '11223344' }] },
        );
      },
    },
    {
      name: 'flat direct spans check both ends after caching the first byte',
      async run() {
        for (const scenario of [
          'last-dword',
          'crossing-read',
          'below-low-span',
          'wrapped-below-span',
          'crossing-write',
        ] as const) {
          const low = scenario === 'below-low-span';
          const below = low || scenario === 'wrapped-below-span';
          const store = scenario === 'crossing-write';
          const valid = scenario === 'last-dword';
          const base = low ? 0x3000n : 0xfffffffcn;
          const offset = valid || below ? base : 0xffffffffn;
          const stop = 0x1004n;
          const before = input(0x1000n, { 3: offset, 6: low ? base - 1n : 0n }, 2n);
          const warmed = store
            ? before.registers[0]
            : 0x1122334455667700n | (low ? 0x92n : below ? 0x91n : 0x94n);
          const code = store
            ? '88038903ebfa'
            : low
              ? '8a038a43018a06ebf7'
              : below
                ? '8a038a06ebfa'
                : '8a038b03ebfa';
          // The low span's second load must hit in the already retained span.
          const failPc = low ? 0x1005n : 0x1002n;
          await using(
            compiler,
            code,
            context(),
            async (cpu) => {
              cpu.setState(before);
              const result = await cpu.run({ stop, budget: 20n });
              if (valid) {
                exit(result, 'stop', 20n, 2n, changed(before, stop, { 0: 0x1122334494939291n }));
                return;
              }
              exit(
                result,
                below ? 'data-fault' : 'segment-fault',
                20n,
                low ? 2n : 1n,
                changed(before, failPc, { 0: warmed }),
              );
              if (below) {
                fault(result, {
                  instructionPc: failPc,
                  address: low ? base - 1n : 0n,
                  width: 1,
                  access: 'read',
                  cause: 'unmapped',
                  failingAddress: low ? base - 1n : 0n,
                  segment: 'ds',
                });
              } else {
                segmentFault(result, {
                  instructionPc: failPc,
                  segment: 'ds',
                  offset,
                  width: 4,
                  access: store ? 'write' : 'read',
                  cause: 'limit',
                  failingOffset: 0x1_0000_0000n,
                });
              }
              memory(cpu, base, store ? '9192938895969798' : '9192939495969798');
            },
            { data: [{ base, bytes: '9192939495969798' }] },
          );
        }
        const nonflat = context();
        nonflat.segments.ds.base = 0x2000n;
        const before = input(0x1000n, { 3: 0x1000n }, 2n);
        const stop = 0x1004n;
        await using(
          compiler,
          '8a038b03ebfa',
          nonflat,
          async (cpu) => {
            cpu.setState(before);
            exit(
              await cpu.run({ stop, budget: 20n }),
              'stop',
              20n,
              2n,
              changed(before, stop, { 0: 0x1122334494939291n }),
            );
          },
          { data: [{ base: 0x3000n, bytes: '91929394' }] },
        );
      },
    },
    {
      name: 'flat reads across two owners follow remapping, replacement, and permission revocation',
      async run() {
        const first = 0x1000n;
        const second = 0x2000n;
        const data = 0x3000n;
        const code = '8b03ebfc'; // MOV EAX,[EBX]; JMP back
        const codeMap = (pc: bigint, backing: string) => ({
          base: pc,
          backing,
          offset: 0,
          length: 4,
          permissions: { read: true, write: false, execute: true },
        });
        const dataMap = (read: boolean) => ({
          base: data,
          backing: 'data2',
          offset: 0,
          length: 4,
          permissions: { read, write: true, execute: false },
        });
        const remap = [codeMap(first, 'code'), codeMap(second, 'data0')];
        await using(
          compiler,
          code,
          context(),
          async (cpu) => {
            const read = async (pc: bigint, value: bigint) => {
              const before = input(pc, { 3: data }, 2n);
              cpu.setState(before);
              const result = await cpu.run({ stop: pc + 2n, budget: 20n });
              exit(result, 'stop', 20n, 1n, changed(before, pc + 2n, { 0: value }));
            };
            await read(first, 0x1122334444332211n);
            await read(second, 0x1122334444332211n);
            cpu.setMappings([...remap, dataMap(true)]);
            cpu.removeBacking('data1');
            await read(first, 0x11223344ddccbbaan);
            await read(second, 0x11223344ddccbbaan);
            cpu.setMappings([...remap, dataMap(false)]);
            const before = input(second, { 3: data }, 2n);
            cpu.setState(before);
            const denied = await cpu.run({ stop: second + 2n, budget: 20n });
            exit(denied, 'data-fault', 20n, 0n, before);
            fault(denied, {
              instructionPc: second,
              address: data,
              width: 4,
              access: 'read',
              cause: 'permission',
              failingAddress: data,
              segment: 'ds',
            });
          },
          {
            data: [
              { base: second, bytes: code, write: false, execute: true },
              { base: data, bytes: '11223344' },
              { base: 0x4000n, bytes: 'aabbccdd' },
            ],
          },
        );
      },
    },
    {
      name: 'segment usability and type precede bounds and mapping checks',
      async run() {
        for (const row of [
          { code: '8b03', type: 'code', usable: false, access: 'read', cause: 'unusable' },
          { code: '8903', type: 'read-only', usable: true, access: 'write', cause: 'type' },
          { code: 'd323', type: 'code', usable: true, access: 'write', cause: 'type' },
          { code: '0f4403', type: 'code', usable: true, access: 'read', cause: 'type' },
        ] as const) {
          const ctx = context();
          Object.assign(ctx.segments.ds, { type: row.type, usable: row.usable, limit: 0 });
          await using(compiler, row.code, ctx, async (cpu) => {
            const before = input(0x1000n, { 1: 0n, 3: 0x2000n }, 2n);
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            exit(result, 'segment-fault', 1n, 0n, before);
            segmentFault(result, {
              instructionPc: 0x1000n,
              segment: 'ds',
              offset: 0x2000n,
              width: 4,
              access: row.access,
              cause: row.cause,
              failingOffset: 0x2000n,
            });
            memory(cpu, 0x1000n, row.code);
          });
        }
      },
    },
    {
      name: 'repairing a segment fault resumes with incompatible owners dormant and repaired code reusable',
      async run() {
        const broken = context();
        broken.segments.ds.base = 0x10000n;
        broken.segments.ds.usable = false;
        await using(
          compiler,
          '8b03',
          broken,
          async (cpu) => {
            const before = input(0x1000n, { 3: 0x2000n });
            cpu.setState(before);
            const failed = await cpu.run({ budget: 1n });
            exit(failed, 'segment-fault', 1n, 0n, before);
            segmentFault(failed, {
              instructionPc: 0x1000n,
              segment: 'ds',
              offset: 0x2000n,
              width: 4,
              access: 'read',
              cause: 'unusable',
              failingOffset: 0x2000n,
            });
            const faultOwner = cpu.inspectTranslations()[0]!.owner;
            cpu.setContext(broken);
            equal(
              cpu.inspectTranslations()[0]!.owner,
              faultOwner,
              'equal context retains faulting owner',
            );
            const repaired = context();
            repaired.segments.ds.base = 0x10000n;
            cpu.setContext(repaired);
            state(cpu.readState(), before);
            equal(
              cpu.inspectTranslations()[0]!.owner,
              faultOwner,
              'repair keeps incompatible owner dormant',
            );
            const resumed = await cpu.run({ budget: 1n });
            const after = changed(before, 0x1002n, { 0: 0x1122334412345678n });
            exit(resumed, 'budget', 1n, 1n, after);
            equal(resumed.fault, undefined, 'repaired linear diagnostic cleared');
            equal(resumed.segmentFault, undefined, 'repaired segment diagnostic cleared');
            const repairedOwner = cpu
              .inspectTranslations()
              .find((entry) => entry.owner !== faultOwner)!.owner;
            check(repairedOwner !== faultOwner, 'repair uses a fresh owner token');
            cpu.setContext(repaired);
            check(
              cpu.inspectTranslations().some((entry) => entry.owner === repairedOwner),
              'equal repair retains owner',
            );
            cpu.setState(before);
            const reused = await cpu.run({ budget: 1n });
            exit(reused, 'budget', 1n, 1n, after);
            equal(reused.resources.compilations, 0, 'same context reuses warm code');
            const other = context();
            other.segments.ds.base = 0x20000n;
            cpu.setContext(other);
            cpu.setContext(repaired);
            state(cpu.readState(), after);
            equal(
              cpu.inspectTranslations().length,
              2,
              'context round trip retains both valid owners',
            );
            cpu.setState(before);
            const rebuilt = await cpu.run({ budget: 1n });
            exit(rebuilt, 'budget', 1n, 1n, after);
            equal(rebuilt.resources.compilations, 0, 'context round trip reuses repaired code');
            check(
              cpu.inspectTranslations().some((entry) => entry.owner === repairedOwner),
              'context round trip keeps the installed token',
            );
            memory(cpu, 0x1000n, '8b03');
            memory(cpu, 0x11fffn, 'a578563412a5');
          },
          { data: [{ base: 0x11fffn, bytes: 'a578563412a5' }] },
        );
      },
    },
    {
      name: 'expand-down accesses check the full span and admit empty imported ranges',
      async run() {
        for (const row of [
          { limit: 0xff00, offset: 0xfffcn, failing: undefined },
          { limit: 0xff00, offset: 0xfffen, failing: 0x10000n },
          { limit: 0xffff, offset: 0xfffcn, failing: 0xfffcn },
        ]) {
          const ctx = context();
          Object.assign(ctx.segments.ds, {
            type: 'expand-down-read-write',
            base: 0x10000n,
            limit: row.limit,
            default32: false,
          });
          await using(
            compiler,
            '8b03',
            ctx,
            async (cpu) => {
              const before = input(0x1000n, { 3: row.offset });
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              if (row.failing === undefined)
                exit(
                  result,
                  'budget',
                  1n,
                  1n,
                  changed(before, 0x1002n, { 0: 0x1122334412345678n }),
                );
              else {
                exit(result, 'segment-fault', 1n, 0n, before);
                segmentFault(result, {
                  instructionPc: 0x1000n,
                  segment: 'ds',
                  offset: row.offset,
                  width: 4,
                  access: 'read',
                  cause: 'limit',
                  failingOffset: row.failing,
                });
              }
              memory(cpu, 0x1fffcn, '78563412a5a5');
            },
            { data: [{ base: 0x1fffcn, bytes: '78563412a5a5' }] },
          );
        }
      },
    },
  ];
}

export function longModeSegmentFaultScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'reached long64 faults retain actual base-register segment provenance',
    async run() {
      for (const row of [
        { code: '8b4500', segment: 'ss', address: 0x2000n },
        { code: '8b0424', segment: 'ss', address: 0x2000n },
        { code: '418b4500', segment: 'ds', address: 0x2000n },
        { code: '418b0424', segment: 'ds', address: 0x2000n },
        { code: '678b4500', segment: 'ss', address: 0x2000n },
        { code: '67418b4500', segment: 'ds', address: 0x2000n },
        { code: '8b042d00100000', segment: 'ds', address: 0x3000n },
        { code: '368b00', segment: 'ds', address: 0x2000n },
        { code: '3e8b4500', segment: 'ss', address: 0x2000n },
        { code: '648b4500', segment: 'fs', address: 0x12000n },
      ] as const)
        await using(
          compiler,
          row.code,
          { mode: 'long64', fsBase: 0x10000n, gsBase: 0n },
          async (cpu) => {
            const before = input(0x1000n, {
              0: 0x2000n,
              4: 0x2000n,
              5: 0x2000n,
              12: 0x2000n,
              13: 0x2000n,
            });
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            exit(result, 'data-fault', 1n, 0n, before);
            fault(result, {
              instructionPc: 0x1000n,
              segment: row.segment,
              address: row.address,
              width: 4,
              access: 'read',
              cause: 'unmapped',
              failingAddress: row.address,
            });
            memory(cpu, 0x1000n, row.code);
          },
        );
    },
  };
}
