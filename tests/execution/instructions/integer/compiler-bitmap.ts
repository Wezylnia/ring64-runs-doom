import { CODE, createFixture, equal, initial, type Scenario } from '../../../support/fixture.js';
import { BANK, PC, little, same, usingPaged } from '../../memory/paging/fixture.js';

// Complete ELF .text, clang 21.0.0 -target x86_64-unknown-linux-gnu -O2
// -ffreestanding -fno-vectorize -fno-slp-vectorize, with -mno-bmi or -mbmi:
// unsigned extract(unsigned long long w, unsigned *out) {
//   unsigned n = 0; while (w) { out[n++] = __builtin_ctzll(w); w &= w - 1; } return n;
// }
const COMPILER_TEXT = [
  '31c04885ff741e660f1f840000000000f3480fbccf89c2ffc0890c96488d4fff4821cf75ebc3',
  '31c04885ff741c660f1f840000000000f3480fbccf89c2ffc0890c96c4e2c0f3cf75edc3',
] as const;

async function hasBmi1(compiler: WebAssembly.Module): Promise<boolean> {
  let advertised = false;
  await createFixture(compiler)('0fa2', async (cpu) => {
    cpu.setState(initial(CODE, { 0: 7n, 1: 0n }));
    const result = await cpu.run({ budget: 1n });
    equal(result.retired, 1n, 'CPUID leaf 7');
    advertised = (result.state.registers[3]! & 8n) !== 0n;
  });
  return advertised;
}

export function compilerBitmapScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'unmodified compiler bitmap text executes padding, enumerates sparse or dense words, and returns through the guest stack',
      async run() {
        const bmi1 = await hasBmi1(compiler);
        const output = PC + 0x1010n,
          sp = PC + 0x1300n,
          stop = PC + 0x3000n;
        for (const [variant, code] of COMPILER_TEXT.entries()) {
          for (const mask of [0n, 1n << 63n, 0x8000000000000031n, 0xffffffffffffffffn]) {
            const expected = Array.from({ length: 64 }, (_, index) => index).filter(
              (index) => (mask & (1n << BigInt(index))) !== 0n,
            );
            const page = new Uint8Array(0x1000).fill(0xa5);
            page.set(little([stop]), 0x300);
            await usingPaged(
              compiler,
              {
                code,
                bank: BANK,
                words: [[0x4008n, 0xa063n]],
                spans: [{ physical: 0xa000n, bytes: page }],
              },
              async (cpu) => {
                cpu.setState(initial(PC, { 4: sp, 6: output, 7: mask }, 2n));
                const tsc = cpu.readTimeState().tsc;
                const result = await cpu.run({ budget: 512n, stop });
                const missingBlsr = variant === 1 && !bmi1 && mask !== 0n;
                const written = missingBlsr ? expected.slice(0, 1) : expected;
                const retired = missingBlsr
                  ? 8n
                  : mask === 0n
                    ? 4n
                    : 5n + BigInt(expected.length * (variant === 1 ? 6 : 7));
                equal(
                  result.reason,
                  missingBlsr ? 'unsupported' : 'stop',
                  'compiler function boundary',
                );
                equal(
                  result.state.rip,
                  missingBlsr ? PC + 0x1cn : stop,
                  'BLSR frontier or caller return',
                );
                equal(result.retired, retired, 'complete original instruction count');
                equal(cpu.readTimeState().tsc - tsc, retired, 'compiler body virtual time');
                equal(
                  result.state.registers[0],
                  BigInt(written.length),
                  'returned or pending count',
                );
                equal(result.state.registers[4], missingBlsr ? sp : sp + 8n, 'guest stack pointer');
                page.set(little(written.map(BigInt), 4), 0x10);
                same(
                  cpu.readPhysicalMemory(0xa000n, page.length),
                  page,
                  'all indices, bounds and return slot',
                );
              },
            );
          }
        }
      },
    },
    {
      name: 'F3 BSF uses the advertised bit-scan semantics on zero and nonzero inputs at all operand widths',
      async run() {
        const bmi1 = await hasBmi1(compiler);
        const dest = 0xaabbccdd55667788n;
        for (const [width, prefix] of [
          [16, '66'],
          [32, ''],
          [64, '48'],
        ] as const) {
          for (const source of [0n, 1n, 1n << BigInt(width - 1)]) {
            const count = source === 0n ? width : source === 1n ? 0 : width - 1;
            let bareFlags = 0n;
            for (const repeated of [false, true]) {
              const code = (repeated ? 'f3' : '') + prefix + '0fbccf'; // destination CX/ECX/RCX
              await createFixture(compiler)(code, async (cpu) => {
                cpu.setState(initial(CODE, { 1: dest, 7: source }, 0x8d7n));
                const result = await cpu.run({ budget: 1n });
                equal(result.reason, 'budget', 'complete scan');
                equal(
                  result.state.rip,
                  CODE + BigInt(code.length / 2),
                  'complete prefix and operand',
                );
                const tzcnt = repeated && bmi1;
                const answer =
                  !tzcnt && source === 0n
                    ? dest
                    : (width === 16 ? dest & ~0xffffn : 0n) | BigInt(count);
                equal(result.state.registers[1], answer, 'width and zero-source destination');
                equal(
                  result.state.flags & 0x40n,
                  (tzcnt ? source === 1n : source === 0n) ? 0x40n : 0n,
                  'ZF',
                );
                if (!repeated) bareFlags = result.state.flags;
                else if (!bmi1) equal(result.state.flags, bareFlags, 'identical pre-BMI BSF flags');
                else
                  equal(
                    result.state.flags & 1n,
                    source === 0n ? 1n : 0n,
                    'TZCNT zero-source carry',
                  );
              });
            }
          }
        }
      },
    },
  ];
}
