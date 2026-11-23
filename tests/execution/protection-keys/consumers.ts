import { ONE, stack, banks } from '../numeric/x87/fixture.js';
import {
  AC,
  AUX,
  DATA,
  createCpu,
  fixture,
  key,
  memory,
  pageFault,
  pair,
  preserved,
  same,
  state,
  using,
  word,
  type Scenario,
} from './fixture.js';

export function consumerScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'protection keys scalar atomic stack numeric image mask and descriptor operands preserve all candidates on refusal',
      async run() {
        for (const [code, width, write, stackOperand] of [
          ['8b03', 4, false, false],
          ['8903', 4, true, false],
          ['830301', 4, true, false],
          ['8703', 4, true, false],
          ['0fb103', 4, true, false],
          ['0fc70b', 8, true, false],
          ['0fa303', 4, false, false],
          ['0f9403', 1, true, false],
          ['0fb603', 1, false, false],
          ['8f03', 8, true, false],
          ['ff33', 8, false, false],
          ['ff13', 8, false, false],
          ['c3', 8, false, true],
          ['48cf', 40, false, true],
          ['0f1003', 16, false, false],
          ['0f1103', 16, true, false],
          ['0f6f03', 8, false, false],
          ['0f7f03', 8, true, false],
          ['d903', 4, false, false],
          ['d91b', 4, true, false],
          ['dd03', 8, false, false],
          ['dd1b', 8, true, false],
          ['d923', 28, false, false],
          ['d933', 28, true, false],
          ['dd23', 108, false, false],
          ['dd33', 108, true, false],
          ['0fae03', 512, true, false],
          ['0fae0b', 512, false, false],
          ['0fae13', 4, false, false],
          ['0fae1b', 4, true, false],
          ['0fae3b', 1, false, false],
          ['660ff7c1', 16, true, false],
          ['0f0103', 10, true, false],
          ['0f0113', 10, false, false],
        ] as const)
          for (const userAddress of [false, true]) {
            const f = fixture(compiler, code);
            f.leaf(Number(DATA), key(3) | 0xa063n | (userAddress ? 4n : 0n));
            await using(side, f, async (cpu) => {
              cpu.setProtectionKeysState(
                userAddress ? { pkru: pair(3, 1), pkrs: 0 } : { pkru: 0, pkrs: pair(3, 1) },
              );
              const before = state();
              before.registers[1] = 0n;
              if (code === '0fa303') before.registers[0] = 0n; // BT uses EAX as the bitstring index.
              before.registers[7] = DATA;
              if (stackOperand) before.registers[4] = DATA;
              cpu.setState(before);
              cpu.setX87State(stack({ 0: ONE }, 3));
              cpu.setXmmState(banks());
              cpu.setMxcsr(0x1f80);
              const numeric = preserved(cpu),
                context = cpu.readContext();
              cpu.setTimeState({ tsc: 100n, tscAux: AUX });
              const result = await cpu.run({ budget: 1n });
              same(
                result.reason,
                'page-fault',
                `consumer ${code}/${width} userAddress=${userAddress}`,
              );
              same(
                result.pageFault?.address,
                DATA,
                `consumer ${code}/${width} first failing address`,
              );
              pageFault(result, DATA, write ? 'write' : 'read', write ? 0x23 : 0x21);
              same(result.retired, 0n, `${code}/${width} refusal retirement`);
              same(result.state, before, `${code}/${width} candidate scalar rollback`);
              same(cpu.readContext(), context, 'candidate context rollback');
              same(cpu.readTimeState(), { tsc: 100n, tscAux: AUX }, 'candidate no time');
              numeric();
              memory(cpu, f.bytes, 'candidate numeric/payload/image guards');
            });
          }
      },
    },
    {
      name: 'protection keys full split-span denial precedes natural AC and dirty promotion without partial scalar or FX stores',
      async run() {
        for (const [code, write, aligned] of [
          ['488b03', false, false],
          ['488903', true, false],
          ['0fae03', true, true],
          ['0fae0b', false, true],
        ] as const) {
          const f = fixture(compiler, code, { cpl: 3 });
          f.paging.cr0 |= AC;
          f.leaf(0x6000, 0xa027n); // D0 before the denied tail.
          f.leaf(0x7000, key(3) | 0xb027n);
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: pair(3, 1), pkrs: 0 });
            const before = state(2n | AC);
            before.registers[3] = aligned ? 0x6ff0n : 0x6ffdn;
            cpu.setState(before);
            const numeric = preserved(cpu),
              result = await cpu.run({ budget: 1n });
            pageFault(result, 0x7000n, write ? 'write' : 'read', write ? 0x27 : 0x25);
            same(result.alignmentCheck, undefined, 'page span before natural AC');
            same(result.state, before, 'split scalar unchanged');
            same(word(cpu, f.pt + 6 * 8), 0xa027n, 'no D on admitted prefix before refused tail');
            numeric();
            memory(cpu, f.bytes, 'split complete payload atomicity');
          });
        }
      },
    },
    {
      name: 'protection keys mandatory alignment numeric admission and canonical checks retain earlier priority',
      async run() {
        for (const [code, kind] of [
          ['0f2803', 'alignment-fault'],
          ['0fae03', 'alignment-fault'],
          ['d903', 'device-not-available'],
          ['8b03', 'data-fault'],
        ] as const) {
          const f = fixture(compiler, code);
          f.leaf(Number(DATA), key(3) | 0xa063n);
          if (kind === 'device-not-available') f.paging.cr0 |= 8n;
          await using(side, f, async (cpu) => {
            cpu.setProtectionKeysState({ pkru: 0, pkrs: pair(3, 1) });
            const before = state();
            before.registers[3] = kind === 'data-fault' ? 0x800000000000n : DATA + 1n;
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            same(result.reason, kind, 'existing pre-data priority');
            if (kind === 'data-fault')
              same(result.fault?.cause, 'noncanonical', 'canonical geometry diagnostic');
            same(result.pageFault, undefined, 'keys not reached before earlier admission');
            same(cpu.readPagingState().cr2, 0xdeadn, 'no manufactured key CR2');
          });
        }
      },
    },
    {
      name: 'protection keys REP preserves one completed element before a denied next page and resumes without replay',
      async run() {
        const f = fixture(compiler, 'f3a4');
        f.leaf(0x7000, key(3) | 0xb063n);
        f.bytes[0xafff] = 0x42;
        await using(side, f, async (cpu) => {
          cpu.setProtectionKeysState({ pkru: 0, pkrs: pair(3, 1) });
          const before = state();
          before.registers[1] = 2n;
          before.registers[6] = 0x6fffn;
          before.registers[7] = 0x4000n;
          cpu.setState(before);
          const failed = await cpu.run({ budget: 1n, repeatBudget: 2n });
          pageFault(failed, 0x7000n, 'read', 0x21);
          same(failed.retired, 0n, 'partial REP not retired');
          same(failed.repeatIterations, 1n, 'one observed complete element');
          same(
            [failed.state.registers[1], failed.state.registers[6], failed.state.registers[7]],
            [1n, 0x7000n, 0x4001n],
            'restart element cursor',
          );
          const expected = f.bytes.slice();
          expected[0x9000] = 0x42;
          memory(cpu, expected, 'first element already committed');
          cpu.setProtectionKeysState({ pkru: 0, pkrs: 0 });
          const resumed = await cpu.run({ budget: 1n, repeatBudget: 2n });
          same(resumed.reason, 'budget', 'repair resumes REP');
          same(resumed.repeatIterations, 1n, 'only remaining element');
          same(
            [resumed.state.registers[1], resumed.state.registers[6], resumed.state.registers[7]],
            [0n, 0x7001n, 0x4002n],
            'final element cursors',
          );
          expected[0x9001] = f.bytes[0xb000]!;
          memory(cpu, expected, 'no replay or extra stores');
        });
      },
    },
    {
      name: 'protection keys INS consumes input before denied destination while OUTS and no-work gates produce no output',
      async run() {
        for (const code of ['f36c', 'f36e'])
          for (const count of [0n, 2n]) {
            const f = fixture(compiler, code),
              calls: unknown[] = [];
            f.leaf(Number(DATA), key(3) | 0xa063n);
            f.options.io = {
              read(port, width) {
                calls.push(['read', port, width]);
                return calls.length === 1 ? 0x11 : 0x22;
              },
              write(...args) {
                calls.push(['write', ...args]);
              },
            };
            await using(side, f, async (cpu) => {
              cpu.setProtectionKeysState({ pkru: 0, pkrs: pair(3, 1) });
              const before = state();
              before.registers[1] = count;
              before.registers[2] = 0x80n;
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n, repeatBudget: 2n });
              same(result.reason, count === 0n ? 'budget' : 'page-fault', 'I/O work gate');
              same(
                calls,
                count && code === 'f36c' ? [['read', 0x80, 8]] : [],
                'ordered provider frontier',
              );
              if (count) {
                pageFault(
                  result,
                  DATA,
                  code === 'f36c' ? 'write' : 'read',
                  code === 'f36c' ? 0x23 : 0x21,
                );
                same(result.state, before, 'failed element cursors');
              }
              memory(cpu, f.bytes, 'no denied I/O payload');
              if (count && code === 'f36c') {
                cpu.setProtectionKeysState({ pkru: 0, pkrs: 0 });
                const resumed = await cpu.run({ budget: 1n, repeatBudget: 2n });
                same(resumed.reason, 'budget', 'input retry completes');
                same(calls.length, 3, 'fresh inputs instead of replaying lost input');
                same(
                  cpu.readPhysicalMemory(0xa000n, 2),
                  new Uint8Array([0x22, 0x22]),
                  'fresh retry values',
                );
              }
            });
          }
      },
    },
  ];
}
