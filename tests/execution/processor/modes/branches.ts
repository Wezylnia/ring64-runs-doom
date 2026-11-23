import { Core, usesMemory64 } from '../../../../sdk/src/wasm/core.js';
import { LIMITS, check, equal, exit, fault, memory } from '../../../support/fixture.js';
import { context, input, using, segmentFault, type Scenario } from './fixture.js';

export function branchScenarios(compiler: WebAssembly.Module): Scenario[] {
  const privateSpans = !usesMemory64(compiler);
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
                    privateSpans ? 0 : 2,
                    `${form}: checked read-modify-write count`,
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
                    privateSpans && form === 'private' ? 0 : 4,
                    `${form}: source-aware stack writes`,
                  );
                  equal(
                    admittedSpans,
                    privateSpans && form === 'private' ? 1 : 0,
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
  ];
}

export function callReturnPriorityScenario(compiler: WebAssembly.Module): Scenario {
  return {
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
  };
}
