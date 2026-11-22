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
      name: 'paged F32 replay preserves a completed MOVSS before a faulting MULSS',
      async run() {
        const code = 'f30f10049ff30f59049ef30f58c848ffc34839cb75ea';
        const payload = new Uint8Array(4096);
        payload.set(little([...Array<bigint>(9).fill(0x3f800000n), 0x40400000n], 4));
        payload.set(little(Array<bigint>(9).fill(0x40000000n), 4), 4096 - 36);
        await usingPaged(
          compiler,
          {
            code,
            words: [[0x4008n, 0xa007n]],
            spans: [{ physical: 0xa000n, bytes: payload }],
          },
          async (cpu) => {
            cpu.setState(initial(PC, { 1: 16n, 3: 0n, 6: unmapped - 36n, 7: data }, 2n));
            const result = await cpu.run({ stop: PC + 22n, budget: 120n });
            equal(result.reason, 'page-fault', 'the second stream reaches the absent leaf');
            equal(result.retired, 55n, 'nine complete laps and the next MOVSS retire');
            equal(result.state.rip, PC + 5n, 'the faulting MULSS remains unstarted');
            equal(result.state.registers[3], 9n, 'the faulting lap never increments the index');
            equal(cpu.readXmmState()[0], 0x40400000n, 'only the preceding MOVSS updates XMM0');
            equal(cpu.readXmmState()[1], 0x41900000n, 'the earlier nine sums retire once');
            equal(cpu.readMxcsr(), 0x1f80, 'the absent operand leaves MXCSR unchanged');
            equal(cpu.readPagingState().cr2, unmapped, 'CR2 records the second stream address');
          },
        );
      },
    },
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
          'split-leaf-partial',
          'unaligned',
          'round-up',
          'partial',
        ] as const) {
          const directed = layout === 'round-up';
          const splitLeaf = layout === 'split-leaf' || layout === 'split-leaf-partial';
          const left = little(
            directed ? [0x3f800001n] : [0x3f800000n, 0x40000000n, 0x40400000n],
            4,
          );
          const right = little(
            directed ? [0x3f800001n] : [0x40800000n, 0x40a00000n, 0x40c00000n],
            4,
          );
          const first = layout === 'reversed' ? 16n : 0n;
          const other = splitLeaf ? 0x1000n : layout === 'unaligned' ? 17n : 16n - first;
          const payload = new Uint8Array(32);
          payload.set(left, Number(first));
          payload.set(right, Number(other & 0xfffn));
          const spans =
            layout === 'split-backing' || splitLeaf
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
              if (layout === 'partial' || layout === 'split-leaf-partial') {
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
    {
      name: 'paged compatibility32 scalar F32 span refuses an address wrapping at 4 GiB',
      async run() {
        // The latch is INC EBX, CMP EBX,ECX. Both streams advance by one dword.
        const code = '8b178b16f30f10049ff30f59049ef30f58c8ffc339cb75ec';
        for (const wrap of [false, true]) {
          const right = little([0x40800000n, 0x40a00000n], 4);
          const payload = new Uint8Array(24);
          payload.set(little([0x3f800000n, 0x40000000n], 4));
          payload.set(right, 16);
          const words = wrap
            ? ([
                ...dataPages,
                [0x2018n, 0x3027n],
                [0x3ff8n, 0x4027n],
                [0x4ff8n, 0xa027n],
                [0x3000n, 0x5027n],
              ] as const)
            : dataPages;
          const spans = wrap
            ? [
                { physical: 0x5000n, bytes: little([0xb027n]) },
                { physical: 0xa000n, bytes: right },
                { physical: 0xaffcn, bytes: little([0x3f800000n], 4) },
                { physical: 0xb000n, bytes: little([0x40000000n], 4) },
              ]
            : [{ physical: 0xa000n, bytes: payload }];
          await usingPaged(
            compiler,
            { code, context: context('compatibility'), words, spans },
            async (cpu) => {
              cpu.setState(
                initial(
                  PC,
                  { 1: 2n, 3: 0n, 6: wrap ? data : data + 16n, 7: wrap ? 0xfffffffcn : data },
                  2n,
                ),
              );
              const result = await cpu.run({ budget: 24n, stop: PC + 24n });
              equal(result.reason, 'stop', 'both address sizes complete');
              equal(result.retired, 14n, 'prelude and two complete laps');
              equal(result.state.registers[3], 2n, 'both induction steps');
              equal(cpu.readXmmState()[1], 0x41600000n, 'operands yield 14.0');
              equal(cpu.readMxcsr(), 0x1f80, 'exact arithmetic');
            },
          );
        }
      },
    },
    {
      name: 'paged dyadic scalar F32 preserves zero signs, mixed data, and exact boundaries',
      async run() {
        // Warm both streams. Four scalar products form left-to-right prefixes;
        // the last prefix supplies the next lap's accumulator in XMM1.
        const prefix =
          '8b178b16f30f101c9ff30f10549f04f30f591c9ef30f59549e04f30f58d9' +
          'f30f10649f08f30f59649e08f30f58d3f30f104c9f0cf30f594c9e0c' +
          'f30f58e2f30f58cc';
        const negative = 0x80000000n;
        const nan = 0x7fc00011n;
        const one = 0x3f800000n;
        const initialAccumulator = (oldXmm & ~0xffffffffn) | negative;
        for (const mode of ['long', 'compatibility'] as const) {
          const code = prefix + (mode === 'long' ? '4883c3044839cb75b9' : '83c30439cb75bb');
          for (const kind of ['zero', 'cancel', 'mixed', 'downward', 'partial', 'stop'] as const) {
            const mixed = kind === 'mixed' || kind === 'partial';
            const downward = kind === 'downward';
            const left = Array<bigint>(8).fill(negative);
            const right = Array<bigint>(8).fill(one);
            if (mixed) {
              left.splice(4, 4, one + 1n, 0x7f800011n, one, 0xbf800000n);
              right[4] = one + 1n;
            } else if (downward) {
              left[1] = left[5] = 0n;
            } else if (kind === 'cancel') {
              left[1] = left[5] = one;
              left[2] = left[6] = 0xbf800000n;
            }
            await usingPaged(
              compiler,
              {
                code,
                ...(mode === 'compatibility' ? { context: context(mode) } : {}),
                words: dataPages,
                spans: [{ physical: 0xa000n, bytes: little([...left, ...right], 4) }],
              },
              async (cpu) => {
                cpu.setState(initial(PC, { 1: 8n, 3: 0n, 6: data + 32n, 7: data }, 2n));
                cpu.setXmmState([0n, initialAccumulator, ...Array<bigint>(14).fill(0n)]);
                const mxcsr = downward ? 0x3f80 : 0x1f80;
                cpu.setMxcsr(mxcsr);
                const stop = PC + BigInt(kind === 'stop' ? 15 : code.length / 2);
                const result = await cpu.run({ budget: kind === 'partial' ? 18n : 40n, stop });
                if (kind === 'stop') {
                  equal(result.reason, 'stop', 'interior stop precedes MULSS');
                  equal(result.retired, 4n, 'preloads and both MOVSS operations retire');
                  equal(cpu.readXmmState()[1], initialAccumulator, 'accumulator is still incoming');
                  equal(cpu.readXmmState()[2], negative, 'second MOVSS has retired');
                  equal(cpu.readXmmState()[3], negative, 'first MOVSS has retired');
                  equal(cpu.readMxcsr(), mxcsr, 'interior stop does not publish arithmetic status');
                  return;
                }
                if (kind === 'partial') {
                  equal(result.reason, 'budget', 'one exact lap and a following MOVSS retire');
                  equal(result.retired, 18n, 'partial retired count');
                  equal(result.state.rip, PC + 9n, 'second MOVSS is unstarted');
                  equal(result.state.registers[3], 4n, 'only the first latch has committed');
                  for (const bank of [1, 2, 4])
                    equal(cpu.readXmmState()[bank], negative, 'first exact lap is published');
                  equal(cpu.readXmmState()[3], one + 1n, 'partial MOVSS overwrites only XMM3');
                  equal(cpu.readMxcsr(), mxcsr, 'the precise exception is still unstarted');
                  const resumed = await cpu.run({ budget: 32n, stop });
                  equal(resumed.reason, 'stop', 'resume executes the exact suffix');
                  equal(resumed.retired, 14n, 'resume excludes all committed instructions');
                } else {
                  equal(
                    result.reason,
                    'stop',
                    `${mode}/${kind}: pc=${result.state.rip.toString(16)} fault=${result.pageFault?.address.toString(16)}`,
                  );
                  equal(result.retired, 32n, 'preloads plus the two original laps');
                }
                const expected = mixed
                  ? [nan, nan, one + 2n, nan]
                  : kind === 'cancel'
                    ? [0n, one, 0n, 0n]
                    : Array<bigint>(4).fill(negative);
                for (const [index, value] of expected.entries())
                  equal(cpu.readXmmState()[index + 1], value, 'exact XMM prefix and upper bits');
                equal(cpu.readMxcsr(), mixed ? mxcsr | 0x21 : mxcsr, 'precise sticky status');
              },
            );
          }
        }
      },
    },
  ];
}
