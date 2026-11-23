import { Core } from '../../../../sdk/src/wasm/core.js';
import { equal, exit, memory } from '../../../support/fixture.js';
import { changed, context, input, using, segmentFault, type Scenario } from './fixture.js';

export function stackLoopScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
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
  ];
}

export function stackPointerScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
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
  ];
}
