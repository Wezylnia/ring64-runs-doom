import { snapshot } from '../far-control/fixture.js';
import { context } from '../modes/fixture.js';
import { BANK, equal, initial, little, same, usingPaged, type Scenario } from './fixture.js';

export function byteReadLoopScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'paged MOVZX keeps the exact byte loop prefix before a missed read',
      async run() {
        const head = 0x400000n;
        const badLinear = head + 0x2000n;
        for (const [fs, ac] of [
          [false, false],
          [false, true],
          [true, true],
        ] as const) {
          const execution = snapshot();
          execution.cpl = 3;
          if (fs) execution.segments.fs.base = head;
          const base = fs ? 0n : head;
          const flags = ac ? 0x40002n : 2n;
          await usingPaged(
            compiler,
            {
              // Warm three bytes in [RDI+1]. The fourth lap read uses absent [RSI].
              code: fs
                ? '640fb607640fb607640fb64f01640fb6570283c001640fb61e83ed0175e6'
                : '0fb6070fb6070fb64f010fb6570283c0010fb61e83ed0175ea',
              context: execution,
              bank: { ...BANK, cr0: BANK.cr0 | 0x40000n },
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, 0xa027n],
              ],
              spans: [{ physical: 0xa000n, bytes: Uint8Array.of(0, 0xff, 0x34, 42) }],
            },
            async (cpu) => {
              cpu.setState(
                initial(
                  head,
                  {
                    0: 0xffffffffffffffffn,
                    3: 0x1234n,
                    5: 2n,
                    6: base + 0x2000n,
                    7: base + 0x1001n,
                  },
                  flags,
                ),
              );
              const result = await cpu.run({ budget: 20n });
              equal(result.reason, 'page-fault', 'the fourth read misses with AC clear or set');
              equal(
                result.pageFault?.address,
                badLinear,
                'non-identity FS resolves the same operand',
              );
              equal(result.pageFault?.errorCode, 4, 'not-present user read');
              equal(result.retired, 5n, 'preload and four completed lap instructions');
              equal(result.state.rip, head + (fs ? 21n : 17n), 'failed MOVZX remains unretired');
              equal(result.state.registers[0], 256n, 'read 0xff, zero-extend, then add one');
              equal(result.state.registers[1], 0x34n, 'second byte committed');
              equal(result.state.registers[2], 42n, 'third byte committed');
              equal(result.state.registers[3], 0x1234n, 'miss publishes no destination');
              equal(
                result.state.flags & 0x408d7n,
                (flags & 0x40000n) | 0x16n,
                'ADD and AC survive',
              );
            },
          );
        }
      },
    },
    {
      name: 'paged MOVZX inner loop preserves state across repeat visits and a retained miss',
      async run() {
        const head = 0x400000n;
        // Preload, then two outer visits of two inner laps with four MOVZX loads.
        const code = '0fb607b9020000000fb6070fb657010fb65f020fb677034101c0ffc975eaffcd75e1';
        for (const miss of [false, true]) {
          const data = miss ? 0x401ffdn : 0x401000n;
          await usingPaged(
            compiler,
            {
              code,
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, 0xa027n],
                [0x4010n, 0xb027n],
              ],
              spans: miss
                ? [
                    { physical: 0xaffdn, bytes: Uint8Array.of(0xff, 10, 11) },
                    { physical: 0xb000n, bytes: Uint8Array.of(12) },
                  ]
                : [{ physical: 0xa000n, bytes: Uint8Array.of(0xff, 10, 11, 12) }],
            },
            async (cpu) => {
              cpu.setState(initial(head, { 5: 2n, 7: data, 8: 0n }, 2n));
              const result = await cpu.run({ budget: 36n, stop: head + 34n });
              equal(result.reason, 'stop', 'both outer visits finish');
              equal(result.resources.nativeEntries, 1, 'both visits use one generated activation');
              equal(result.retired, 35n, 'all inner laps retire exactly once');
              equal(result.state.registers[1], 0n, 'the inner counter finishes');
              equal(result.state.registers[5], 0n, 'the outer counter finishes');
              equal(result.state.registers[6], 12n, 'the fourth read commits on both mapped pages');
              equal(
                result.state.registers[8],
                1020n,
                'the four guarded or checked additions commit',
              );
              equal(result.state.flags, 0x46n, 'the outer loop exit publishes the last DEC');
            },
          );
        }
      },
    },
    {
      name: 'paged affine byte loop agrees across direct and checked span boundaries',
      async run() {
        const head = 0x400000n;
        // Preload, then four indexed MOVZX loads and a four-byte induction step.
        const code = '0fb60731c90fb6040f0fb6540f010fb65c0f020fb6740f034101c04883c1044883f90875e0';
        const bytes = Uint8Array.of(0xff, 1, 2, 3, 4, 5, 6, 7);
        for (const layout of [
          'local',
          'split-ram',
          'split-tlb',
          'contiguous-tlb',
          'global',
          'denied',
        ] as const) {
          const splitTlb = layout === 'split-tlb';
          const contiguousTlb = layout === 'contiguous-tlb';
          const crossesTlb = splitTlb || contiguousTlb;
          const denied = layout === 'denied';
          const global = layout === 'global';
          const splitRam = denied || layout === 'split-ram';
          await usingPaged(
            compiler,
            {
              code,
              bank: { ...BANK, cr4: global ? BANK.cr4 | 0x80n : BANK.cr4 },
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, global ? 0xa127n : 0xa027n],
                [0x4010n, 0xb027n],
              ],
              spans:
                splitRam || splitTlb
                  ? [
                      { physical: splitTlb ? 0xaffcn : 0xa000n, bytes: bytes.slice(0, 4) },
                      {
                        physical: splitTlb ? 0xb000n : 0xa004n,
                        bytes: bytes.slice(4),
                        read: !denied,
                      },
                    ]
                  : [{ physical: contiguousTlb ? 0xaffcn : 0xa000n, bytes }],
            },
            async (cpu) => {
              const start = initial(head, { 7: crossesTlb ? 0x401ffcn : 0x401000n, 8: 0n }, 2n);
              if (contiguousTlb) {
                cpu.setState(initial(head, { 7: 0x402000n }, 2n));
                equal((await cpu.run({ budget: 1n })).reason, 'budget', 'retain the second leaf');
              }
              cpu.setState(start);
              const result = await cpu.run({ budget: 20n, stop: head + 37n });
              equal(result.reason, denied ? 'physical-memory-fault' : 'stop', layout);
              equal(result.retired, denied ? 10n : 18n, `${layout} retirement`);
              equal(result.state.rip, head + (denied ? 5n : 37n), `${layout} next PC`);
              equal(result.state.registers[0], denied ? 0xffn : 4n, 'failed byte does not publish');
              equal(result.state.registers[1], denied ? 4n : 8n, 'induction counter');
              equal(result.state.registers[6], denied ? 3n : 7n, 'last successful byte');
              equal(result.state.registers[8], denied ? 255n : 259n, 'exact sum with byte 0xff');
              equal(result.state.flags, denied ? 0x97n : 0x46n, 'last comparison decides flags');
              if (layout !== 'local' && !contiguousTlb) return;
              cpu.setState(start);
              const partial = await cpu.run({ budget: 11n });
              equal(partial.reason, 'budget', 'partial second lap takes the checked entry');
              equal(partial.retired, 11n, 'one partial-lap byte retires');
              equal(partial.state.rip, head + 9n, 'resume after the checked byte');
              equal(partial.state.registers[0], 4n, 'partial-lap byte is visible');
              equal(partial.state.flags, 0x97n, 'partial lap preserves the prior comparison');
            },
          );
        }
      },
    },
    {
      name: 'paged compatibility reads preserve flat data and checked segment boundaries',
      async run() {
        const head = 0x3fffd0n;
        for (const layout of ['dword', 'byte', 'nonflat', 'limit', 'wrap', 'no-read'] as const) {
          const execution = context('compatibility');
          execution.segments.cs.base = 0x30n;
          if (layout === 'nonflat') execution.segments.ds.base = 0x400000n;
          if (layout === 'limit') execution.segments.ds.limit = 0x40100b;
          const byte = layout === 'byte';
          const payload = byte
            ? Uint8Array.of(0xff, 0x34, 42, 7)
            : little([0x12345678ffffffffn, 0x000000070000002an]);
          const code = byte
            ? '0fb6070fb6070fb64f010fb6570283c0010fb61e83ed0175ea'
            : '8b078b078b4f048b570883c0018b1e83ed0175ee';
          const data = layout === 'nonflat' ? 0x1000n : 0x401000n;
          const fourth = layout === 'wrap' ? 0xfffffffen : data + (byte ? 3n : 12n);
          const fault =
            layout === 'limit' || layout === 'wrap'
              ? 'segment-fault'
              : layout === 'no-read'
                ? 'physical-memory-fault'
                : undefined;
          await usingPaged(
            compiler,
            {
              code,
              context: execution,
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, 0xa027n],
              ],
              spans:
                layout === 'no-read'
                  ? [
                      { physical: 0xa000n, bytes: payload.slice(0, 12) },
                      { physical: 0xa00cn, bytes: payload.slice(12), read: false },
                    ]
                  : [{ physical: 0xa000n, bytes: payload }],
            },
            async (cpu) => {
              const high = 0xaabbccdd00000000n;
              cpu.setState(
                initial(
                  head,
                  {
                    0: high,
                    2: 0n,
                    3: high | 0x1234n,
                    5: 2n,
                    6: high | fourth,
                    7: high | data,
                  },
                  2n,
                ),
              );
              const result = await cpu.run({ budget: 16n, stop: head + BigInt(code.length / 2) });
              equal(result.reason, fault ?? 'stop', layout);
              equal(result.retired, fault ? 5n : 15n, 'preserve exact iteration or prefix');
              equal(
                result.state.rip,
                head + (fault ? 13n : BigInt(code.length / 2)),
                'CS-relative PC',
              );
              equal(
                result.state.registers[0],
                high | (byte ? 256n : 0n),
                '32-bit writes preserve high bank',
              );
              equal(result.state.registers[2], 42n, 'earlier load remains committed');
              equal(
                result.state.registers[3],
                high | (fault ? 0x1234n : 7n),
                'failing load publishes no destination',
              );
              equal(result.state.flags, fault ? 0x57n : 0x46n, 'exact prefix or last-lap flags');
              if (fault === 'segment-fault')
                equal(result.segmentFault?.segment, 'ds', 'checked DS has priority over page walk');
            },
          );
        }
      },
    },
    {
      name: 'paged 32-bit affine byte span keeps reads within a flat DS and the address ceiling',
      async run() {
        const head = 0x3fffd0n;
        const code = '0fb60731c90fb6040f0fb6540f010fb65c0f020fb6740f0301c583c10483f90875e3';
        const bytes = Uint8Array.of(0xff, 1, 2, 3, 4, 5, 6, 7);
        for (const layout of ['local', 'address-ceiling', 'wrap', 'nonflat'] as const) {
          const execution = context('compatibility');
          execution.segments.cs.base = 0x30n;
          if (layout === 'nonflat') execution.segments.ds.base = 0x400000n;
          const pdAt3 = new Uint8Array(0x1000);
          pdAt3.set(little([0x7027n]), 0xff8);
          const highPt = new Uint8Array(0x1000);
          highPt.set(little([0xb027n]), 0xff8);
          const data = {
            local: 0x401000n,
            'address-ceiling': 0xfffffff8n,
            wrap: 0xfffffffcn,
            nonflat: 0x1000n,
          }[layout];
          const spans =
            layout === 'wrap'
              ? [
                  { physical: 0x5000n, bytes: little([0xa027n]) },
                  { physical: 0xa000n, bytes: bytes.slice(4) },
                  { physical: 0xbffcn, bytes: bytes.slice(0, 4) },
                ]
              : [{ physical: layout === 'address-ceiling' ? 0xbff8n : 0xa000n, bytes }];
          await usingPaged(
            compiler,
            {
              code,
              context: execution,
              words: [
                [0x1000n, 0x2027n],
                [0x2000n, 0x3027n],
                [0x2018n, 0x6027n],
                [0x3000n, 0x5027n],
                [0x3010n, 0x4027n],
                [0x4000n, 0x9027n],
                [0x4008n, 0xa027n],
              ],
              spans: [
                { physical: 0x6000n, bytes: pdAt3 },
                { physical: 0x7000n, bytes: highPt },
                ...spans,
              ],
            },
            async (cpu) => {
              const high = 0x1234567800000000n;
              cpu.setState(
                initial(
                  head,
                  {
                    0: high,
                    1: high,
                    5: high,
                    6: high,
                    7: high | data,
                  },
                  2n,
                ),
              );
              const result = await cpu.run({ budget: 20n, stop: head + 34n });
              equal(result.reason, 'stop', layout);
              equal(result.retired, 18n, 'both laps retire');
              equal(result.state.rip, head + 34n, 'CS-relative exit');
              equal(result.state.registers[0], high | 4n, 'zero extension preserves high bank');
              equal(result.state.registers[1], high | 8n, 'induction keeps high bank');
              equal(result.state.registers[5], high | 259n, 'exact byte sum across the span');
              equal(result.state.registers[6], high | 7n, 'last byte is visible');
              equal(result.state.flags, 0x46n, 'the exit comparison supplies flags');
            },
          );
        }
      },
    },
  ];
}
