import { BANK, PC, equal, initial, little, usingPaged, type Scenario } from './fixture.js';
import { context } from '../modes/fixture.js';

// Integer preload warms the data page before the six-instruction SSE loop.
const code = '8b17f30f1007f30f5906f30f58c183c00183e90175ec';
const missingLoad = code.replace('f30f1007', 'f30f1006');
const data = PC + 0x1000n;
const unmapped = PC + 0x2000n;
const loaded = 0x3fc00000n;
const oldXmm = 0x123456789abcdef0abcdef123f000000n;
const dataPages = [
  [0x1000n, 0x2027n],
  [0x2000n, 0x3027n],
  [0x3010n, 0x4027n],
  [0x4000n, 0x9027n],
  [0x4008n, 0xa027n],
] as const;

export function pagingScalarF32LoopScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'paged scalar F32 loop preserves NaN, rounding status, and checked numeric exceptions',
      async run() {
        for (const test of [
          {
            label: 'masked signaling NaN',
            left: loaded,
            right: 0x7f800011n,
            mxcsr: 0x1f80,
            after: 0x1f81,
            result: 0x7fc00011n,
            reason: 'stop',
            bank: BANK,
          },
          {
            label: 'masked precision',
            left: 0x3f800001n,
            right: 0x3f800001n,
            mxcsr: 0x1f80,
            after: 0x1fa0,
            result: 0x3f800002n,
            reason: 'stop',
            bank: BANK,
          },
          {
            label: 'unmasked invalid',
            left: loaded,
            right: 0x7f800011n,
            mxcsr: 0x1f00,
            after: 0x1f01,
            result: loaded,
            reason: 'simd-exception',
            bank: BANK,
          },
          {
            label: 'unmasked precision',
            left: 0x3f800001n,
            right: 0x3f800001n,
            mxcsr: 0x0f80,
            after: 0x0fa0,
            result: 0x3f800001n,
            reason: 'simd-exception',
            bank: BANK,
          },
          {
            label: 'numeric UD',
            left: loaded,
            right: 0x7f800011n,
            mxcsr: 0x1f00,
            after: 0x1f01,
            result: loaded,
            reason: 'invalid-encoding',
            bank: { ...BANK, cr4: BANK.cr4 & ~0x400n },
          },
        ] as const) {
          await usingPaged(
            compiler,
            {
              code,
              bank: test.bank,
              words: dataPages,
              spans: [{ physical: 0xa000n, bytes: little([test.left, test.right], 4) }],
            },
            async (cpu) => {
              cpu.setState(initial(PC, { 1: 2n, 6: data + 4n, 7: data }, 2n));
              cpu.setXmmState([oldXmm, ...Array<bigint>(15).fill(0n)]);
              cpu.setMxcsr(test.mxcsr);
              const result = await cpu.run({ budget: 30n, stop: PC + 22n });
              equal(result.reason, test.reason, test.label);
              equal(
                result.retired,
                test.reason === 'stop' ? 13n : 2n,
                'only completed instructions',
              );
              equal(result.state.rip, PC + (test.reason === 'stop' ? 22n : 6n), 'fault or exit PC');
              equal(cpu.readMxcsr(), test.after, 'published MXCSR');
              equal(cpu.readXmmState()[0], test.result, 'published XMM including upper lanes');
              if (test.reason === 'simd-exception')
                equal(result.simdException?.instructionPc, PC + 6n, '#XM points to MULSS');
              if (test.reason === 'invalid-encoding')
                equal(result.numericControlFault?.cause, 'osxmmexcpt', 'checked numeric #UD');
            },
          );
        }
      },
    },
    {
      name: 'paged scalar F32 loop keeps admission and operand misses before destination writes',
      async run() {
        for (const [bank, reason, codeBytes, retired] of [
          [{ ...BANK, cr0: BANK.cr0 | 8n }, 'device-not-available', missingLoad, 1n],
          [{ ...BANK, cr0: BANK.cr0 | 4n }, 'invalid-encoding', missingLoad, 1n],
          [{ ...BANK, cr4: BANK.cr4 & ~0x200n }, 'invalid-encoding', missingLoad, 1n],
          [BANK, 'page-fault', missingLoad, 1n],
          [BANK, 'page-fault', code, 2n],
        ] as const) {
          await usingPaged(
            compiler,
            {
              code: codeBytes,
              bank,
              words: dataPages,
              spans: [{ physical: 0xa000n, bytes: little([loaded], 4) }],
            },
            async (cpu) => {
              cpu.setState(initial(PC, { 1: 2n, 6: unmapped, 7: data }, 2n));
              cpu.setXmmState([oldXmm, ...Array<bigint>(15).fill(0n)]);
              const result = await cpu.run({ budget: 30n, stop: PC + 22n });
              equal(result.reason, reason, 'checked reason and priority');
              equal(result.retired, retired, 'exact committed prefix');
              equal(
                result.state.rip,
                PC + (retired === 1n ? 2n : 6n),
                'original failed instruction',
              );
              equal(
                cpu.readXmmState()[0],
                retired === 1n ? oldXmm : loaded,
                'miss has no XMM write',
              );
              equal(cpu.readMxcsr(), 0x1f80, 'miss has no MXCSR write');
              if (reason === 'page-fault')
                equal(result.pageFault?.address, unmapped, 'operand address');
            },
          );
        }
      },
    },
    {
      name: 'paged scalar F32 loop returns exact interior budget and stop boundaries',
      async run() {
        await usingPaged(
          compiler,
          {
            code,
            words: dataPages,
            spans: [{ physical: 0xa000n, bytes: little([loaded, 0x40000000n], 4) }],
          },
          async (cpu) => {
            const start = () => {
              cpu.setState(initial(PC, { 1: 2n, 6: data + 4n, 7: data }, 2n));
              cpu.setXmmState([oldXmm, 0x3f800000n, ...Array<bigint>(14).fill(0n)]);
            };
            start();
            const partial = await cpu.run({ budget: 8n, stop: PC + 22n });
            equal(partial.reason, 'budget', 'one full lap then its next MOVSS');
            equal(partial.retired, 8n, 'exact budget');
            equal(partial.state.rip, PC + 6n, 'second MULSS has not begun');
            equal(partial.state.registers[0], 1n, 'one integer addition');
            equal(cpu.readXmmState()[0], loaded, 'second MOVSS retires');
            const resumed = await cpu.run({ budget: 10n, stop: PC + 22n });
            equal(resumed.retired, 5n, 'resume does not repeat MOVSS');
            equal(resumed.reason, 'stop', 'normal loop exit');
            equal(cpu.readXmmState()[0], 0x40800000n, 'exact product plus addend');
            start();
            const stopped = await cpu.run({ budget: 30n, stop: PC + 10n });
            equal(stopped.reason, 'stop', 'interior stop prevents speculative lap');
            equal(stopped.retired, 3n, 'preload, MOVSS, MULSS retire');
            equal(cpu.readXmmState()[0], 0x40400000n, 'ADDSS has not begun');
          },
        );
      },
    },
    {
      name: 'paged aligned scalar F32 span preserves both operands and live rounding',
      async run() {
        // Warm both data translations, then MOVSS/MULSS/ADDSS with a shared SIB index.
        const code = '8b178b16f30f10049ff30f59049ef30f58c848ffc34839cb75ea';
        for (const layout of [
          'union',
          'reversed',
          'split-backing',
          'split-leaf',
          'unaligned',
          'round-up',
          'partial',
        ] as const) {
          const directed = layout === 'round-up';
          const left = little(
            directed ? [0x3f800001n] : [0x3f800000n, 0x40000000n, 0x40400000n],
            4,
          );
          const right = little(
            directed ? [0x3f800001n] : [0x40800000n, 0x40a00000n, 0x40c00000n],
            4,
          );
          const first = layout === 'reversed' ? 16n : 0n;
          const other =
            layout === 'split-leaf' ? 0x1000n : layout === 'unaligned' ? 17n : 16n - first;
          const payload = new Uint8Array(32);
          payload.set(left, Number(first));
          payload.set(right, Number(other & 0xfffn));
          const spans =
            layout === 'split-backing' || layout === 'split-leaf'
              ? [
                  { physical: 0xa000n, bytes: left },
                  { physical: 0xa000n + other, bytes: right },
                ]
              : [{ physical: 0xa000n, bytes: payload }];
          const iterations = directed ? 1n : 3n;
          await usingPaged(
            compiler,
            {
              code,
              words: [...dataPages, [0x4010n, 0xb027n]],
              spans,
            },
            async (cpu) => {
              cpu.setState(
                initial(PC, { 1: iterations, 3: 0n, 6: data + other, 7: data + first }, 2n),
              );
              cpu.setMxcsr(directed ? 0x5f80 : 0x1f80);
              let prefix = 0n;
              if (layout === 'partial') {
                const partial = await cpu.run({ budget: 9n, stop: PC + 26n });
                equal(partial.reason, 'budget', 'one full lap and the next MOVSS');
                equal(partial.retired, 9n, 'exact partial prefix');
                equal(partial.state.rip, PC + 9n, 'the second MULSS is unstarted');
                equal(cpu.readXmmState()[0], 0x40000000n, 'the second MOVSS completed');
                equal(cpu.readXmmState()[1], 0x40800000n, 'first sum is already published');
                prefix = partial.retired;
              }
              const result = await cpu.run({ budget: 27n, stop: PC + 26n });
              equal(result.reason, 'stop', layout);
              equal(
                result.retired + prefix,
                2n + 6n * iterations,
                'two preloads and each original lap',
              );
              equal(cpu.readXmmState()[1], directed ? 0x3f800003n : 0x42000000n, 'independent sum');
              equal(cpu.readMxcsr(), directed ? 0x5fa0 : 0x1f80, 'live rounding and sticky status');
            },
          );
        }
      },
    },
    {
      name: 'paged compatibility32 scalar F32 loop uses only flat dword DS operands',
      async run() {
        for (const layout of ['flat', 'segmented', 'limited', 'unmasked'] as const) {
          const execution = context('compatibility');
          const base = layout === 'segmented' ? 0x800n : 0n;
          execution.segments.ds.base = base;
          if (layout === 'limited') execution.segments.ds.limit = Number(data + 3n);
          const right = layout === 'unmasked' ? 0x7f800011n : 0x40000000n;
          await usingPaged(
            compiler,
            {
              code,
              context: execution,
              words: dataPages,
              spans: [{ physical: 0xa000n, bytes: little([loaded, right], 4) }],
            },
            async (cpu) => {
              cpu.setState(initial(PC, { 1: 2n, 6: data + 4n - base, 7: data - base }, 2n));
              cpu.setXmmState([oldXmm, 0x3f800000n, ...Array<bigint>(14).fill(0n)]);
              if (layout === 'unmasked') cpu.setMxcsr(0x1f00);
              const result = await cpu.run({ budget: 30n, stop: PC + 22n });
              if (layout === 'limited' || layout === 'unmasked') {
                equal(
                  result.reason,
                  layout === 'limited' ? 'segment-fault' : 'simd-exception',
                  layout,
                );
                equal(result.retired, 2n, 'only the prelude and MOVSS');
                equal(result.state.rip, PC + 6n, 'MULSS fault');
                equal(cpu.readXmmState()[0], loaded, 'failed MULSS keeps the MOVSS value');
                equal(cpu.readMxcsr(), layout === 'limited' ? 0x1f80 : 0x1f01, 'checked status');
              } else {
                equal(result.reason, 'stop', layout);
                equal(result.retired, 13n, 'all original instructions');
                equal(cpu.readXmmState()[0], 0x40800000n, 'complete XMM result');
              }
            },
          );
        }
      },
    },
  ];
}
