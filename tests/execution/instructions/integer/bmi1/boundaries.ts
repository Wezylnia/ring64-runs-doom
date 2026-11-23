import { createCpu } from '../../../../../sdk/src/index.js';
import {
  check,
  CODE,
  createFixture,
  DATA,
  equal,
  exit,
  hex,
  initial,
  type Scenario,
} from '../../../../support/fixture.js';
import {
  fixture as aligned,
  runOne,
  state as userState,
  using as usingPhysical,
} from '../../../memory/alignment-checking/fixture.js';
import { preserveNumeric } from '../../../far-control/fixture.js';
import { legacyOptions, little } from '../../../memory/legacy-paging/fixture.js';
import { context, LONG, using } from '../../../modes/fixture.js';

export function boundaryScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'BMI1 crossing data reads precede zero-length extraction and select one debug trap only on a repaired complete read',
      async run() {
        const cpu = await createCpu(legacyOptions(compiler, false, 'c4e270f703')); // BEXTR EAX,[EBX],ECX
        try {
          const full = {
            base: 0n,
            backing: 'ram',
            offset: 0,
            length: 0x10000,
            permissions: { read: true, write: true, execute: true },
          };
          cpu.setMappings([
            { ...full, length: 0xb000 },
            { ...full, base: 0xb002n, offset: 0xb002, length: 0x4ffe },
          ]);
          cpu.writePhysicalMemory(0xaffen, hex('7856'));
          const old = initial(CODE, { 0: 0x1122334455667788n, 1: 0n, 3: 0x6ffen }, 0x8d7n);
          cpu.setState(old);
          cpu.setDebugState({ addresses: [0x7000n, 0n, 0n, 0n], dr6: 0n, dr7: 0x30401n });
          const clock = cpu.readTimeState().tsc;
          const pf = await cpu.run({ budget: 1n });
          exit(pf, 'page-fault', 1n, 0n, old);
          equal(pf.pageFault?.address, 0x7000n, 'missing second data page');
          equal(pf.pageFault?.errorCode, 0, 'supervisor read');
          cpu.writePhysicalMemory(0x201cn, little(0xb067n, 4));
          const privateFault = await cpu.run({ budget: 1n });
          exit(privateFault, 'physical-memory-fault', 1n, 0n, old);
          equal(privateFault.physicalMemoryFault?.phase, 'final-access', 'missing second backing');
          equal(
            privateFault.physicalMemoryFault?.failingPhysicalAddress,
            0xb000n,
            'first missing physical byte',
          );
          equal(cpu.readPagingState().cr2, 0x7000n, 'private refusal preserves CR2');
          equal(cpu.readDebugState().dr6 & 1n, 0n, 'failed reads did not select a breakpoint');
          cpu.setMappings([full]);
          cpu.writePhysicalMemory(0xb000n, hex('3412'));
          const done = await cpu.run({ budget: 1n });
          equal(done.reason, 'debug-exception', 'now the second-page watchpoint matches');
          equal(done.retired, 1n, 'one completed read');
          equal(done.state.rip, CODE + 5n, 'trap resumes after BEXTR');
          equal(done.state.registers[0], 0x1122334400000000n, 'zero-length result published');
          equal(done.state.flags & 0x841n, 0x40n, 'zero result published before trap');
          equal(cpu.readTimeState().tsc - clock, 1n, 'refusals did not retire');
          cpu.setDebugState({ addresses: [0n, 0n, 0n, 0n], dr7: 0x400n, dr6: 0n });
          cpu.setState(initial(CODE, { 0: 0n, 1: 0x2000n, 3: 0x6ffen }));
          equal(
            (await cpu.run({ budget: 1n })).state.registers[0],
            0x12345678n,
            'repaired data spans both pages',
          );
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'BMI1 respects natural alignment and stack segment bounds before destination or flag publication',
      async run() {
        const code = 'c4e270f213'; // ANDN EDX,ECX,[EBX]
        const f = aligned(compiler, code);
        await usingPhysical({ compiler, createCpu }, f, async (cpu) => {
          const before = userState({ 1: 0xffffffffn, 3: 0x6001n }); // mathematically always zero, still reads
          const result = await runOne(cpu, before);
          exit(result, 'alignment-check', 1n, 0n, before);
          equal(result.alignmentCheck?.address, 0x6001n, 'unaligned user data');
          equal(result.alignmentCheck?.width, 4, 'dword alignment');
        });
        const ctx = context('protected');
        ctx.segments.ss.limit = 0x5002;
        await using(
          compiler,
          'c4e268f34d00',
          ctx,
          async (cpu) => {
            const before = initial(CODE, { 2: 0xfeedn, 5: 0x5000n }, 0x8d7n);
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            exit(result, 'segment-fault', 1n, 0n, before);
            equal(result.segmentFault?.segment, 'ss', 'EBP chooses SS');
            equal(result.segmentFault?.cause, 'limit', 'dword straddles segment limit');
          },
          { data: [{ base: 0x5000n, bytes: 'ffffffff' }] },
        );
      },
    },
    {
      name: 'BMI1 addresses use long SIB extension, RIP relative, or 16-bit legacy offsets with a dword operand',
      async run() {
        await createFixture(compiler)(
          'c402b0f26c5403',
          async (cpu) => {
            cpu.setState(initial(CODE, { 9: 0xff00ff00ff00ff00n, 10: 3n, 12: DATA - 9n }));
            const result = await cpu.run({ budget: 1n });
            equal(result.reason, 'budget', 'ANDN R13,R9,[R12+R10*2+3]');
            equal(
              result.state.registers[13],
              0x0033005500770099n,
              'both extended address registers contribute',
            );
          },
          { ramBase: DATA, ram: '9988776655443322' },
        );
        await using(
          compiler,
          'c4e2f0f70500010000',
          LONG,
          async (cpu) => {
            cpu.setState(initial(CODE, { 1: 0x0838n }));
            equal(
              (await cpu.run({ budget: 1n })).state.registers[0],
              0x81n,
              'RIP relative high-bit slice',
            );
          },
          { data: [{ base: CODE + 0x109n, bytes: 'ffeeddccbbaa9981' }] },
        );
        for (const mode of ['protected', 'compatibility'] as const) {
          const ctx = context(mode, false);
          ctx.segments.fs.base = 0x1000n;
          await using(
            compiler,
            '64c4e2f8f318',
            ctx,
            async (cpu) => {
              cpu.setState(initial(CODE, { 0: 0x11223344aaaabbbbn, 3: 0x85000n, 6: 0x40n }));
              const result = await cpu.run({ budget: 1n });
              equal(result.reason, 'budget', 'FS:[BX+SI]');
              equal(
                result.state.registers[0],
                0x1122334400020000n,
                'W=1 outside long64 still selects dword',
              );
            },
            { data: [{ base: 0x6040n, bytes: '00000280' }] },
          );
        }
      },
    },
    {
      name: 'BMI1 executes on general-purpose registers with both EM and TS set and OSXSAVE disabled',
      async run() {
        const code = 'c4e270f2c2c4e270f7c2c4e278f3dac4e278f3d2c4e278f3caf30fbcc2';
        const cpu = await createCpu(legacyOptions(compiler, false, code));
        try {
          const bank = cpu.readPagingState();
          cpu.setPagingState({ ...bank, cr0: bank.cr0 | 0xcn, cr4: bank.cr4 & ~0x40600n });
          const numeric = preserveNumeric(cpu);
          cpu.setState(initial(CODE, { 0: 0n, 1: 0x0800n, 2: 0x1040n }));
          const result = await cpu.run({ budget: 6n });
          equal(result.reason, 'budget', 'six scalar instructions');
          equal(result.retired, 6n, 'complete BMI1 family independent of numeric admission');
          equal(result.state.rip, CODE + BigInt(code.length / 2), 'exact end');
          equal(result.state.registers[0], 6n, 'TZCNT of 0x1040');
          numeric();
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'BMI1 recognizes forbidden prefixes and execution modes while preserving adjacent private decode frontiers',
      async run() {
        for (const code of [
          '66c4e278f3c9',
          'f0c4e278f3c9',
          '48c4e278f3c9',
          'f3c4e278f3c9',
          'f2c4e278f3c9',
          'c4e27cf3c9',
          // The address is unbacked. A REX anywhere before an identified VEX BMI1 still selects #UD.
          '4867c4e278f30b',
          '6748c4e278f30b',
          '4864c4e278f30b',
          '6448c4e278f30b',
        ]) {
          await using(compiler, code, LONG, async (cpu) => {
            const before = initial();
            cpu.setState(before);
            const report = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
            equal(report.reason, 'event-budget', 'forbidden encoding selects #UD');
            check(report.state.event.kind === 'pending', 'staged illegal opcode');
            equal(report.state.event.exception.vector, 6, '#UD before operand');
            equal(report.retired, 0n, 'invalid VEX form');
          });
        }
        await using(
          compiler,
          '67c4e278f30b',
          LONG,
          async (cpu) => {
            cpu.setState(initial(CODE, { 3: 0xfedcba9800006000n }));
            const result = await cpu.run({ budget: 1n });
            equal(result.reason, 'budget', '67 BLSR EAX,[EBX] without REX');
            equal(
              result.state.registers[0],
              0x80000000n,
              '32-bit address discards high bits of RBX',
            );
          },
          { data: [{ base: 0x6000n, bytes: '01000080' }] },
        );
        for (const code of ['c4e27af7c1', 'c4e278f3c1', 'c5f858c0', 'c4e379f0c101'])
          await using(compiler, code, LONG, async (cpu) => {
            const before = initial();
            cpu.setState(before);
            exit(
              await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } }),
              'unsupported',
              1n,
              0n,
              before,
            );
          });
        for (const mode of ['real', 'virtual8086'] as const) {
          const ctx = context(mode, false);
          await using(compiler, 'c4e278f3c9', ctx, async (cpu) => {
            const r = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
            check(
              r.state.event.kind === 'pending',
              'VEX forbidden in 16-bit execution environment',
            );
            equal(r.state.event.exception.vector, 6, '#UD for VEX BMI1');
          });
          await using(compiler, 'f30fbcc1', ctx, async (cpu) => {
            cpu.setState(initial(CODE, { 0: 0x1122334455667788n, 1: 0n }));
            const r = await cpu.run({ budget: 1n });
            equal(r.reason, 'budget', 'no VEX restriction on TZCNT');
            equal(r.state.registers[0], 0x1122334455660010n, '16-bit zero count');
          });
        }
        await using(
          compiler,
          'c4060050',
          context('real', false),
          async (cpu) => {
            cpu.setState(initial(CODE, { 0: 0n }));
            const result = await cpu.run({ budget: 1n });
            equal(result.reason, 'budget', 'legacy C4 /r loads ES:AX');
            equal(result.state.registers[0], 0x5678n, 'LES m16:16 offset');
            equal(
              cpu.readContext().segments.es.selector,
              0x1234,
              'same second byte is the LES ModR/M',
            );
          },
          { data: [{ base: 0x5000n, bytes: '78563412' }] },
        );
      },
    },
  ];
}
