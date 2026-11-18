import type { Cpu } from '../../../host/index.js';
import { LONG, ONE, equal, input, other, seed, stack, using, type Scenario } from './fixture.js';

// complete .text from musl v1.2.5 src/math/x86_64; assemble with clang -target x86_64-linux-musl -c.
// https://git.musl-libc.org/cgit/musl/tree/src/math/x86_64?h=v1.2.5
/*
Copyright © 2005-2020 Rich Felker, et al.

Permission is hereby granted, free of charge, to any person obtaining
a copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be
included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
*/
const atan = 'db6c2408d9e8d9f3c3',
  atan2 = 'db6c2408db6c2418d9f3c3',
  asin = 'db6c2408d9c0d9e8dce1d8c2dec9d9fad9f3c3',
  acos = 'db6c2408d9c0d9e8dce1d8c2dec9d9fad9e1d9c9d9f3c3';
const half = 0x3ffe8000000000000000n,
  sqrtThreeOverTwo = 0x3ffeddb3d742c265539en;
const base = 0x4000n,
  rsp = base + 8n,
  stop = 0x2000n;

// mpfr 4.2.2 raw binary80 down/up endpoints, or the exact nearest result for class and normal-boundary anchors.
const quadrants = `
+zero,+zero 0 0 0
+zero,-zero 0 80000000000000000000 4000c90fdaa22168c235
-zero,+zero 80000000000000000000 0 80000000000000000000
-zero,-zero 80000000000000000000 80000000000000000000 c000c90fdaa22168c235
+zero,+one 0 3fff8000000000000000 0
+zero,-one 0 bfff8000000000000000 4000c90fdaa22168c235
-zero,+one 80000000000000000000 3fff8000000000000000 80000000000000000000
-zero,-one 80000000000000000000 bfff8000000000000000 c000c90fdaa22168c235
+zero,+infinity 0 7fff8000000000000000 0
+zero,-infinity 0 ffff8000000000000000 4000c90fdaa22168c235
-zero,+infinity 80000000000000000000 7fff8000000000000000 80000000000000000000
-zero,-infinity 80000000000000000000 ffff8000000000000000 c000c90fdaa22168c235
+one,+zero 3fff8000000000000000 0 3fffc90fdaa22168c235
+one,-zero 3fff8000000000000000 80000000000000000000 3fffc90fdaa22168c235
-one,+zero bfff8000000000000000 0 bfffc90fdaa22168c235
-one,-zero bfff8000000000000000 80000000000000000000 bfffc90fdaa22168c235
+one,+one 3fff8000000000000000 3fff8000000000000000 3ffec90fdaa22168c235
+one,-one 3fff8000000000000000 bfff8000000000000000 400096cbe3f9990e91a8
-one,+one bfff8000000000000000 3fff8000000000000000 bffec90fdaa22168c235
-one,-one bfff8000000000000000 bfff8000000000000000 c00096cbe3f9990e91a8
+one,+infinity 3fff8000000000000000 7fff8000000000000000 0
+one,-infinity 3fff8000000000000000 ffff8000000000000000 4000c90fdaa22168c235
-one,+infinity bfff8000000000000000 7fff8000000000000000 80000000000000000000
-one,-infinity bfff8000000000000000 ffff8000000000000000 c000c90fdaa22168c235
+infinity,+zero 7fff8000000000000000 0 3fffc90fdaa22168c235
+infinity,-zero 7fff8000000000000000 80000000000000000000 3fffc90fdaa22168c235
-infinity,+zero ffff8000000000000000 0 bfffc90fdaa22168c235
-infinity,-zero ffff8000000000000000 80000000000000000000 bfffc90fdaa22168c235
+infinity,+one 7fff8000000000000000 3fff8000000000000000 3fffc90fdaa22168c235
+infinity,-one 7fff8000000000000000 bfff8000000000000000 3fffc90fdaa22168c235
-infinity,+one ffff8000000000000000 3fff8000000000000000 bfffc90fdaa22168c235
-infinity,-one ffff8000000000000000 bfff8000000000000000 bfffc90fdaa22168c235
+infinity,+infinity 7fff8000000000000000 7fff8000000000000000 3ffec90fdaa22168c235
+infinity,-infinity 7fff8000000000000000 ffff8000000000000000 400096cbe3f9990e91a8
-infinity,+infinity ffff8000000000000000 7fff8000000000000000 bffec90fdaa22168c235
-infinity,-infinity ffff8000000000000000 ffff8000000000000000 c00096cbe3f9990e91a8
half:++ 3ffe8000000000000000 3fff8000000000000000 3ffded63382b0dda7b45 3ffded63382b0dda7b46
half:+- 3ffe8000000000000000 bfff8000000000000000 4000ab63739cbfad72cc 4000ab63739cbfad72cd
half:-+ bffe8000000000000000 3fff8000000000000000 bffded63382b0dda7b46 bffded63382b0dda7b45
half:-- bffe8000000000000000 bfff8000000000000000 c000ab63739cbfad72cd c000ab63739cbfad72cc
below-half:++ 3ffdffffffffffffffff 3fff8000000000000000 3ffded63382b0dda7b44 3ffded63382b0dda7b45
below-half:+- 3ffdffffffffffffffff bfff8000000000000000 4000ab63739cbfad72cc 4000ab63739cbfad72cd
below-half:-+ bffdffffffffffffffff 3fff8000000000000000 bffded63382b0dda7b45 bffded63382b0dda7b44
below-half:-- bffdffffffffffffffff bfff8000000000000000 c000ab63739cbfad72cd c000ab63739cbfad72cc
above-half:++ 3ffe8000000000000001 3fff8000000000000000 3ffded63382b0dda7b47 3ffded63382b0dda7b48
above-half:+- 3ffe8000000000000001 bfff8000000000000000 4000ab63739cbfad72cb 4000ab63739cbfad72cc
above-half:-+ bffe8000000000000001 3fff8000000000000000 bffded63382b0dda7b48 bffded63382b0dda7b47
above-half:-- bffe8000000000000001 bfff8000000000000000 c000ab63739cbfad72cc c000ab63739cbfad72cb
below-one:++ 3ffeffffffffffffffff 3fff8000000000000000 3ffec90fdaa22168c234 3ffec90fdaa22168c235
below-one:+- 3ffeffffffffffffffff bfff8000000000000000 400096cbe3f9990e91a7 400096cbe3f9990e91a8
below-one:-+ bffeffffffffffffffff 3fff8000000000000000 bffec90fdaa22168c235 bffec90fdaa22168c234
below-one:-- bffeffffffffffffffff bfff8000000000000000 c00096cbe3f9990e91a8 c00096cbe3f9990e91a7
above-one:++ 3fff8000000000000001 3fff8000000000000000 3ffec90fdaa22168c235 3ffec90fdaa22168c236
above-one:+- 3fff8000000000000001 bfff8000000000000000 400096cbe3f9990e91a7 400096cbe3f9990e91a8
above-one:-+ bfff8000000000000001 3fff8000000000000000 bffec90fdaa22168c236 bffec90fdaa22168c235
above-one:-- bfff8000000000000001 bfff8000000000000000 c00096cbe3f9990e91a8 c00096cbe3f9990e91a7
minnormal:++ 18000000000000000 3fff8000000000000000 18000000000000000
minnormal:+- 18000000000000000 bfff8000000000000000 4000c90fdaa22168c234 4000c90fdaa22168c235
minnormal:-+ 80018000000000000000 3fff8000000000000000 80018000000000000000
minnormal:-- 80018000000000000000 bfff8000000000000000 c000c90fdaa22168c235 c000c90fdaa22168c234
minsub:++ 1 3fff8000000000000000 0 1
minsub:+- 1 bfff8000000000000000 4000c90fdaa22168c234 4000c90fdaa22168c235
minsub:-+ 80000000000000000001 3fff8000000000000000 80000000000000000001 80000000000000000000
minsub:-- 80000000000000000001 bfff8000000000000000 c000c90fdaa22168c235 c000c90fdaa22168c234
maxsub:++ 7fffffffffffffff 3fff8000000000000000 7ffffffffffffffe 7fffffffffffffff
maxsub:+- 7fffffffffffffff bfff8000000000000000 4000c90fdaa22168c234 4000c90fdaa22168c235
maxsub:-+ 80007fffffffffffffff 3fff8000000000000000 80007fffffffffffffff 80007ffffffffffffffe
maxsub:-- 80007fffffffffffffff bfff8000000000000000 c000c90fdaa22168c235 c000c90fdaa22168c234
minsub-by-max:++ 1 7ffeffffffffffffffff 0 1
minsub-by-max:+- 1 fffeffffffffffffffff 4000c90fdaa22168c234 4000c90fdaa22168c235
minsub-by-max:-+ 80000000000000000001 7ffeffffffffffffffff 80000000000000000001 80000000000000000000
minsub-by-max:-- 80000000000000000001 fffeffffffffffffffff c000c90fdaa22168c235 c000c90fdaa22168c234
minnormal-by-max:++ 18000000000000000 7ffeffffffffffffffff 0 1
minnormal-by-max:+- 18000000000000000 fffeffffffffffffffff 4000c90fdaa22168c234 4000c90fdaa22168c235
minnormal-by-max:-+ 80018000000000000000 7ffeffffffffffffffff 80000000000000000001 80000000000000000000
minnormal-by-max:-- 80018000000000000000 fffeffffffffffffffff c000c90fdaa22168c235 c000c90fdaa22168c234
one-by-minsub:++ 3fff8000000000000000 1 3fffc90fdaa22168c234 3fffc90fdaa22168c235
one-by-minsub:+- 3fff8000000000000000 80000000000000000001 3fffc90fdaa22168c234 3fffc90fdaa22168c235
one-by-minsub:-+ bfff8000000000000000 1 bfffc90fdaa22168c235 bfffc90fdaa22168c234
one-by-minsub:-- bfff8000000000000000 80000000000000000001 bfffc90fdaa22168c235 bfffc90fdaa22168c234
`
  .trim()
  .split('\n');

async function call(
  cpu: Cpu,
  name: string,
  operands: readonly bigint[],
  x: bigint,
  y: bigint,
  before: bigint,
  fpatan: bigint,
  lo: bigint,
  hi = lo,
): Promise<void> {
  const frame = new Uint8Array(64).fill(0xa5);
  const view = new DataView(frame.buffer);
  view.setBigUint64(8, stop, true);
  for (const [i, arg] of operands.entries()) {
    const offset = 16 + i * 16;
    view.setBigUint64(offset, BigInt.asUintN(64, arg), true);
    view.setUint16(offset + 8, Number(arg >> 64n), true);
  }
  cpu.writeMemory(base, frame);
  const caller = input(0x1000n, { 4: rsp }, 2n);
  cpu.setState(caller);
  seed(cpu, stack());
  const loaded = await cpu.run({ budget: 16n, stop: fpatan });
  equal(loaded.reason, 'stop', name + ' reaches FPATAN');
  equal(loaded.retired, before, name + ' preparation retirements');
  const source = cpu.readX87State(),
    top = (source.statusWord >> 11) & 7;
  equal(source.registers[top], x, name + ' x operand at FPATAN');
  equal(source.registers[(top + 1) & 7], y, name + ' y operand at FPATAN');

  const result = await cpu.run({ budget: 3n, stop });
  equal(result.reason, 'stop', name + ' completes FPATAN and RET');
  equal(result.retired, 2n, name + ' retires once for FPATAN and once for RET');
  equal(result.state.rip, stop, 'RET used the saved return address');
  equal(result.state.registers[4], rsp + 8n, 'SysV stack consumed only the return slot');
  for (const i of [3, 5, 12, 13, 14, 15])
    equal(result.state.registers[i], caller.registers[i], 'callee-saved GPR');
  const numeric = cpu.readX87State();
  equal((numeric.statusWord >> 11) & 7, 7, 'one x87 result');
  const actual = numeric.registers[7]!;
  const magnitude = actual & ((1n << 79n) - 1n);
  equal(
    Boolean(magnitude & (1n << 63n)),
    magnitude >> 64n !== 0n,
    'canonical binary80 integer bit',
  );
  equal(
    numeric.tagWord,
    0x3fff | ((magnitude === 0n ? 1 : magnitude >> 64n === 0n ? 2 : 0) << 14),
    'physical return tag',
  );
  equal(actual >> 79n, y >> 79n, name + ' result sign');
  equal(
    (actual >= lo && actual <= hi) || (actual >= hi && actual <= lo),
    true,
    name + ' raw angle ' + actual.toString(16),
  );
  for (const [i, byte] of cpu.readMemory(base, frame.length).entries())
    equal(byte, frame[i], 'stack guards, return slot, argument and padding');
  other(cpu);
}

export function muslFpatanScenarios(compiler: WebAssembly.Module): Scenario[] {
  const setup = { data: [{ base, bytes: '00'.repeat(64) }] };
  return [
    {
      name: 'musl atanl, asinl and acosl return their binary80 angles from original x86-64 bodies',
      async run() {
        for (const [name, code, x, y, before, pc, lo, hi] of [
          [
            'atanl(½)',
            atan,
            ONE,
            half,
            2n,
            0x1006n,
            0x3ffded63382b0dda7b45n,
            0x3ffded63382b0dda7b46n,
          ],
          [
            'asinl(½)',
            asin,
            sqrtThreeOverTwo,
            half,
            7n,
            0x1010n,
            0x3ffe860a91c16b9b2c22n,
            0x3ffe860a91c16b9b2c23n,
          ],
          [
            'acosl(½)',
            acos,
            half,
            sqrtThreeOverTwo,
            9n,
            0x1014n,
            0x3fff860a91c16b9b2c23n,
            0x3fff860a91c16b9b2c24n,
          ],
        ] as const)
          await using(
            compiler,
            code,
            LONG,
            (cpu) => call(cpu, name, [half], x, y, before, pc, lo, hi),
            setup,
          );
      },
    },
    {
      name: 'original musl atan2l completes all eighty binary80 quadrant and range witnesses',
      async run() {
        await using(
          compiler,
          atan2,
          LONG,
          async (cpu) => {
            for (const row of quadrants) {
              const [name, ordinate, abscissa, lower, upper = lower] = row.split(' ');
              const y = BigInt('0x' + ordinate),
                x = BigInt('0x' + abscissa);
              await call(
                cpu,
                name!,
                [y, x],
                x,
                y,
                2n,
                0x1008n,
                BigInt('0x' + lower),
                BigInt('0x' + upper),
              );
            }
          },
          setup,
        );
      },
    },
  ];
}
