import {
  BANK,
  PC,
  equal,
  hex,
  initial,
  little,
  same,
  usingPaged,
  word,
  type Scenario,
} from '../fixture.js';

const A = 0x11223344n;
const B = 0x55667788n;
function page(value: bigint): Uint8Array {
  const bytes = new Uint8Array(0x1000);
  bytes.set(little([value], 4));
  return bytes;
}
function setup(code: string, leaf = 0xa067n) {
  return {
    code: code.replaceAll(' ', ''),
    words: [
      [0x4800n, leaf],
      [0x4808n, 0x4027n],
    ] as const,
    spans: [
      { physical: 0xa000n, bytes: page(A) },
      { physical: 0xb000n, bytes: page(B) },
    ],
  };
}

export function retainedTranslationScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'retained translations guest PTE stores keep the old data mapping until INVLPG',
      async run() {
        await usingPaged(compiler, setup('8b03 48890f 8b13 0f013b 8b33'), async (cpu) => {
          const before = initial(PC, { 3: 0x500000n, 7: 0x501800n, 1: 0xb067n }, 2n);
          cpu.setState(before);
          const result = await cpu.run({ budget: 5n });
          equal(result.reason, 'budget', 'all five instructions complete');
          equal(result.retired, 5n, 'single retirement per operation');
          equal(result.state.rip, PC + 12n, 'exact successor');
          same(
            result.state.registers,
            before.registers.map((value, i) => (i === 0 || i === 2 ? A : i === 6 ? B : value)),
            'retained and invalidated literal values',
          );
          equal(result.state.flags, 2n, 'MOV and INVLPG preserve flags');
          equal(word(cpu, 0x4800n), 0xb067n, 'no cached old PTE overwrite');
          same(cpu.readPhysicalMemory(0xa000n, 4), little([A], 4), 'old payload intact');
          same(cpu.readPhysicalMemory(0xb000n, 4), little([B], 4), 'new payload intact');
        });
      },
    },
    {
      name: 'retained translations D0 RMW refresh precedes old operand capture while D-ready hits keep their mapping',
      async run() {
        for (const dirtyReady of [false, true])
          await usingPaged(
            compiler,
            setup('8b03 48890f 830301 8b13', dirtyReady ? 0xa067n : 0xa027n),
            async (cpu) => {
              cpu.setState(initial(PC, { 3: 0x500000n, 7: 0x501800n, 1: 0xb027n }, 2n));
              const result = await cpu.run({ budget: 4n });
              equal(result.reason, 'budget', 'RMW completes');
              equal(result.retired, 4n, 'no replay of PTE store or RMW');
              equal(result.state.registers[0], A, 'read fill came from A');
              equal(
                result.state.registers[2],
                (dirtyReady ? A : B) + 1n,
                'RMW captured selected literal bytes',
              );
              equal(
                word(cpu, 0x4800n),
                dirtyReady ? 0xb027n : 0xb067n,
                'D publication follows selected mapping knowledge',
              );
              same(
                cpu.readPhysicalMemory(0xa000n, 4),
                little([A + (dirtyReady ? 1n : 0n)], 4),
                'A payload',
              );
              same(
                cpu.readPhysicalMemory(0xb000n, 4),
                little([B + (dirtyReady ? 0n : 1n)], 4),
                'B payload',
              );
            },
          );
      },
    },
    {
      name: 'retained translations stale restrictive D1 rights fault once then retry current page permissions',
      async run() {
        await usingPaged(
          compiler,
          { ...setup('8b03 48890f 8903', 0xa065n), bank: { ...BANK, cr0: BANK.cr0 | 0x10000n } },
          async (cpu) => {
            cpu.setState(initial(PC, { 3: 0x500000n, 7: 0x501800n, 1: 0xa067n }, 2n));
            const fault = await cpu.run({ budget: 3n });
            equal(fault.reason, 'page-fault', 'retained read-only fact faults');
            equal(fault.retired, 2n, 'granting guest store already completed');
            equal(fault.state.rip, PC + 5n, 'write not retired');
            equal(fault.pageFault?.address, 0x500000n, 'write LA');
            equal(fault.pageFault?.errorCode, 3, 'present supervisor write refusal');
            equal(word(cpu, 0x4800n), 0xa067n, 'guest permission grant retained');
            const retry = await cpu.run({ budget: 1n });
            equal(retry.reason, 'budget', 'PF eviction prevents indefinite stale refault');
            equal(retry.retired, 1n, 'only rejected write retries');
            equal(retry.state.rip, PC + 7n, 'fresh successor');
            equal(cpu.readPagingState().cr2, 0x500000n, 'successful retry preserves CR2');
          },
        );
      },
    },
    {
      name: 'retained translations source eviction rejects an older generated body with unchanged physical bytes',
      async run() {
        const reads = 64;
        const data = new Uint8Array(reads * 0x1000);
        for (let i = 0; i < reads; i++) data.set(little([BigInt(i + 1)], 4), i * 0x1000);
        await usingPaged(
          compiler,
          {
            code: '48890f' + '8b034881c300100000'.repeat(reads),
            words: [
              [0x4008n, 0xa027n],
              [0x4ff8n, 0x4027n],
              ...Array.from(
                { length: reads },
                (_, i) => [0x4800n + BigInt(i * 8), 0x100027n + BigInt(i * 0x1000)] as const,
              ),
            ],
            spans: [
              { physical: 0xa000n, bytes: hex('b811111111'), execute: true },
              { physical: 0xb000n, bytes: hex('b822222222'), execute: true },
              { physical: 0x100000n, bytes: data },
            ],
          },
          async (cpu) => {
            const target = PC + 0x1000n;
            cpu.setState(initial(target, {}, 2n));
            equal(
              (await cpu.run({ budget: 1n })).state.registers[0],
              0x11111111n,
              'prime old source translation',
            );
            cpu.setState(initial(PC, { 7: 0x5ff008n, 1: 0xb027n }, 2n));
            const retarget = await cpu.run({ budget: 1n });
            equal(retarget.retired, 1n, 'guest retarget commits once');
            cpu.setState(initial(target, {}, 2n));
            const retained = await cpu.run({ budget: 1n });
            equal(
              retained.state.registers[0],
              0x11111111n,
              'compile a new old-body owner after PTE already names B',
            );
            cpu.setState(initial(PC + 3n, { 3: 0x500000n }, 2n));
            const evict = await cpu.run({ budget: BigInt(reads * 2) });
            equal(evict.reason, 'budget', 'nonconflicting data work completes');
            equal(evict.retired, BigInt(reads * 2), 'all reads and increments completed');
            equal(evict.state.registers[0], 64n, 'last data value');
            cpu.setState(initial(target, {}, 2n));
            const fresh = await cpu.run({ budget: 1n });
            equal(fresh.reason, 'budget', 'stale-body reconciliation makes progress');
            equal(fresh.retired, 1n, 'new body retires once');
            equal(fresh.state.registers[0], 0x22222222n, 'new authoritative code wins');
            same(
              cpu.readPhysicalMemory(0xa000n, 5),
              hex('b811111111'),
              'old code bytes never changed',
            );
            equal(
              word(cpu, 0x4008n),
              0xb027n,
              'no later table mutation can explain owner retirement',
            );
          },
        );
      },
    },
  ];
}
