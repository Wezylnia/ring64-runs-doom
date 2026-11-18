import { CODE, createFixture, initial, equal, type Scenario } from '../../../support/fixture.js';
import { same } from '../../far-control/fixture.js';
import { bitmapScenario } from './bitmap.js';

const FORMS = [
  ['andn', 'c44230f2ed', 'c442b0f2ed'],
  ['bextr', 'c44230f7ed', 'c442b0f7ed'],
  ['blsi', 'c4c210f3dd', 'c4c290f3dd'],
  ['blsmsk', 'c4c210f3d5', 'c4c290f3d5'],
  ['blsr', 'c4c210f3cd', 'c4c290f3cd'],
  ['tzcnt', 'f3450fbced', 'f34d0fbced'],
] as const;
type Op = (typeof FORMS)[number][0];

function reference(op: Op, width: number, input: bigint, control: bigint) {
  const source = BigInt.asUintN(width, input);
  const set = Array.from({ length: width }, (_, i) => i).filter(
    (i) => ((source >> BigInt(i)) & 1n) === 1n,
  );
  const first = set[0] ?? width,
    start = Number(control & 255n),
    length = Number((control >> 8n) & 255n);
  const low = (count: number) => (1n << BigInt(count)) - 1n;
  let value: bigint;
  switch (op) {
    case 'andn':
      value = source & ~control;
      break;
    case 'bextr':
      value =
        start >= width ? 0n : (source >> BigInt(start)) & low(Math.min(width - start, length));
      break;
    case 'blsi':
      value = set.length ? 1n << BigInt(first) : 0n;
      break;
    case 'blsmsk':
      value = low(Math.min(width, first + 1));
      break;
    case 'blsr':
      value = set.slice(1).reduce((v, i) => v | (1n << BigInt(i)), 0n);
      break;
    case 'tzcnt':
      value = BigInt(first);
      break;
  }
  const carry =
    op === 'blsi' ? source !== 0n : ['blsr', 'blsmsk', 'tzcnt'].includes(op) && source === 0n;
  const flags =
    BigInt(carry) | (value === 0n ? 64n : 0n) | (((value >> BigInt(width - 1)) & 1n) << 7n);
  const mask = op === 'tzcnt' ? 0x41n : op === 'bextr' ? 0x841n : 0x8c1n;
  return { value, flags, mask };
}

export function bmi1Scenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    bitmapScenario(compiler),
    {
      name: 'BMI1 scalar results, defined flags and input aliases agree with independent bit positions at 32 and 64 bits',
      async run() {
        for (const [op, dword, qword] of FORMS) {
          for (const [width, code] of [
            [32, dword],
            [64, qword],
          ] as const) {
            await createFixture(compiler)(code, async (cpu) => {
              const high = 1n << BigInt(width - 1);
              for (const value of [0n, 1n, 2n, high, 0xffffffffffffffffn, 0xfedcba9876543210n]) {
                const controls =
                  op === 'bextr'
                    ? [
                        0n,
                        0xff00n,
                        0x0100n,
                        0x1000n + BigInt(width - 1),
                        0xff00n + BigInt(width),
                        0xabcd000000000c07n,
                        0xffffn,
                      ]
                    : [0x55aa0f0f55aa0f0fn];
                for (const control of controls) {
                  const input = initial(CODE, { 9: control, 13: value }, 0xed7n);
                  cpu.setState(input);
                  const r = await cpu.run({ budget: 1n });
                  equal(r.reason, 'budget', `${op}${width}`);
                  equal(r.retired, 1n, 'one scalar operation');
                  const expected = reference(op, width, value, control);
                  const registers = [...input.registers];
                  registers[13] = expected.value;
                  same(
                    r.state.registers,
                    registers,
                    'R13 destination aliases old data; high GPRs remain distinct',
                  );
                  equal(
                    r.state.flags & expected.mask,
                    expected.flags & expected.mask,
                    'architecturally defined flags',
                  );
                  equal(r.state.flags & 0x602n, 0x602n, 'direction, interrupt and fixed flags');
                }
              }
            });
          }
        }
      },
    },
  ];
}
