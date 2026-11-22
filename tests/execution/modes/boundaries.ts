import { createCpu, type RunResult, type SegmentFault } from '../../../sdk/src/index.js';
import { Core } from '../../../sdk/src/wasm/core.js';
import { LIMITS, check, equal, exit, fault, hex, memory, state } from '../../support/fixture.js';
import { changed, context, input, using, type Scenario } from './fixture.js';

function segmentFault(result: RunResult, expected: SegmentFault): void {
  equal(result.fault, undefined, 'exclusive segment diagnostic');
  check(result.segmentFault, 'missing segment failure metadata');
  for (const key of [
    'instructionPc',
    'segment',
    'offset',
    'width',
    'access',
    'cause',
    'failingOffset',
  ] as const)
    equal(result.segmentFault[key], expected[key], `segment fault ${key}`);
}

export function boundaryScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'flat call target tables preserve holes, fetch priority, and short code segments',
      async run() {
        const head = 0x1000n;
        const stop = head + 14n;
        const stack = 0x3040n;
        const known = head + 0x20n;
        const hole = head + 0x10n;
        // TEST EBX,EBX; JZ child; MOV ECX,1; CALL EAX; DEC ECX; JNZ call; HLT.
        // The first RET is unreachable in discovery. The child is INC EBX; RET.
        const code = '85db741cb901000000ffd04975fbf490c3'.padEnd(0x40, '90') + '43c3';
        let checkedTargets = 0;
        const create = Core.create;
        Core.create = async (...args) => {
          const core = await create(...args);
          const imports = Reflect.get(core, 'artifactImports') as (
            observed: boolean,
          ) => Record<string, WebAssembly.ImportValue>;
          Reflect.set(core, 'artifactImports', (observed: boolean) => {
            const env = imports.call(core, observed);
            const checkTarget = env.target_check;
            if (typeof checkTarget === 'function') {
              env.target_check = (target: bigint) => {
                checkedTargets++;
                return checkTarget(target);
              };
            }
            return env;
          });
          return core;
        };
        const setup = { data: [{ base: 0x3000n, bytes: '00'.repeat(64) }] };
        const start = (target: bigint) => input(head, { 0: target, 3: 1n, 4: stack }, 2n);
        try {
          await using(
            compiler,
            code,
            context(),
            async (cpu) => {
              cpu.setState(start(known));
              const hit = await cpu.run({ stop, budget: 20n });
              equal(hit.reason, 'stop', 'the discovered handler returns');
              equal(hit.retired, 8n, 'both near transfers retire exactly once');
              equal(hit.state.registers[3], 2n, 'the discovered handler executes');
              equal(checkedTargets, 0, 'full captured CS admits both near offsets');
              equal(hit.resources.dispatches, 0, 'exact entries stay inside the owner');

              cpu.setState(start(hole));
              const separate = await cpu.run({ stop, budget: 20n });
              equal(
                separate.reason,
                'stop',
                'a table hole still executes its separately decoded RET',
              );
              equal(separate.retired, 7n, 'the hidden RET does not execute the discovered child');
              equal(separate.state.registers[3], 1n, 'the hidden RET performs no child increment');
              check(separate.resources.dispatches > 0, 'the hole needs ordinary dispatch');

              cpu.setState(start(head - 1n));
              const absent = await cpu.run({ stop, budget: 20n });
              equal(absent.reason, 'fetch-fault', 'an unbacked code target faults at fetch');
              equal(absent.state.rip, head - 1n, 'the branch target is committed before fetch');
              equal(absent.state.registers[4], stack - 4n, 'the first CALL has already pushed');
              equal(absent.retired, 4n, 'the CALL retires before its target fetch');
            },
            setup,
          );
          const short = context();
          short.segments.cs.limit = 0x1040;
          checkedTargets = 0;
          await using(
            compiler,
            code,
            short,
            async (cpu) => {
              cpu.setState(start(0x2000n));
              const denied = await cpu.run({ stop, budget: 20n });
              equal(
                denied.reason,
                'segment-fault',
                'the short CS refuses the target before pushing',
              );
              equal(denied.state.rip, head + 9n, 'the CALL remains unstarted');
              equal(denied.state.registers[4], stack, 'no return address was pushed');
              equal(denied.retired, 3n, 'only the original prefix retired');
              equal(checkedTargets, 1, 'the checked target helper owns the CS fault');
            },
            setup,
          );
        } finally {
          Core.create = create;
        }
      },
    },
    {
      name: 'linear indirect call spans keep stack admission and source observation exact',
      async run() {
        const head = 0x1000n;
        const stop = head + 14n;
        const stack = 0x3040n;
        const destination = 0x2000n;
        // TEST EBX,EBX; JZ child; MOV ECX,2; again: CALL EAX; DEC ECX; JNZ again; HLT.
        // child: PUSH EBX; MOV EBX,[ESP+4]; MOV [EDI],EBX; MOV EBX,[EDI];
        // ADD DWORD [EDI+4],1; POP EBX; RET.
        const code =
          '85db741cb902000000ffd04975fbf4'.padEnd(0x40, '90') + '538b5c2404891f8b1f834704015bc3';
        let checkedWrites = 0;
        let admittedSpans = 0;
        let checkedRmw = 0;
        const create = Core.create;
        Core.create = async (...args) => {
          const core = await create(...args);
          const imports = Reflect.get(core, 'artifactImports') as (
            observed: boolean,
          ) => Record<string, WebAssembly.ImportValue>;
          Reflect.set(core, 'artifactImports', (observed: boolean) => {
            const env = imports.call(core, observed);
            const store = env.segmented_store;
            const span = env.write_span;
            const rmw = env.segmented_load_for_write;
            if (typeof rmw === 'function') {
              env.segmented_load_for_write = (...args: (number | bigint)[]) => {
                checkedRmw++;
                return rmw(...args);
              };
            }
            if (typeof store === 'function') {
              env.segmented_store = (
                address: bigint,
                width: number,
                value: bigint,
                segment: number,
              ) => {
                if (segment === 1) checkedWrites++;
                return store(address, width, value, segment);
              };
            }
            if (typeof span === 'function') {
              env.write_span = (address: bigint, width: number, segment: number) => {
                const result = span(address, width, segment);
                if (segment === 1 && result !== 0) admittedSpans++;
                return result;
              };
            }
            return env;
          });
          return core;
        };
        try {
          for (const form of ['private', 'executable', 'readonly', 'segment', 'split-ds']) {
            checkedWrites = admittedSpans = checkedRmw = 0;
            const ctx = context();
            if (form === 'segment') ctx.segments.ss.limit = 0x303a;
            await using(
              compiler,
              code,
              ctx,
              async (cpu) => {
                const start = input(head, { 0: head + 0x20n, 3: 1n, 4: stack, 7: destination }, 2n);
                cpu.setState(start);
                const result = await cpu.run({ stop, budget: 40n });
                if (form === 'split-ds') {
                  equal(
                    result.reason,
                    'data-fault',
                    'a separate readonly destination refuses cached RMW',
                  );
                  equal(result.state.rip, head + 0x29n, 'the original ADD faults');
                  equal(result.retired, 8n, 'only the prefix preceding the ADD retires');
                  equal(
                    result.state.registers[4],
                    stack - 8n,
                    'the CALL and PUSH remain committed',
                  );
                  equal(result.state.flags, 2n, 'failed ADD preserves incoming flags');
                  equal(checkedRmw, 1, 'split permission goes through the checked RMW');
                  equal(
                    (await cpu.readMemory(destination + 4n, 4))[0],
                    1,
                    'the readonly dword is unchanged',
                  );
                  return;
                }
                equal(result.state.registers[4], stack, `${form}: the stack pointer is precise`);
                if (form === 'readonly' || form === 'segment') {
                  equal(result.reason, form === 'readonly' ? 'data-fault' : 'segment-fault', form);
                  equal(result.state.rip, head + 9n, `${form}: the refused CALL is unstarted`);
                  equal(result.retired, 3n, `${form}: only the caller's original prefix retires`);
                  equal(admittedSpans, 0, `${form}: refused write span`);
                } else {
                  equal(result.reason, 'stop', form);
                  equal(result.retired, 23n, `${form}: both calls and returns retire`);
                  equal(
                    checkedRmw,
                    0,
                    `${form}: the data RMW uses cached read and write permissions`,
                  );
                  equal(
                    (await cpu.readMemory(destination + 4n, 4))[0],
                    3,
                    `${form}: both ADDs commit`,
                  );
                  equal(
                    result.state.registers[3],
                    1n,
                    `${form}: the child pops the saved register`,
                  );
                  equal(
                    (await cpu.readMemory(destination, 4))[0],
                    11,
                    `${form}: the return PC is readable`,
                  );
                  equal(
                    checkedWrites,
                    form === 'private' ? 0 : 4,
                    `${form}: source-aware stack writes`,
                  );
                  equal(
                    admittedSpans,
                    form === 'private' ? 1 : 0,
                    `${form}: a retained private span`,
                  );
                }
              },
              {
                data: [
                  {
                    base: destination,
                    bytes: form === 'split-ds' ? '00000000' : '0000000001000000',
                  },
                  ...(form === 'split-ds'
                    ? [{ base: destination + 4n, bytes: '01000000', write: false }]
                    : []),
                  {
                    base: 0x3000n,
                    bytes: '00'.repeat(64),
                    write: form !== 'readonly',
                    execute: form === 'executable',
                  },
                ],
              },
            );
          }
        } finally {
          Core.create = create;
        }
      },
    },
    {
      name: 'flat regions admit returning branches after a different preamble owner',
      async run() {
        const head = 0x1000n;
        const loop = head + 0x20n;
        const handler = head + 0x80n;
        const table = 0x2000n;
        const stack = 0x3040n;
        // MOV EAX,loop; JMP EAX; loop: MOV ECX,640; MOV EAX,[EDI];
        // CALL EAX; DEC ECX; JNZ again; NOP. Handler: ADD EBX,1; RET.
        const code =
          'b820100000ffe0'.padEnd(0x40, '82') +
          'b9800200008b07ffd04975f990'.padEnd(0xc0, '82') +
          '83c301c3';
        await using(
          compiler,
          code,
          context(),
          async (cpu) => {
            const start = input(head, { 3: 0n, 4: stack, 7: table }, 2n);
            const execute = async (value: bigint) => {
              cpu.setState(start);
              const result = await cpu.run({ stop: loop + 12n, budget: 4000n });
              equal(result.reason, 'stop', 'the caller reaches the same stop');
              equal(result.retired, 3843n, 'the exact guest instruction count');
              equal(result.state.registers[3], value, 'the active handler executed');
              equal(result.state.registers[4], stack, 'the stack returns to its starting address');
              return result.resources.dispatches;
            };
            check((await execute(640n)) <= 550, 'admission reduces the cold dispatch loop');
            check((await execute(640n)) <= 4, 'the cached region absorbs returns on the warm run');
            check(
              cpu
                .inspectTranslations()
                .every((owner) => owner.instructionCount <= LIMITS.maxInstructions),
              'the region fits the existing instruction cap',
            );

            cpu.writeMemory(handler + 2n, Uint8Array.of(3));
            await execute(1920n);
            cpu.setState({ ...start, flags: 0x102n });
            const stepped = await cpu.run({ stop: loop + 12n, budget: 4000n });
            equal(stepped.reason, 'debug-exception', 'the observed unit still single-steps');
            equal(stepped.retired, 1n, 'only the initial MOV retires under TF');
          },
          {
            codeWrite: true,
            data: [
              { base: table, bytes: '80100000' },
              { base: 0x3000n, bytes: '00'.repeat(64) },
            ],
          },
        );
      },
    },
    {
      name: 'flat stack RMW requires both cached permissions and the full segment span',
      async run() {
        // MOV EAX,[ESP]; MOV [ESP+4],EAX; INC DWORD [ESP+8]; DEC ECX; JNZ head; NOP.
        const code = '8b042489442404ff4424084975f290';
        const start = input(0x1000n, { 1: 3n, 4: 0x2000n }, 3n);
        let checks = 0;
        const create = Core.create;
        Core.create = async (...args) => {
          const core = await create(...args);
          const imports = Reflect.get(core, 'artifactImports') as (
            observed: boolean,
          ) => Record<string, WebAssembly.ImportValue>;
          Reflect.set(core, 'artifactImports', (observed: boolean) => {
            const env = imports.call(core, observed);
            const rmw = env.segmented_load_for_write;
            if (typeof rmw === 'function') {
              env.segmented_load_for_write = (...args: (number | bigint)[]) => {
                checks++;
                return rmw(...args);
              };
            }
            return env;
          });
          return core;
        };
        try {
          for (const scenario of ['allowed', 'executable', 'readonly', 'limit']) {
            checks = 0;
            const ctx = context();
            if (scenario === 'limit') ctx.segments.ss.limit = 0x200a;
            const data =
              scenario === 'readonly'
                ? [
                    { base: 0x2000n, bytes: '0300000000000000' },
                    { base: 0x2008n, bytes: '02000000', write: false },
                  ]
                : [
                    {
                      base: 0x2000n,
                      bytes: '030000000000000002000000',
                      execute: scenario === 'executable',
                    },
                  ];
            await using(
              compiler,
              code,
              ctx,
              async (cpu) => {
                cpu.setState(start);
                const result = await cpu.run({ stop: 0x100en, budget: 100n });
                if (scenario === 'allowed' || scenario === 'executable') {
                  equal(result.reason, 'stop', 'all three laps complete');
                  equal(result.retired, 15n, 'each RMW retires once');
                  equal(result.state.flags & 1n, 1n, 'INC and DEC preserve carry');
                  equal(
                    checks,
                    scenario === 'allowed' ? 0 : 3,
                    'only non-executable RAM gets both proofs',
                  );
                  memory(cpu, 0x2000n, '030000000300000005000000');
                } else {
                  equal(
                    result.reason,
                    scenario === 'limit' ? 'segment-fault' : 'data-fault',
                    scenario,
                  );
                  equal(result.retired, 2n, 'only the prefix retires on a refusal');
                  equal(result.state.rip, 0x1007n, 'RIP points to the faulting INC');
                  equal(result.state.flags, 3n, 'the fault does not update arithmetic flags');
                  equal(checks, 1, 'the checked RMW reports the fault');
                  memory(cpu, 0x2000n, '0300000003000000');
                  memory(cpu, 0x2008n, '02000000');
                }
              },
              { data },
            );
          }
        } finally {
          Core.create = create;
        }
      },
    },
    {
      name: 'rotated flat stack loop settles its prefix before a fault or a partial lap',
      async run() {
        // JMP head; spill: MOV EAX,[ESP]; MOV [ESP+4],EAX; INC DWORD [ESP+8];
        // MOV ESI,[ESP+12]; head: ADD EAX,[ESI]; DEC ECX; JZ done;
        // TEST ECX,1; JNZ spill; JMP head; done: NOP.
        const code = 'eb0f8b042489442404ff4424088b74240c030649740af7c10100000075e4ebf190';
        const registers = { 0: 1n, 1: 3n, 4: 0x2000n, 6: 0x3000n };
        const stackSpans: boolean[] = [];
        const create = Core.create;
        Core.create = async (...args) => {
          const core = await create(...args);
          const imports = Reflect.get(core, 'artifactImports') as (
            observed: boolean,
          ) => Record<string, WebAssembly.ImportValue>;
          Reflect.set(core, 'artifactImports', (observed: boolean) => {
            const env = imports.call(core, observed);
            const read = env.read_span;
            if (typeof read === 'function') {
              env.read_span = (address: bigint, bytes: number, segment: number) => {
                const result = read(address, bytes, segment);
                if (address === 0x2000n && bytes === 16) stackSpans.push(result !== 0);
                return result;
              };
            }
            return env;
          });
          return core;
        };
        try {
          for (const scenario of ['complete', 'split', 'fault', 'budget']) {
            const tail = scenario === 'fault' ? '00400000' : '00300000';
            const stack =
              scenario === 'split'
                ? [
                    { base: 0x2000n, bytes: '0300000000000000' },
                    { base: 0x2008n, bytes: '02000000' + tail },
                  ]
                : [{ base: 0x2000n, bytes: '030000000000000002000000' + tail }];
            await using(
              compiler,
              code,
              context(),
              async (cpu) => {
                cpu.setState(input(0x1000n, registers, 3n));
                const partial = scenario === 'budget';
                const failed = scenario === 'fault';
                const result = await cpu.run({ stop: 0x1020n, budget: partial ? 14n : 100n });
                equal(result.reason, partial ? 'budget' : failed ? 'data-fault' : 'stop', scenario);
                equal(result.retired, partial ? 14n : failed ? 16n : 19n, 'retired prefix');
                equal(result.state.rip, partial ? 0x1009n : failed ? 0x1011n : 0x1020n, 'next RIP');
                if (failed) {
                  equal(result.fault?.address, 0x4000n, 'the second stream is unmapped');
                  equal(result.state.flags & 0x8d5n, 4n, 'the last committed INC leaves PF set');
                }
                memory(cpu, 0x2000n, '0300000003000000');
                memory(cpu, 0x2008n, (partial ? '02000000' : '03000000') + tail);
              },
              { data: [...stack, { base: 0x3000n, bytes: '05000000' }] },
            );
          }
          equal(stackSpans.length, 4, 'one checked stack span per entry to the clone');
          equal(
            stackSpans.join(','),
            'true,false,true,true',
            'a split span refuses only the proof',
          );
        } finally {
          Core.create = create;
        }
      },
    },
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
    {
      name: 'SP16 wraps independently of a full four-byte stack slot',
      async run() {
        for (const push of [false, true])
          for (const limit of [0xffffffff, 0xffff]) {
            const ctx = context('protected', true, false);
            ctx.segments.ss.base = 0x20000n;
            ctx.segments.ss.limit = limit;
            await using(
              compiler,
              push ? '54' : '58',
              ctx,
              async (cpu) => {
                const before = input(0x1000n, {
                  4: push ? 0x1122334400010003n : 0x112233440001ffffn,
                });
                cpu.setState(before);
                const result = await cpu.run({ budget: 1n });
                if (limit === 0xffff) {
                  exit(result, 'segment-fault', 1n, 0n, before);
                  segmentFault(result, {
                    instructionPc: 0x1000n,
                    segment: 'ss',
                    offset: 0xffffn,
                    width: 4,
                    access: push ? 'write' : 'read',
                    cause: 'limit',
                    failingOffset: 0x10000n,
                  });
                } else
                  exit(
                    result,
                    'budget',
                    1n,
                    1n,
                    changed(
                      before,
                      0x1001n,
                      push
                        ? { 4: 0x112233440001ffffn }
                        : { 0: 0x1122334412345678n, 4: 0x1122334400010003n },
                    ),
                  );
                memory(
                  cpu,
                  0x2ffffn,
                  push && limit !== 0xffff ? '03000100' : push ? 'a5a5a5a5' : '78563412',
                );
              },
              { data: [{ base: 0x2ffffn, bytes: push ? 'a5a5a5a5' : '78563412' }] },
            );
          }
      },
    },
    {
      name: 'POP pointer aliases merge after incrementing the selected stack view',
      async run() {
        for (const row of [
          { code: '665c', stack32: true, data: 'efbe', after: 0x112233440001beefn },
          { code: '5c', stack32: false, data: 'efbeadde', after: 0x11223344deadbeefn },
        ]) {
          const ctx = context('protected', true, row.stack32);
          ctx.segments.ss.base = 0x20000n;
          await using(
            compiler,
            row.code,
            ctx,
            async (cpu) => {
              const before = input(0x1000n, { 4: 0x112233440000ffffn });
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              exit(
                result,
                'budget',
                1n,
                1n,
                changed(before, 0x1000n + BigInt(row.code.length / 2), { 4: row.after }),
              );
              equal(result.fault, undefined, 'no linear fault');
              equal(result.segmentFault, undefined, 'no segment fault');
              memory(cpu, 0x2ffffn, row.data);
            },
            { data: [{ base: 0x2ffffn, bytes: row.data }] },
          );
        }
      },
    },
    {
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
    },
    {
      name: 'CALL checks its target before SS while RET reads SS before its target',
      async run() {
        for (const row of [
          { code: '64e8fa000000', stack: undefined, call: true },
          { code: '64c3', stack: undefined, call: false },
          { code: '64c3', stack: '00110000', call: false },
        ]) {
          const ctx = context();
          ctx.segments.cs.limit = 0x10ff;
          ctx.segments.fs.usable = false;
          await using(
            compiler,
            row.code,
            ctx,
            async (cpu) => {
              const before = input(0x1000n, { 4: 0x4000n });
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              if (row.call || row.stack !== undefined) {
                exit(result, 'segment-fault', 1n, 0n, before);
                segmentFault(result, {
                  instructionPc: 0x1000n,
                  segment: 'cs',
                  offset: 0x1100n,
                  width: 1,
                  access: 'execute',
                  cause: 'limit',
                  failingOffset: 0x1100n,
                });
              } else {
                exit(result, 'data-fault', 1n, 0n, before);
                fault(result, {
                  instructionPc: 0x1000n,
                  segment: 'ss',
                  address: 0x4000n,
                  width: 4,
                  access: 'read',
                  cause: 'unmapped',
                  failingAddress: 0x4000n,
                });
              }
              memory(cpu, 0x1000n, row.code);
              if (row.stack) memory(cpu, 0x4000n, row.stack);
            },
            { data: row.stack ? [{ base: 0x4000n, bytes: row.stack }] : [] },
          );
        }
      },
    },
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
