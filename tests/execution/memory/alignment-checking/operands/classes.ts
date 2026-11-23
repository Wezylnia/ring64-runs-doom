import { stack, ONE } from '../../../numeric/x87/fixture.js';
import { banks } from '../../../numeric/simd/fixture.js';
import {
  AC,
  createCpu,
  fixture,
  families,
  using,
  state,
  runOne,
  scalar,
  checkFault,
  noOtherFault,
  clock,
  memory,
  preserved,
  same,
  equal,
  hex,
  type Scenario,
} from '../fixture.js';

type Packet = {
  name: string;
  code: string;
  at: bigint;
  width: number;
  granule: number;
  write?: boolean;
  field?: bigint;
  di?: boolean;
  si?: boolean;
  long?: boolean;
};
export const packets: Packet[] = [
  { name: 'word', code: '668b03', at: 0x6001n, width: 2, granule: 2 },
  { name: 'dword RMW', code: '0103', at: 0x6001n, width: 4, granule: 4, write: true },
  { name: 'locked RMW', code: 'f00103', at: 0x6001n, width: 4, granule: 4, write: true },
  { name: 'CMPXCHG mismatch', code: '0fb10b', at: 0x6001n, width: 4, granule: 4, write: true },
  {
    name: 'CMPXCHG8B outside long64',
    code: '0fc70e',
    at: 0x6004n,
    width: 8,
    granule: 8,
    write: true,
    si: true,
  },
  {
    name: 'locked CMPXCHG8B',
    code: 'f00fc70e',
    at: 0x6004n,
    width: 8,
    granule: 8,
    write: true,
    si: true,
  },
  { name: 'MMX MOVD', code: '0f6e03', at: 0x6002n, width: 4, granule: 4 },
  { name: 'MMX PUNPCKLBW', code: '0f6003', at: 0x6002n, width: 4, granule: 4 },
  { name: 'MMX PUNPCKHBW', code: '0f6803', at: 0x6004n, width: 8, granule: 8 },
  { name: 'MMX PALIGNR', code: '0f3a0f03ff', at: 0x6004n, width: 8, granule: 8 },
  { name: 'MMX MOVQ', code: '0f6f03', at: 0x6004n, width: 8, granule: 8 },
  { name: 'MMX bridge CVTPI2PS', code: '0f2a03', at: 0x6004n, width: 8, granule: 8 },
  { name: 'MMX bridge CVTPS2PI', code: '0f2d03', at: 0x6004n, width: 8, granule: 8 },
  { name: 'MOVNTI', code: '0fc303', at: 0x6002n, width: 4, granule: 4, write: true },
  { name: 'MOVNTQ', code: '0fe703', at: 0x6004n, width: 8, granule: 8, write: true },
  { name: 'MOVSS', code: 'f30f1003', at: 0x6002n, width: 4, granule: 4 },
  { name: 'MOVSD', code: 'f20f1003', at: 0x6004n, width: 8, granule: 8 },
  { name: 'MOVDDUP', code: 'f20f1203', at: 0x6004n, width: 8, granule: 8 },
  { name: 'FISTTP word', code: 'df0b', at: 0x6001n, width: 2, granule: 2, write: true },
  { name: 'FISTTP dword', code: 'db0b', at: 0x6002n, width: 4, granule: 4, write: true },
  { name: 'FISTTP qword', code: 'dd0b', at: 0x6004n, width: 8, granule: 8, write: true },
  { name: 'x87 m32', code: 'd903', at: 0x6002n, width: 4, granule: 4 },
  { name: 'x87 m64', code: 'dd03', at: 0x6004n, width: 8, granule: 8 },
  { name: 'x87 m80 load', code: 'db2b', at: 0x6004n, width: 10, granule: 8 },
  { name: 'x87 m80 store-pop', code: 'db3b', at: 0x6004n, width: 10, granule: 8, write: true },
  { name: 'packed BCD selected profile', code: 'df23', at: 0x6004n, width: 10, granule: 8 },
  { name: 'FNSTCW', code: 'd93b', at: 0x6001n, width: 2, granule: 2, write: true },
  { name: 'environment28', code: 'd923', at: 0x6002n, width: 28, granule: 4 },
  { name: 'environment14', code: '66d923', at: 0x6001n, width: 14, granule: 2 },
  { name: 'FNSAVE108', code: 'dd33', at: 0x6002n, width: 108, granule: 4, write: true },
  { name: 'FNSAVE94', code: '66dd33', at: 0x6001n, width: 94, granule: 2, write: true },
  { name: 'LDMXCSR before reserved payload', code: '0fae13', at: 0x6002n, width: 4, granule: 4 },
  { name: 'STMXCSR', code: '0fae1b', at: 0x6002n, width: 4, granule: 4, write: true },
  {
    name: 'SGDT legacy base field',
    code: '0f0103',
    at: 0x6000n,
    field: 0x6002n,
    width: 4,
    granule: 4,
    write: true,
  },
  {
    name: 'SIDT legacy base field',
    code: '0f010b',
    at: 0x6000n,
    field: 0x6002n,
    width: 4,
    granule: 4,
    write: true,
  },
  {
    name: 'MASKMOVQ including zero mask',
    code: '0ff7c1',
    at: 0x6001n,
    width: 8,
    granule: 8,
    write: true,
    di: true,
  },
  { name: 'integer qword', code: '488b03', at: 0x6004n, width: 8, granule: 8, long: true },
];

export function classScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'alignment checking semantic operand granules suppress all candidate numeric and RMW state',
      async run() {
        for (const p of packets)
          for (const family of p.long
            ? (['ia32e', 'fixed'] as const)
            : (['non-pae', 'pae'] as const))
            for (const zeroMask of p.di ? [false, true] : [false]) {
              const f = fixture(compiler, p.code, family);
              await using(side, f, async (cpu) => {
                const before = state({
                  3: p.at,
                  6: p.si ? p.at : 0x6001n,
                  7: p.di ? p.at : 0x7001n,
                });
                const numeric = stack({ 0: ONE }, 4);
                numeric.registers[1] = zeroMask ? 0n : 0x8080808080808080n;
                cpu.setX87State(numeric);
                cpu.setXmmState(banks());
                const unchanged = preserved(cpu),
                  context = cpu.readContext(),
                  control = cpu.readPagingState();
                const result = await runOne(cpu, before);
                checkFault(result, p.field ?? p.at, p.width, p.write ? 'write' : 'read', p.granule);
                scalar(result, before);
                unchanged();
                memory(cpu, f.bytes, p.name);
                same(cpu.readContext(), context, 'classification context preservation');
                same(cpu.readPagingState(), control, 'classification CR2/control preservation');
                clock(cpu);
              });
            }
      },
    },
    {
      name: 'alignment checking admitted unaligned128 loads differ from mandatory16 GP under every gate arm',
      async run() {
        for (const family of families)
          for (const code of ['0f1003', '660f1003', 'f30f6f03', 'f20ff003']) {
            const f = fixture(compiler, code, family);
            await using(side, f, async (cpu) => {
              const vector = banks();
              cpu.setXmmState(vector);
              const before = state();
              const result = await runOne(cpu, before);
              equal(result.reason, 'budget', 'selected unaligned128 policy');
              equal(result.alignmentCheck, undefined, 'no false AC for unaligned128');
              noOtherFault(result);
              scalar(result, before, 1n, code.length / 2);
              vector[0] = 0xa500f0e0d0c0b0a09080706050403020n;
              same(cpu.readXmmState(), vector, 'exact sixteen byte load');
              memory(cpu, f.bytes, 'unaligned128 load no writes');
              clock(cpu, 101n);
            });
          }
        for (const code of [
          '0f2803',
          '660f2803',
          '660f6f03',
          '660f6003',
          '660f2d03',
          '660f3a0f03ff',
          'f30f1203',
          'f30f1603',
          'f20f7c03',
          '660fd003',
        ])
          for (const ac of [false, true]) {
            const f = fixture(compiler, code);
            await using(side, f, async (cpu) => {
              const before = state({}, 2n | (ac ? AC : 0n)),
                unchanged = preserved(cpu);
              const result = await runOne(cpu, before);
              equal(result.reason, 'alignment-fault', 'mandatory GP stays distinct');
              same(
                result.alignmentFault,
                {
                  instructionPc: 0x1000n,
                  segment: 'ds',
                  offset: 0x6001n,
                  address: 0x6001n,
                  width: 16,
                  access: 'read',
                  alignment: 16,
                },
                'existing mandatory diagnostic',
              );
              equal(result.alignmentCheck, undefined, 'no natural packet for GP');
              scalar(result, before);
              unchanged();
              memory(cpu, f.bytes, 'mandatory GP no payload');
            });
          }
      },
    },
    {
      name: 'alignment checking system records use ordered field geometry and whole nominal admission',
      async run() {
        for (const family of families) {
          const long = family === 'ia32e' || family === 'fixed';
          const f = fixture(compiler, '0f0103', family),
            at = long ? 0x6006n : 0x6002n;
          await using(side, f, async (cpu) => {
            const before = state({ 3: at }),
              context = cpu.readContext(),
              wanted = f.bytes.slice();
            wanted.set(
              hex(long ? '3f000070000000000000' : '3f0000700000'),
              0xa000 + Number(at - 0x6000n),
            );
            const result = await runOne(cpu, before);
            equal(result.reason, 'budget', 'component-aligned record admitted');
            equal(result.alignmentCheck, undefined, 'record extent is not granule');
            scalar(result, before, 1n, 3);
            memory(cpu, wanted, 'literal SGDT record and guards');
            same(cpu.readContext(), context, 'SGDT does not load table');
          });
        }
      },
    },
  ];
}
