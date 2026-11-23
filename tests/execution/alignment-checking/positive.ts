import { stack, ONE } from '../numeric/x87/fixture.js';
import { banks } from '../numeric/simd/fixture.js';
import {
  createCpu,
  fixture,
  using,
  state,
  runOne,
  scalar,
  memory,
  same,
  equal,
  hex,
  put,
  preserved,
  clock,
  type Scenario,
} from './fixture.js';

export function positiveScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'alignment checking FRSTOR image granule never rechecks its internal ten-byte register slots',
      async run() {
        for (const small of [false, true]) {
          const code = small ? '66dd23' : 'dd23',
            at = small ? 0x6002n : 0x6004n;
          const environment = small
            ? '7f03002800000000000000000000'
            : '7f03a5a5' +
              '0028a5a5' +
              '0000a5a5' +
              '00000000' +
              '00000000' +
              '00000000' +
              '0000a5a5';
          equal(environment.length / 2, small ? 14 : 28, 'independent environment literal width');
          const image = environment + '0000000000000080ff3f'.repeat(8),
            f = fixture(compiler, code);
          f.bytes.set(hex(image), 0xa000 + Number(at - 0x6000n));
          await using(side, f, async (cpu) => {
            const before = state({ 3: at }),
              numeric = stack({}, 3),
              expected = structuredClone(numeric);
            cpu.setX87State(numeric);
            const result = await runOne(cpu, before);
            equal(result.reason, 'budget', 'full image checks only format base granule');
            scalar(result, before, 1n, code.length / 2);
            expected.registers.fill(ONE);
            expected.controlWord = 0x37f;
            expected.statusWord = 0x2800;
            expected.tagWord = 0;
            expected.instructionPointer = 0n;
            expected.dataPointer = 0n;
            expected.opcode = small ? 0x345 : 0;
            same(
              cpu.readX87State(),
              expected,
              'all physical registers restore independently of per-slot address residue',
            );
            memory(cpu, f.bytes, 'FRSTOR image read only');
          });
        }
      },
    },
    {
      name: 'alignment checking aligned FX image at16mod512 never gains a natural512 check',
      async run() {
        const f = fixture(compiler, '0fae030fae0b');
        await using(side, f, async (cpu) => {
          const before = state({ 3: 0x6010n }),
            numeric = stack({ 0: ONE }, 5),
            vector = banks(),
            image = new Uint8Array(512).fill(0xa5),
            wanted = f.bytes.slice();
          cpu.setX87State(numeric);
          cpu.setXmmState(vector);
          cpu.setMxcsr(0x1f80);
          put(image, 0, 0x37fn, 2);
          put(image, 2, 0x2800n, 2);
          put(image, 4, 0x20n, 1);
          put(image, 6, 0x345n, 2);
          put(image, 8, 0x9999aaaan, 4);
          put(image, 12, 0n, 2);
          put(image, 16, 0x33334444n, 4);
          put(image, 20, 0n, 2);
          put(image, 24, 0x1f80n, 4);
          put(image, 28, 0xffffn, 4);
          for (let i = 0; i < 8; i++) {
            put(image, 32 + i * 16, numeric.registers[(5 + i) & 7]!, 10);
            put(image, 160 + i * 16, vector[i]!, 16);
          }
          wanted.set(image, 0xa010);
          const unchanged = preserved(cpu),
            result = await runOne(cpu, before);
          equal(result.reason, 'budget', 'mandatory16-aligned image admitted');
          equal(result.alignmentCheck, undefined, 'no false512 divisor');
          scalar(result, before, 1n, 3);
          unchanged();
          memory(cpu, wanted, 'exact selected FX fields and reserved holes');

          const changedNumeric = stack({ 0: 0x40008000000000000000n }, 1, 0x27f),
            changedVector = vector.map((value) => value ^ 0xffffffffffffffffffffffffffffffffn);
          changedNumeric.instructionPointer = 0x123456789abcdeffn;
          changedNumeric.dataPointer = 0xfedcba9876543210n;
          changedNumeric.opcode = 0x678;
          cpu.setX87State(changedNumeric);
          cpu.setXmmState(changedVector);
          cpu.setMxcsr(0xffc5);
          const restored = await cpu.run({ budget: 1n, repeatBudget: 7n });
          equal(restored.reason, 'budget', 'aligned FXRSTOR uses its exempt full-image read');
          equal(
            restored.alignmentCheck,
            undefined,
            'restore gains no natural512 or internal-slot check',
          );
          scalar(restored, { ...before, rip: 0x1003n }, 1n, 3);
          same(
            cpu.readX87State(),
            { ...numeric, instructionPointer: 0x9999aaaan, dataPointer: 0x33334444n },
            'legacy image restores raw banks and its literal truncated pointers',
          );
          same(
            cpu.readXmmState(),
            [...vector.slice(0, 8), ...changedVector.slice(8)],
            'legacy FXRSTOR restores XMM0..7 and preserves changed XMM8..15',
          );
          equal(cpu.readMxcsr(), 0x1f80, 'literal saved MXCSR restored');
          memory(cpu, wanted, 'FXRSTOR reads the independently validated image without writing it');
          clock(cpu, 102n);
        });
      },
    },
    {
      name: 'alignment checking m80 aligns to eight rather than ten and preserves exact load store-pop state',
      async run() {
        for (const store of [false, true]) {
          const f = fixture(compiler, store ? 'db3b' : 'db2b');
          f.bytes.set(hex(store ? 'a5'.repeat(10) : '0000000000000080ff3f'), 0xa008);
          await using(side, f, async (cpu) => {
            const before = state({ 3: 0x6008n }),
              numeric = store ? stack({ 0: ONE }) : stack(),
              expected = structuredClone(numeric),
              wanted = f.bytes.slice();
            cpu.setX87State(numeric);
            const result = await runOne(cpu, before);
            equal(result.reason, 'budget', 'eight-aligned ten-byte value admitted');
            equal(result.alignmentCheck, undefined, 'no width10 mask');
            scalar(result, before, 1n, 2);
            if (store) {
              expected.statusWord = 0x800;
              expected.tagWord = 0xffff;
              wanted.set(hex('0000000000000080ff3f'), 0xa008);
            } else {
              expected.registers[7] = ONE;
              expected.statusWord = 0x3800;
              expected.tagWord = 0x3fff;
            }
            expected.instructionPointer = 0x1000n;
            expected.dataPointer = 0x6008n;
            same(cpu.readX87State(), expected, 'literal raw x87 transition');
            memory(cpu, wanted, 'exact m80 payload and guards');
          });
        }
      },
    },
    {
      name: 'alignment checking environment14 and28 use format granules without internal field rechecks',
      async run() {
        for (const small of [false, true]) {
          const code = small ? '66d923' : 'd923',
            at = small ? 0x6002n : 0x6004n;
          const image = small
            ? '7f0200a818e7efcdffff1032ffff'
            : '7f02a5a500a8a5a518e7a5a5efcdab89adde56f410325476efbea5a5';
          const f = fixture(compiler, code);
          f.bytes.set(hex(image), 0xa000 + Number(at - 0x6000n));
          await using(side, f, async (cpu) => {
            const before = state({ 3: at }),
              numeric = stack({}, 3),
              expected = structuredClone(numeric);
            // These independently authored raw banks are the source of the
            // loaded tag normalization; arbitrary prior values do not mean E3A1.
            numeric.registers = [
              0n,
              ONE,
              0x7fff8000000000000022n,
              0x3fff0000000000000001n,
              0x80000000000000000000n,
              0x40008000000000000000n,
              1n,
              0x12340000000000000007n,
            ];
            expected.registers = [...numeric.registers];
            cpu.setX87State(numeric);
            const result = await runOne(cpu, before);
            equal(result.reason, 'budget', 'format-aligned environment admitted');
            scalar(result, before, 1n, code.length / 2);
            expected.controlWord = 0x27f;
            expected.statusWord = 0x2800;
            expected.tagWord = 0xe3a1;
            expected.instructionPointer = small ? 0xcdefn : 0x89abcdefn;
            expected.dataPointer = small ? 0x3210n : 0x76543210n;
            expected.opcode = small ? 0x345 : 0x456;
            same(cpu.readX87State(), expected, 'literal environment fields');
            memory(cpu, f.bytes, 'environment load never writes image');
          });
        }
      },
    },
    {
      name: 'alignment checking MASKMOVDQU zero mask keeps nominal A admission without natural AC or dirty publication',
      async run() {
        const f = fixture(compiler, '660ff7c1');
        f.leaf(0x6000, 0xa007n);
        await using(side, f, async (cpu) => {
          const before = state({ 7: 0x6001n }),
            vector = banks(),
            wanted = f.bytes.slice();
          vector[1] = 0n;
          cpu.setXmmState(vector);
          const unchanged = preserved(cpu),
            result = await runOne(cpu, before);
          equal(result.reason, 'budget', 'unaligned masked128 zero mask admitted');
          equal(result.alignmentCheck, undefined, 'no natural128 check');
          scalar(result, before, 1n, 4);
          put(wanted, f.pt + 6 * f.width, 0xa027n, f.width);
          memory(cpu, wanted, 'nominal A update but no D or selected payload');
          unchanged();
        });
      },
    },
  ];
}
