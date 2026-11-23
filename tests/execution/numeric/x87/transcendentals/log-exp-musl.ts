import { equal, LONG, ONE, input, other, seed, stack, using, type Scenario } from '../fixture.js';

// Complete .text from musl v1.2.5 src/math/x86_64/{exp2l,log2l,logl,log10l,log1pl}.s.
// Reproduce with clang -target x86_64-linux-musl -c FILE.s, then extract .text.
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
const exp = [
  'db6c2408d9eadec9c74424fc000082c2d94424fcdfe9d9e87205ddd9d9e0c3d9',
  'c1d9e1dfe9ddd87703d9f0c350e80a00000058d9e8dee9c3db6c2408d9c04883',
  'ec10db3c24668b4424086625ff7f663d0c40724e663d0e407346d91424813c2400',
  'f87fc67223c704240000005fd90424d9c1d8e1dec1dfe9740fc7042401000000d9',
  '0424d8f1d91c24d9e8d9c1d9fcd9cad8e2d9f0dec1d9fdddd94883c410c331c0',
  '663dbf3fd9e872eddb3c24db542408db442408dee981442408ff3f0000d9f0d9e8',
  'dec1db2c24dec94883c410c3',
].join('');
const log1p = '8b44240ed9ed25ffffff7fdb6c24083d0094fd3f7703d9f9c3d9e8dec1d9f1c3';
const half = 0x3ffe8000000000000000n;
// For these positive normal results remove the explicit integer bit to count adjacent values across 1.
const rank = (v: bigint) => ((v >> 64n) << 63n) | (v & ((1n << 63n) - 1n));

export function muslLogExpScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'six musl x86-64 long double functions return through the SysV stack with binary80 precision',
    async run() {
      // Reference literals are MPFR 4.2.2, rounded to 64 bits at the function result.
      // These library paths include rounded x87 constants and multiple arithmetic instructions.
      for (const [name, code, offset, argument, expected, allowance] of [
        ['exp2l(0)', exp, 0x38, 0n, ONE, 0n],
        ['exp2l(1/2)', exp, 0x38, half, 0x3fffb504f333f9de6484n, 2n],
        ['exp2l(-8192.5)', exp, 0x38, 0xc00c8002000000000000n, 0x1ffeb504f333f9de6484n, 2n],
        ['expm1l(1/2)', exp, 0, half, 0x3ffea61298e1e069bc97n, 2n],
        ['expm1l(3/2)', exp, 0, 0x3fffc000000000000000n, 0x4000ded3fe64fc54157en, 3n],
        ['log2l(2)', 'd9e8db6c2408d9f1c3', 0, 0x40008000000000000000n, ONE, 1n],
        [
          'logl(3/2)',
          'd9eddb6c2408d9f1c3',
          0,
          0x3fffc000000000000000n,
          0x3ffdcf991f65fcc25f96n,
          2n,
        ],
        ['log10l(10)', 'd9ecdb6c2408d9f1c3', 0, 0x4002a000000000000000n, ONE, 2n],
        ['log1pl(1/8)', log1p, 0, 0x3ffc8000000000000000n, 0x3ffbf1383b7157972f4fn, 2n],
        ['log1pl(1/2)', log1p, 0, half, 0x3ffdcf991f65fcc25f96n, 2n],
      ] as const) {
        const base = 0x4000n,
          stop = 0x2000n,
          rsp = 0x4188n;
        const image = new Uint8Array(0x200).fill(0xa5);
        const frame = new DataView(image.buffer);
        frame.setBigUint64(0x188, stop, true);
        frame.setBigUint64(0x190, BigInt.asUintN(64, argument), true);
        frame.setUint16(0x198, Number(argument >> 64n), true);
        const data = Array.from(image, (b) => b.toString(16).padStart(2, '0')).join('');
        await using(
          compiler,
          code,
          LONG,
          async (cpu) => {
            const caller = input(0x1000n + BigInt(offset), { 4: rsp }, 2n);
            cpu.setState(caller);
            seed(cpu, stack());
            const r = await cpu.run({ budget: 160n, stop });
            equal(r.reason, 'stop', name);
            equal(r.state.rip, stop, 'RET used the caller return address');
            equal(r.state.registers[4], rsp + 8n, 'SysV stack consumed the return slot');
            for (const i of [3, 5, 12, 13, 14, 15])
              equal(r.state.registers[i], caller.registers[i], 'callee-saved GPR');
            const numeric = cpu.readX87State();
            equal((numeric.statusWord >> 11) & 7, 7, 'one x87 return value');
            equal(numeric.tagWord, 0x3fff, 'remaining seven physical registers empty');
            const actual = numeric.registers[7]!;
            equal(actual >> 79n, 0n, 'positive result');
            equal(actual & (1n << 63n), 1n << 63n, 'explicit integer bit of normal result');
            const delta = rank(actual) - rank(expected);
            equal(
              delta >= -allowance && delta <= allowance,
              true,
              name + ' result ' + actual.toString(16),
            );
            const guard = cpu.readMemory(base, image.length);
            for (let i = 0; i < image.length; i++)
              if (i < 0x108 || i >= 0x188)
                equal(guard[i], image[i], 'stack guards, argument and padding');
            other(cpu);
          },
          { data: [{ base, bytes: data }] },
        );
      }
    },
  };
}
