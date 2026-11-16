import {
  AC,
  AUX,
  PC,
  createCpu,
  fixture,
  using,
  state,
  runOne,
  scalar,
  checkFault,
  clock,
  memory,
  same,
  equal,
  hex,
  put,
  preserved,
  type Scenario,
} from './fixture.js';

export function orderedScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'alignment checking INS OUTS no-work and privilege gates prevent all operand and provider work',
      async run() {
        for (const code of ['f36d', 'f36f'])
          for (const kind of ['zero', 'budget', 'privilege'] as const) {
            const f = fixture(compiler, code),
              calls: unknown[] = [];
            f.options.io = {
              read(...args) {
                calls.push(args);
                return 0x11;
              },
              write(...args) {
                calls.push(args);
              },
            };
            await using(side, f, async (cpu) => {
              const before = state({
                1: kind === 'zero' ? 0n : 2n,
                2: 0x80n,
                6: 0x6001n,
                7: 0x6001n,
              });
              cpu.setState(before);
              cpu.setTimeState({ tsc: 100n, tscAux: AUX });
              const result = await cpu.run({
                budget: 1n,
                repeatBudget: kind === 'privilege' ? 7n : 0n,
              });
              equal(
                result.reason,
                kind === 'zero'
                  ? 'budget'
                  : kind === 'budget'
                    ? 'repeat-budget'
                    : 'protection-fault',
                'no-work before permission before reached operand',
              );
              equal(result.alignmentCheck, undefined, 'unreached operand has no AC');
              if (kind === 'privilege')
                same(
                  result.protectionFault,
                  { vector: 13, errorCode: 0, phase: 'privilege', instructionPc: PC },
                  'invalid TR with CPL3 IOPL0 precedes operand',
                );
              same(
                result.state,
                { ...before, rip: kind === 'zero' ? PC + 2n : PC },
                'I/O gate state',
              );
              equal(
                result.retired,
                kind === 'zero' ? 1n : 0n,
                'zero instruction versus refused element',
              );
              equal(result.repeatIterations, 0n, 'no completed provider element');
              same(calls, [], 'no provider before actual admitted work');
              memory(cpu, f.bytes, 'no operand walk or payload');
              clock(cpu, kind === 'zero' ? 101n : 100n);
            });
          }
      },
    },
    {
      name: 'alignment checking INS provider exception remains terminal before a later destination AC',
      async run() {
        const f = fixture(compiler, 'f36d');
        let calls = 0;
        f.options.io = {
          read() {
            calls++;
            throw Error('provider effect before destination');
          },
          write() {
            throw Error('INS cannot write port');
          },
        };
        await using(side, f, async (cpu) => {
          cpu.setState(state({ 1: 2n, 2: 0x80n, 7: 0x6001n }, AC | 0x3002n));
          let rejected = false;
          try {
            await cpu.run({ budget: 1n, repeatBudget: 7n });
          } catch {
            rejected = true;
          }
          equal(rejected, true, 'provider failure wins before possible destination AC');
          equal(calls, 1, 'one uncertain external effect');
          // Existing provider failure policy forbids a retryable snapshot. Do not
          // inspect guest RAM/state here or claim externally consumed input rollback.
          let checkpointRejected = false;
          try {
            cpu.readState();
          } catch {
            checkpointRejected = true;
          }
          equal(checkpointRejected, true, 'terminal provider failure exposes no checkpoint');
          rejected = false;
          try {
            await cpu.run({ budget: 1n });
          } catch {
            rejected = true;
          }
          equal(rejected, true, 'terminal CPU cannot replay input');
          equal(calls, 1, 'no duplicate provider call');
        });
      },
    },
    {
      name: 'alignment checking ENTER retains an earlier push before a genuinely different chain-read residue',
      async run() {
        const f = fixture(compiler, 'c8000002');
        await using(side, f, async (cpu) => {
          const before = state({ 4: 0x5000n, 5: 0x6001n }),
            wanted = f.bytes.slice(),
            unchanged = preserved(cpu);
          wanted.set(hex('01600000'), 0x9ffc);
          const result = await runOne(cpu, before);
          checkFault(result, 0x5ffdn, 4, 'read', 4, 'ss');
          scalar(result, before);
          memory(cpu, wanted, 'prior push survives later chain AC');
          unchanged();
          clock(cpu);
        });
      },
    },
    {
      name: 'alignment checking ENTER final byte probe stays byte-sized and preserves payload on private refusal',
      async run() {
        for (const denied of [false, true]) {
          const f = fixture(compiler, 'c8010000');
          if (denied) {
            const permissions = { read: true, write: true, execute: true };
            f.options.memory!.mappings = [
              { base: 0n, backing: 'ram', offset: 0, length: 0x9ffb, permissions },
              {
                base: 0x9ffbn,
                backing: 'ram',
                offset: 0x9ffb,
                length: 1,
                permissions: { ...permissions, write: false },
              },
              {
                base: 0x9ffcn,
                backing: 'ram',
                offset: 0x9ffc,
                length: f.bytes.length - 0x9ffc,
                permissions,
              },
            ];
          }
          await using(side, f, async (cpu) => {
            const before = state({ 4: 0x5000n, 5: 0x12345678n }),
              wanted = f.bytes.slice();
            wanted.set(hex('78563412'), 0x9ffc);
            const result = await runOne(cpu, before);
            equal(result.alignmentCheck, undefined, 'one-byte endpoint cannot AC');
            if (denied) {
              equal(
                result.reason,
                'physical-memory-fault',
                'probe still checks private write permission',
              );
              same(
                result.physicalMemoryFault,
                {
                  instructionPc: PC,
                  address: 0x4ffbn,
                  access: 'write',
                  physicalAddress: 0x9ffbn,
                  width: 1,
                  failingPhysicalAddress: 0x9ffbn,
                  physicalAccess: 'write',
                  cause: 'permission',
                  phase: 'final-access',
                  segment: 'ss',
                },
                'exact probe refusal',
              );
              scalar(result, before);
            } else {
              equal(result.reason, 'budget', 'odd endpoint succeeds');
              scalar(result, before, 1n, 4, { 4: 0x4ffbn, 5: 0x4ffcn });
            }
            memory(cpu, wanted, 'only saved BP is payload');
            clock(cpu, denied ? 100n : 101n);
          });
        }
      },
    },
    {
      name: 'alignment checking POPA preserves all registers on AC and never accesses the saved-SP hole',
      async run() {
        for (const skipped of [false, true]) {
          const f = fixture(compiler, '61');
          const values = [
            0x11112222n,
            0x33334444n,
            0x55556666n,
            0xdeadbeefn,
            0x77778888n,
            0x9999aaaan,
            0xbbbbccccn,
            0xddddeeeen,
          ];
          for (const [i, value] of values.entries()) put(f.bytes, 0x15000 + 4 * i, value, 4);
          if (skipped) {
            const permissions = { read: true, write: true, execute: true };
            f.options.memory!.mappings = [
              { base: 0n, backing: 'ram', offset: 0, length: 0x1500c, permissions },
              {
                base: 0x1500cn,
                backing: 'ram',
                offset: 0x1500c,
                length: 4,
                permissions: { ...permissions, read: false },
              },
              {
                base: 0x15010n,
                backing: 'ram',
                offset: 0x15010,
                length: f.bytes.length - 0x15010,
                permissions,
              },
            ];
          }
          await using(side, f, async (cpu) => {
            const before = state({ 4: skipped ? 0x5000n : 0x5001n });
            const result = await runOne(cpu, before);
            if (!skipped) {
              checkFault(result, 0x5001n, 4, 'read', 4, 'ss');
              scalar(result, before);
            } else {
              const changes: Record<number, bigint> = { 4: 0x5020n };
              for (const [i, register] of [7, 6, 5, 3, 2, 1, 0].entries())
                changes[register] =
                  (before.registers[register]! & ~0xffffffffn) | values[i < 3 ? i : i + 1]!;
              equal(result.reason, 'budget', 'denied skipped slot is never touched');
              scalar(result, before, 1n, 1, changes);
              cpu.setMappings([
                {
                  base: 0n,
                  backing: 'ram',
                  offset: 0,
                  length: f.bytes.length,
                  permissions: { read: true, write: true, execute: true },
                },
              ]);
            }
            memory(cpu, f.bytes, 'POPA never changes payload');
          });
        }
      },
    },
    {
      name: 'alignment checking MOVS preserves source-before-destination order and skips zero or suspended work',
      async run() {
        for (const source of [false, true])
          for (const gate of ['reached', 'zero', 'budget'] as const) {
            const f = fixture(compiler, 'f3a5');
            f.leaf(0x6000, 0xa007n);
            f.leaf(0x7000, 0xb007n);
            await using(side, f, async (cpu) => {
              const before = state({
                1: gate === 'zero' ? 0n : 2n,
                6: source ? 0x6001n : 0x6000n,
                7: source ? 0x7000n : 0x7001n,
              });
              cpu.setState(before);
              cpu.setTimeState({ tsc: 100n, tscAux: AUX });
              const result = await cpu.run({
                  budget: 1n,
                  repeatBudget: gate === 'budget' ? 0n : 7n,
                }),
                wanted = f.bytes.slice();
              if (gate === 'reached') {
                checkFault(
                  result,
                  source ? 0x6001n : 0x7001n,
                  4,
                  source ? 'read' : 'write',
                  4,
                  source ? 'ds' : 'es',
                );
                put(wanted, f.pt + 6 * f.width, 0xa027n, f.width);
                if (!source) put(wanted, f.pt + 7 * f.width, 0xb027n, f.width);
              } else
                equal(
                  result.reason,
                  gate === 'zero' ? 'budget' : 'repeat-budget',
                  'no-work boundary before operands',
                );
              same(
                result.state,
                { ...before, rip: gate === 'zero' ? PC + 2n : PC },
                'no current element commit',
              );
              equal(result.retired, gate === 'zero' ? 1n : 0n, 'zero repeat completes once');
              equal(result.repeatIterations, 0n, 'no completed element');
              memory(cpu, wanted, 'only reached source/destination A effects');
              clock(cpu, gate === 'zero' ? 101n : 100n);
            });
          }
      },
    },
    {
      name: 'alignment checking REP resumed after AM activation preserves a real completed prefix without invented residue changes',
      async run() {
        const f = fixture(compiler, 'f3a5');
        f.paging.cr0 &= ~AC;
        f.bytes.set(hex('112233445566778899aabbcc'), 0xa001);
        await using(side, f, async (cpu) => {
          const before = state({ 1: 3n }),
            wanted = f.bytes.slice();
          cpu.setState(before);
          cpu.setTimeState({ tsc: 100n, tscAux: AUX });
          const first = await cpu.run({ budget: 1n, repeatBudget: 1n });
          equal(first.reason, 'repeat-budget', 'one bounded completed prefix');
          equal(first.retired, 0n, 'unfinished REP is not retired');
          equal(first.repeatIterations, 1n, 'one element committed');
          const progress = { ...before, registers: [...before.registers] };
          progress.registers[1] = 2n;
          progress.registers[6] = 0x6005n;
          progress.registers[7] = 0x7005n;
          same(first.state, progress, 'exact REP prefix');
          wanted.set(hex('11223344'), 0xb001);
          memory(cpu, wanted, 'first four destination bytes');
          cpu.setPagingState({ ...cpu.readPagingState(), cr0: cpu.readPagingState().cr0 | AC });
          const failed = await cpu.run({ budget: 1n, repeatBudget: 7n });
          checkFault(failed, 0x6005n, 4, 'read', 4);
          same(failed.state, progress, 'new AC does not undo prior prefix');
          equal(failed.repeatIterations, 0n, 'failed resumed element not counted');
          equal(failed.retired, 0n, 'failed REP no retirement');
          clock(cpu);
          memory(cpu, wanted, 'no duplicate or new payload');
          cpu.setPagingState({ ...cpu.readPagingState(), cr0: cpu.readPagingState().cr0 & ~AC });
          const done = await cpu.run({ budget: 1n, repeatBudget: 2n });
          equal(done.retired, 1n, 'one final REP retirement');
          equal(done.repeatIterations, 2n, 'two fresh remaining elements');
          progress.rip = PC + 2n;
          progress.registers[1] = 0n;
          progress.registers[6] = 0x600dn;
          progress.registers[7] = 0x700dn;
          same(done.state, progress, 'full REP completion');
          wanted.set(hex('112233445566778899aabbcc'), 0xb001);
          memory(cpu, wanted, 'literal complete copy');
          clock(cpu, 101n);
        });
      },
    },
    {
      name: 'alignment checking INS retains consumed input before destination AC and retry consumes fresh values',
      async run() {
        const f = fixture(compiler, 'f36d'),
          calls: unknown[] = [],
          values = [0x11111111, 0x22222222, 0x33333333];
        f.options.io = {
          read(port, width) {
            calls.push(['read', port, width]);
            return values[calls.length - 1]!;
          },
          write() {
            throw Error('INS cannot emit output');
          },
        };
        await using(side, f, async (cpu) => {
          const before = state({ 1: 2n, 2: 0x80n, 7: 0x6001n }, AC | 0x3002n),
            wanted = f.bytes.slice();
          const failed = await runOne(cpu, before);
          checkFault(failed, 0x6001n, 4, 'write', 4, 'es');
          scalar(failed, before);
          same(calls, [['read', 0x80, 32]], 'input consumed before destination AC');
          memory(cpu, wanted, 'failed element no payload');
          cpu.setPagingState({ ...cpu.readPagingState(), cr0: cpu.readPagingState().cr0 & ~AC });
          const done = await cpu.run({ budget: 1n, repeatBudget: 7n });
          equal(done.retired, 1n, 'fresh retry completes REP');
          equal(done.repeatIterations, 2n, 'two fresh input elements');
          same(
            calls,
            [
              ['read', 0x80, 32],
              ['read', 0x80, 32],
              ['read', 0x80, 32],
            ],
            'lost first value is not replayed',
          );
          const expected = { ...before, rip: PC + 2n, registers: [...before.registers] };
          expected.registers[1] = 0n;
          expected.registers[7] = 0x6009n;
          same(done.state, expected, 'INS exact final state');
          wanted.set(hex('2222222233333333'), 0xa001);
          memory(cpu, wanted, 'only fresh values published');
          clock(cpu, 101n);
        });
      },
    },
    {
      name: 'alignment checking OUTS source refusal prevents provider output',
      async run() {
        const f = fixture(compiler, 'f36f'),
          calls: unknown[] = [];
        f.options.io = {
          read() {
            throw Error('OUTS cannot read port');
          },
          write(...args) {
            calls.push(args);
          },
        };
        await using(side, f, async (cpu) => {
          const before = state({ 1: 2n, 2: 0x80n }, AC | 0x3002n),
            result = await runOne(cpu, before);
          checkFault(result, 0x6001n, 4, 'read', 4);
          scalar(result, before);
          same(calls, [], 'no output before source');
          memory(cpu, f.bytes, 'OUTS no payload');
        });
      },
    },
  ];
}
