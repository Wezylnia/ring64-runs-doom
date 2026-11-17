import { createCpu } from '../../../host/index.js';
import { LIMITS, equal, hex, initial, type Scenario } from '../../support/fixture.js';
import { little, word } from '../paging/fixture.js';

export function ioSourcePteScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'completed INS retains its fetch translation until guest INVLPG exposes the source fault before another input',
      async run() {
        const tables = new Uint8Array(0x4000);
        for (const [address, value] of [
          [0x10000, 0x11003n],
          [0x11000, 0x12003n],
          [0x12000, 0x13003n],
          [0x13008, 0x20003n],
          [0x13010, 0x21023n],
          [0x13030, 0x13003n],
        ] as const)
          tables.set(little([value]), address - 0x10000);
        const inputs = [0x22, 0, 2];
        let calls = 0;
        const code = new Uint8Array(0x1003).fill(0x82);
        code.set(hex('f36c'));
        code.set(hex('0f013b'), 0x1000);
        const cpu = await createCpu({
          compiler,
          limits: LIMITS,
          memoryModel: 'ia32e-paged',
          entry: 0x1000n,
          paging: { cr0: 0x80000033n, cr2: 0n, cr3: 0x10000n, cr4: 0x628n, efer: 0xd00n },
          memoryLimits: { maxBackingBytes: 0x6000, maxBackings: 2, maxMappings: 2 },
          memory: {
            backings: [
              { id: 'tables', bytes: tables },
              { id: 'code', bytes: code },
            ],
            mappings: [
              {
                base: 0x10000n,
                backing: 'tables',
                offset: 0,
                length: tables.length,
                permissions: { read: true, write: true, execute: false },
              },
              {
                base: 0x20000n,
                backing: 'code',
                offset: 0,
                length: code.length,
                permissions: { read: true, write: false, execute: true },
              },
            ],
          },
          io: {
            read() {
              return inputs[calls++]!;
            },
            write() {},
          },
        });
        try {
          cpu.setState(initial(0x1000n, { 1: 3n, 2: 0x80n, 7: 0x6008n }));
          const first = await cpu.run({ budget: 1n, repeatBudget: 2n });
          equal(
            first.reason,
            'repeat-budget',
            'completed element keeps the retained fetch mapping',
          );
          equal(word(cpu, 0x13008n), 0x20022n, 'input changed source PTE after fetch A');
          equal(calls, 2, 'second required fetch still uses the retained mapping');
          equal(first.repeatIterations, 2n, 'two elements committed before invalidation');
          equal(first.retired, 0n, 'unfinished REP');
          equal(first.state.registers[7], 0x600an, 'committed index');
          equal(first.state.registers[1], 1n, 'committed count');
          const helper = {
            ...first.state,
            rip: 0x2000n,
            registers: first.state.registers.map((value, i) => (i === 3 ? 0x1000n : value)),
          };
          cpu.setState(helper);
          const invalidate = await cpu.run({ budget: 1n });
          equal(invalidate.reason, 'budget', 'guest INVLPG completes on another code page');
          equal(invalidate.retired, 1n, 'one invalidation');
          cpu.setState({ ...invalidate.state, rip: 0x1000n });
          const fault = await cpu.run({ budget: 1n, repeatBudget: 1n });
          equal(fault.reason, 'page-fault', 'invalidation exposes cleared source PTE.P');
          equal(fault.pageFault?.address, 0x1000n, 'source fetch fault');
          equal(fault.pageFault?.errorCode, 0x10, 'NXE instruction fetch error bit');
          equal(fault.retired, 0n, 'unfinished REP cannot restart before fresh fetch');
          equal(fault.repeatIterations, 0n, 'no input after invalidation');
          equal(calls, 2, 'third provider call is blocked');
          cpu.writePhysicalMemory(0x13008n, little([0x20023n]));
          const last = await cpu.run({ budget: 1n, repeatBudget: 1n });
          equal(last.reason, 'budget', 'source repair resumes remaining element');
          equal(calls, 3, 'only remaining input');
          equal(last.state.rip, 0x1002n, 'completed REP');
          equal(last.state.registers[7], 0x600bn, 'final index');
          equal(last.state.registers[1], 0n, 'final count');
        } finally {
          cpu.dispose();
        }
      },
    },
  ];
}
