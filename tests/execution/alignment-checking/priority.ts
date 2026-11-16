import { stack, ONE } from '../x87/fixture.js';
import { banks } from '../simd/fixture.js';
import {
  AC,
  PC,
  createCpu,
  fixture,
  families,
  using,
  state,
  runOne,
  scalar,
  checkFault,
  memory,
  same,
  equal,
  put,
  preserved,
  type Scenario,
} from './fixture.js';

export function priorityScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'alignment checking FX mandatory GP precedes absent data and remains independent of AM AC',
      async run() {
        for (const family of families)
          for (const store of [false, true]) {
            const f = fixture(compiler, store ? '0fae03' : '0fae0b', family);
            f.leaf(0x6000, 0n);
            await using(side, f, async (cpu) => {
              for (const am of [false, true])
                for (const ac of [false, true]) {
                  const bank = cpu.readPagingState();
                  cpu.setPagingState({ ...bank, cr0: am ? bank.cr0 | AC : bank.cr0 & ~AC });
                  const before = state({}, 2n | (ac ? AC : 0n)),
                    unchanged = preserved(cpu),
                    result = await runOne(cpu, before);
                  equal(result.reason, 'alignment-fault', 'FX selected mandatory GP');
                  same(
                    result.alignmentFault,
                    {
                      instructionPc: PC,
                      segment: 'ds',
                      offset: 0x6001n,
                      address: 0x6001n,
                      width: 512,
                      access: store ? 'write' : 'read',
                      alignment: 16,
                    },
                    'literal full-image mandatory packet',
                  );
                  equal(result.alignmentCheck, undefined, 'FX GP is not natural AC');
                  equal(result.pageFault, undefined, 'mandatory alignment before absent page');
                  scalar(result, before);
                  unchanged();
                  equal(cpu.readPagingState().cr2, 0xdeadn, 'no data PF');
                  memory(cpu, f.bytes, 'no data walk or image payload');
                }
            });
          }
      },
    },
    {
      name: 'alignment checking byte operands remain admitted under the complete active gate',
      async run() {
        for (const family of families)
          for (const store of [false, true]) {
            const f = fixture(compiler, store ? '8803' : '8a03', family);
            await using(side, f, async (cpu) => {
              const before = state(),
                wanted = f.bytes.slice(),
                result = await runOne(cpu, before);
              equal(result.reason, 'budget', 'byte has no alignment violation');
              equal(result.alignmentCheck, undefined, 'granule1 never AC');
              scalar(result, before, 1n, 2, store ? {} : { 0: 0x11223344cafeba20n });
              if (store) wanted[0xa001] = 0xbe;
              memory(cpu, wanted, 'one exact byte only');
            });
          }
      },
    },
    {
      name: 'alignment checking numeric admission precedes an unaligned absent operand and preserves raw state',
      async run() {
        for (const [code, family] of [
          ['f30f1003', 'simd'],
          ['d903', 'x87'],
          ['0fae03', 'fx-image'],
        ] as const) {
          const f = fixture(compiler, code);
          f.paging.cr0 |= 8n;
          f.leaf(0x6000, 0n);
          await using(side, f, async (cpu) => {
            const before = state(),
              unchanged = preserved(cpu),
              result = await runOne(cpu, before);
            equal(result.reason, 'device-not-available', 'TS NM before data');
            same(
              result.deviceNotAvailable,
              {
                vector: 7,
                instructionPc: PC,
                family,
                controls: { mp: true, em: false, ts: true, osfxsr: true, osxmmexcpt: true },
              },
              'literal NM controls and source family',
            );
            equal(result.alignmentCheck, undefined, 'no AC before numeric admission');
            equal(result.pageFault, undefined, 'no operand page walk');
            scalar(result, before);
            unchanged();
            memory(cpu, f.bytes, 'no data effects before NM');
          });
        }
      },
    },
    {
      name: 'alignment checking waiting MF wins while nonwaiting environment store still reaches AC without candidate masking',
      async run() {
        for (const waiting of [false, true]) {
          const f = fixture(compiler, waiting ? 'd903' : 'd933');
          await using(side, f, async (cpu) => {
            const numeric = stack({ 0: ONE }, 0, 0x37e, 0x8081),
              before = state({ 3: 0x6002n });
            cpu.setX87State(numeric);
            const result = await runOne(cpu, before);
            if (waiting) {
              equal(result.reason, 'x87-exception', 'old deferred MF before operand AC');
              same(
                result.x87Exception,
                {
                  instructionPc: PC,
                  vector: 16,
                  pending: 1,
                  controlWord: 0x37e,
                  statusWord: 0x8081,
                  instructionPointer: numeric.instructionPointer,
                  dataPointer: numeric.dataPointer,
                  opcode: 0x345,
                },
                'literal deferred producer',
              );
              equal(result.alignmentCheck, undefined, 'waiting admission no AC');
            } else checkFault(result, 0x6002n, 28, 'write', 4);
            scalar(result, before);
            same(cpu.readX87State(), numeric, 'neither pop nor FNSTENV mask on refusal');
            memory(cpu, f.bytes, 'no image payload');
          });
        }
      },
    },
    {
      name: 'alignment checking masked128 nominal denied tail wins even with zero selected payload',
      async run() {
        for (const zero of [false, true]) {
          const f = fixture(compiler, '660ff7c1'),
            permissions = { read: true, write: true, execute: true };
          f.leaf(0x6000, 0xa007n);
          f.options.memory!.mappings = [
            { base: 0n, backing: 'ram', offset: 0, length: 0xa010, permissions },
            {
              base: 0xa010n,
              backing: 'ram',
              offset: 0xa010,
              length: 1,
              permissions: { ...permissions, write: false },
            },
            {
              base: 0xa011n,
              backing: 'ram',
              offset: 0xa011,
              length: f.bytes.length - 0xa011,
              permissions,
            },
          ];
          await using(side, f, async (cpu) => {
            const before = state({ 7: 0x6001n }),
              vector = banks(),
              wanted = f.bytes.slice();
            vector[1] = zero ? 0n : 0x80n;
            cpu.setXmmState(vector);
            const unchanged = preserved(cpu),
              result = await runOne(cpu, before);
            equal(result.reason, 'physical-memory-fault', 'nominal tail is not suppressed by mask');
            equal(result.alignmentCheck, undefined, 'no false128 AC');
            same(
              result.physicalMemoryFault,
              {
                instructionPc: PC,
                address: 0x6001n,
                access: 'write',
                physicalAddress: 0xa001n,
                width: 16,
                failingPhysicalAddress: 0xa010n,
                physicalAccess: 'write',
                cause: 'permission',
                phase: 'final-access',
                segment: 'ds',
              },
              'exact nominal masked admission',
            );
            put(wanted, f.pt + 6 * f.width, 0xa027n, f.width);
            scalar(result, before);
            unchanged();
            memory(cpu, wanted, 'no D or selected byte despite A receipt');
          });
        }
      },
    },
    {
      name: 'alignment checking false CMOV and zero count RMW do not erase their actual memory operand',
      async run() {
        for (const [code, write] of [
          ['0f4403', false],
          ['c12300', true],
        ] as const) {
          const f = fixture(compiler, code);
          await using(side, f, async (cpu) => {
            const before = state(),
              result = await runOne(cpu, before);
            checkFault(result, 0x6001n, 4, write ? 'write' : 'read', 4);
            scalar(result, before);
            memory(cpu, f.bytes, 'no flags or payload for refused no-result operation');
          });
        }
      },
    },
  ];
}
