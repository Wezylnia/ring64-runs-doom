import {
  using as usingEvents,
  snapshot,
  loadedCode,
  loadedData,
  withGate,
  gate,
  CODE32,
  DATA32,
  STACK,
  FILL,
  image,
  ready,
  delivery,
} from '../exception-delivery/fixture.js';
import { BANK, PC, initial, little, same, usingPaged } from '../paging/fixture.js';
import { physicalCpu, resetContext } from '../reset/fixture.js';
import {
  INDEFINITE,
  LONG,
  ONE,
  changed,
  context,
  environment,
  equal,
  hex,
  input,
  noFault,
  other,
  outcome,
  pending,
  pop,
  profiles,
  raw,
  seed,
  slot,
  stack,
  using,
  x87,
  type Scenario,
} from './fixture.js';

const sign = 1n << 79n,
  integerBit = 1n << 63n,
  infinity = raw(0x7fff),
  minNormal = raw(1),
  pi = 0x4000c90fdaa22168c235n;
const sentinel = 0x4007cafe12345678abcdn;

export function fpatanScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'FPATAN resolves tags, NaNs and denormal masks before it publishes the wrapped stack',
      async run() {
        const quiet = 0xffffc000000000000123n,
          signaling = 0x7fff8000000000000456n,
          malformed = raw(0x4000, 1n);
        const packets = [
          [ONE, sign, 0x37f, sign, 0],
          [infinity, 1n, 0x37f, 0n, 2],
          [integerBit, 0n, 0x37f, 0n, 2],
          [1n, ONE, 0x37d, undefined, 0x8082],
          [ONE, integerBit, 0x37d, undefined, 0x8082],
          [quiet, 1n, 0x37d, quiet, 0],
          [quiet, signaling, 0x37f, quiet, 1],
          [signaling, quiet, 0x37e, undefined, 0x8081],
          [quiet, malformed, 0x37f, INDEFINITE, 1],
          [undefined, signaling, 0x37f, INDEFINITE, 0x41],
          [ONE, undefined, 0x37e, undefined, 0x80c1],
          [undefined, ONE, 0x37e, undefined, 0x80c1],
          [undefined, undefined, 0x37f, INDEFINITE, 0x41],
        ] as const;
        await using(compiler, '90d9f39b', LONG, async (cpu) => {
          const caller = input(0x1001n);
          cpu.setState(input());
          const predecessor = await cpu.run({ budget: 1n });
          equal(predecessor.state.rip, caller.rip, 'NOP publishes the retained middle entry');
          equal(predecessor.resources.compilations, 1, 'cold predecessor installs code');
          for (const [x, y, cw, answer, exceptions] of packets) {
            const values: Record<number, bigint> = { 2: sentinel };
            if (x !== undefined) values[0] = x;
            if (y !== undefined) values[1] = y;
            const original = stack(values, 7, cw, 0x4704);
            cpu.setState(caller);
            seed(cpu, original);
            const result = await cpu.run({ budget: 1n });
            outcome(result, 'budget', 1n, 1n, 1n, 0n, changed(caller, 0x1003n));
            equal(result.resources.compilations, 0, 'same artifact with new x87 state');

            const expected = environment(
              original,
              caller.rip,
              undefined,
              exceptions & 0x80 ? 0x1f3 : undefined,
            );
            expected.statusWord = (expected.statusWord & ~0x0200) | exceptions;
            if (answer !== undefined) {
              slot(expected, 0, answer);
              pop(expected);
            }
            x87(cpu, expected);
            other(cpu);
            if (exceptions & 0x80) {
              const detector = await cpu.run({ budget: 1n });
              equal(detector.reason, 'x87-exception', 'waiting detector reports ordered condition');
              equal(detector.retired, 0n, 'no retirement on the detector');
              pending(detector, expected, 0x1003n);
              x87(cpu, expected);
            }
          }
        });
      },
    },
    {
      name: 'FPATAN observes an older pending exception only when the quota-limited entry executes',
      async run() {
        await using(
          compiler,
          '90d9f3',
          LONG,
          async (cpu) => {
            const caller = input();
            const old = stack({ 0: ONE, 1: sign }, 7, 0x37e, 0x8081);
            cpu.setState(caller);
            seed(cpu, old);
            const limited = await cpu.run({ budget: 2n });
            outcome(limited, 'resource-limit', 2n, 1n, 2n, 0n, changed(caller, 0x1001n));
            noFault(limited);
            x87(cpu, old);
            const result = await cpu.run({ budget: 1n });
            outcome(result, 'x87-exception', 1n, 0n, 1n, 0n, changed(caller, 0x1001n));
            equal(result.resources.compilations, 1, 'new quota admits the waiting entry');
            pending(result, old, 0x1001n);
            x87(cpu, old);
          },
          { limits: { maxInstructions: 1, maxTranslationsPerRun: 1 } },
        );
      },
    },
    {
      name: 'FPATAN honors all directions on dyadic and subnormal anchors independently of PC',
      async run() {
        const tiny = raw(0x3f9b),
          tinyPredecessor = raw(0x3f9a, (1n << 64n) - 1n);
        await using(compiler, 'd9f3', LONG, async (cpu) => {
          for (const precision of [0, 2, 3])
            for (const rc of [0, 1, 2, 3])
              for (const negative of [0n, sign])
                for (const [ordinate, lower, tinyWhenDown] of [
                  [tiny, tinyPredecessor, false],
                  [minNormal, integerBit - 1n, true],
                ] as const) {
                  const cw = 0x7f | (precision << 8) | (rc << 10);
                  const before = stack(
                    { 0: ONE, 1: negative | ordinate, 2: sentinel },
                    7,
                    cw,
                    0x4700,
                  );
                  cpu.setState(input());
                  seed(cpu, before);
                  const result = await cpu.run({ budget: 1n });
                  equal(result.reason, 'budget', 'one completed angle');
                  equal(result.retired, 1n, 'producer retired');
                  const roundsAway = rc === 0 || rc === (negative ? 1 : 2);
                  const expected = environment(before);
                  expected.statusWord = (expected.statusWord & ~0x200) | 0x20;
                  if (roundsAway) expected.statusWord |= 0x200;
                  else if (tinyWhenDown) expected.statusWord |= 0x10;
                  slot(expected, 0, negative | (roundsAway ? ordinate : lower));
                  pop(expected);
                  x87(cpu, expected);
                }
        });
      },
    },
    {
      name: 'FPATAN gradual and wrapped underflow each use one rounding on two normal operands',
      async run() {
        for (const [cw, answer, status, waits] of [
          [0x37f, 1n, 0x30, false],
          [0x36f, 0x5fc2c000000000000000n, 0x82b0, true],
        ] as const)
          await using(compiler, 'd9f39b', LONG, async (cpu) => {
            const original = stack(
              { 0: raw(0x403f), 1: raw(2, 0xc000000000000000n), 2: sentinel },
              7,
              cw,
            );
            cpu.setState(input());
            seed(cpu, original);
            const r = await cpu.run({ budget: 1n });
            equal(r.reason, 'budget', 'producer completes before a waiting detector');
            equal(r.retired, 1n, 'single publication');
            const expected = environment(original, 0x1000n, undefined, waits ? 0x1f3 : undefined);
            expected.statusWord |= status;
            slot(expected, 0, answer);
            pop(expected);
            x87(cpu, expected);
            const detector = await cpu.run({ budget: 1n });
            equal(
              detector.reason,
              waits ? 'x87-exception' : 'budget',
              'UM governs waiting detector',
            );
            equal(detector.retired, waits ? 0n : 1n, 'WAIT does not retire on #MF');
            if (waits) pending(detector, expected, 0x1002n);
            x87(cpu, expected);
          });
        await using(compiler, 'd9f39b', LONG, async (cpu) => {
          cpu.setState(input());
          seed(cpu, stack({ 0: sign | raw(0x4001), 1: minNormal }, 7, 0x36f));
          const r = await cpu.run({ budget: 2n });
          equal(
            r.reason,
            'budget',
            'negative x reconstructs a normal angle before underflow classification',
          );
          equal(r.retired, 2n, 'no pending condition at WAIT');
          const numeric = cpu.readX87State();
          equal(
            numeric.statusWord & 0x80ff,
            0x20,
            'near π sets PE without operand or result underflow',
          );
          const angle = numeric.registers[0]!;
          equal(
            angle === pi || angle === pi - 1n,
            true,
            'near π stays within its nearest ULP interval',
          );
        });
      },
    },
    {
      name: 'FPATAN honors all imported x87 modes and register-only prefix metadata',
      async run() {
        for (const [i, profile] of profiles.entries()) {
          const execution =
            profile.mode === 'long64'
              ? { ...LONG, fsBase: 0x12340000n }
              : context(profile.mode, profile.code32, profile.stack32);
          const pc =
            profile.mode === 'protected' || profile.mode === 'compatibility' ? 0x10003n : 0x1000n;
          if (execution.mode !== 'long64') execution.segments.cs.base = 0x20000n;
          const linear = execution.mode === 'long64' ? pc : execution.segments.cs.base + pc;
          const code =
            (i % 2 === 0 ? 'f2' : 'f3') +
            '646667' +
            (execution.mode === 'long64' ? '4f' : '') +
            'd9f3';
          await using(
            compiler,
            code,
            execution,
            async (cpu) => {
              const before = input(pc);
              const original = stack({ 0: ONE, 1: sign, 2: sentinel }, i & 7, 0x37f, 0x4700);
              cpu.setState(before);
              seed(cpu, original);
              const actual = await cpu.run({ budget: 1n, repeatBudget: 3n });
              outcome(
                actual,
                'budget',
                1n,
                1n,
                3n,
                0n,
                changed(before, pc + BigInt(code.length / 2)),
              );
              const real = execution.mode === 'real' || execution.mode === 'virtual8086';
              const expected = environment(original, real ? linear : pc);
              expected.statusWord &= ~0x200;
              slot(expected, (i + 1) & 7, sign);
              pop(expected);
              x87(cpu, expected);
              other(cpu);
              noFault(actual);
            },
            { entry: pc, codeBase: linear },
          );
        }
      },
    },
    {
      name: 'FPATAN paging selects exact fetch limits and repairs the selector before #NM or pending x87',
      async run() {
        const end = PC + 0x1000n;
        const successor = new Uint8Array(4096);
        successor[0] = 0xf3;
        await usingPaged(
          compiler,
          {
            bank: { ...BANK, cr0: BANK.cr0 | 8n, efer: BANK.efer | 0x800n },
            spans: [{ physical: 0xc000n, bytes: successor, execute: true }],
          },
          async (cpu) => {
            const old = stack({ 0: ONE, 1: sign }, 7, 0x37e, 0x8081);
            seed(cpu, old);
            for (const [code, reason, vector] of [
              ['d9', 'page-fault', 14],
              ['f0d9', 'page-fault', 14],
              ['f0d9f3', 'invalid-encoding', 6],
              ['f0f3d9', 'unsupported', undefined],
              ['f3f0d9', 'unsupported', undefined],
              ['f0f0', 'unsupported', undefined],
              ['d9f2', 'unsupported', undefined],
              ['d9fe', 'unsupported', undefined],
              ['66'.repeat(13) + 'd9f3', 'device-not-available', 7],
              ['66'.repeat(14) + 'd9', 'invalid-encoding', 13],
            ] as const) {
              const bytes = hex(code),
                entry = end - BigInt(bytes.length);
              cpu.writePhysicalMemory(0xa000n - BigInt(bytes.length), bytes);
              cpu.setState(initial(entry));
              const fault = await cpu.run({ budget: 1n });
              equal(fault.reason, reason, code + ' first refusal');
              equal(fault.retired, 0n, 'no retirement');
              equal(fault.state.rip, entry, 'restart at instruction prefix');
              equal(
                fault.pageFault?.address,
                vector === 14 ? end : undefined,
                'fetch only the reached missing byte',
              );
              if (vector === 14) {
                equal(fault.pageFault?.access, 'execute', 'missing selector is an execute failure');
                equal(fault.pageFault?.instructionPc, entry, 'fetch fault keeps prefix-start RIP');
                equal(fault.pageFault?.errorCode, 16, 'supervisor fetch from an absent page');
                equal(cpu.readPagingState().cr2, end, 'CR2 records the missing selector address');
              }
              x87(cpu, old);
              if (vector === 6 || vector === 13) {
                const event = await cpu.run({ budget: 1n, exceptionDelivery: { budget: 0n } });
                equal(event.reason, 'event-budget', 'selected decode fault staged');
                same(
                  event.state.event,
                  {
                    kind: 'pending',
                    exception: {
                      vector,
                      instructionPc: entry,
                      returnPc: entry,
                      savedFlags: 0x10002n,
                      ...(vector === 13 ? { errorCode: 0 } : {}),
                    },
                  },
                  'decode fault vector and restart',
                );
              }
            }
            cpu.writePhysicalMemory(0x9fffn, Uint8Array.of(0xd9));
            cpu.writePhysicalMemory(0x4008n, little([0xc007n]));
            const entry = end - 1n;
            for (const [cr0, reason] of [
              [BANK.cr0 | 8n, 'device-not-available'],
              [BANK.cr0 | 4n, 'device-not-available'],
              [BANK.cr0, 'x87-exception'],
            ] as const) {
              cpu.setPagingState({ ...BANK, cr0 });
              cpu.setState(initial(entry, {}, 0x10002n));
              const refused = await cpu.run({ budget: 1n });
              equal(
                refused.reason,
                reason,
                'live control and older producer after repairing the page',
              );
              equal(refused.retired, 0n, 'still restartable');
              equal(refused.state.flags, 0x10002n, 'refusal preserves RF');
              if (reason === 'x87-exception') pending(refused, old, entry);
              x87(cpu, old);
            }
            const repaired = stack({ 0: ONE, 1: sign }, 7);
            seed(cpu, repaired);
            const result = await cpu.run({ budget: 1n });
            equal(result.reason, 'budget', 'repaired FPATAN executes');
            equal(result.retired, 1n, 'exactly one retry');
            equal(result.state.rip, entry + 2n, 'selector consumed across the page');
            const expected = environment(repaired, entry);
            slot(expected, 0, sign);
            pop(expected);
            x87(cpu, expected);
          },
        );
      },
    },
    {
      name: 'physical legacy FPATAN preserves the pending NE0 frontier and old producer',
      async run() {
        const execution = resetContext();
        execution.segments.cs = { ...execution.segments.cs, selector: 0, base: 0n };
        const ram = new Uint8Array(0x10000);
        ram.set(hex('d9f39b'), 0x1000);
        const cpu = await physicalCpu(
          compiler,
          ram,
          new Uint8Array(0x10000),
          { cr0: 0x12n, cr2: 0xdeadn, cr3: 0n, cr4: 0x600n, efer: 0n },
          execution,
        );
        try {
          const old = stack({ 0: ONE, 1: sign }, 7, 0x37e, 0x8081);
          seed(cpu, old);
          for (const [cr0, reason] of [
            [0x1an, 'device-not-available'],
            [0x16n, 'device-not-available'],
            [0x12n, 'unsupported'],
            [0x32n, 'x87-exception'],
          ] as const) {
            cpu.setPagingState({ ...cpu.readPagingState(), cr0 });
            cpu.setState(initial(0x1000n, {}, 0x10002n));
            const refused = await cpu.run({ budget: 1n });
            equal(refused.reason, reason, 'live controls determine waiting admission');
            equal(refused.retired, 0n, 'older producer remains restartable');
            equal(refused.state.flags, 0x10002n, 'pre-begin refusal preserves RF');
            equal(cpu.readPagingState().cr2, 0xdeadn, 'no operand fetch');
            if (reason === 'x87-exception') pending(refused, old, 0x1000n);
            else equal(refused.x87Exception, undefined, 'no pending x87 report');
            same(refused.state.event, { kind: 'ready' }, 'report mode does not stage vector 16');
            x87(cpu, old);
          }
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'FPATAN #MF handler resumes the detector and preserves the published producer and pop',
      async run() {
        const execution = snapshot('protected');
        execution.segments.cs = loadedCode(32);
        execution.segments.ss = loadedData(32);
        const address = withGate(execution, 16);
        await usingEvents(
          compiler,
          'd9f3dfe09b90',
          execution,
          async (cpu) => {
            const before = stack({ 0: sign | ONE, 1: 0n, 2: sentinel }, 7, 0x35f);
            cpu.setState(initial(0x1000n, { 4: 0x8000n }, 0x202n));
            seed(cpu, before);
            const producer = await cpu.run({ budget: 1n });
            equal(producer.reason, 'budget', 'unmasked PE still completes the producer');
            equal(producer.retired, 1n, 'producer retired once');
            const expected = environment(before, 0x1000n, undefined, 0x1f3);
            expected.statusWord |= 0x82a0;
            slot(expected, 0, pi);
            pop(expected);
            x87(cpu, expected);
            const observer = await cpu.run({ budget: 1n });
            equal(observer.reason, 'budget', 'FNSTSW is non-waiting');
            equal(
              observer.state.registers[0]! & 0xffffn,
              BigInt(expected.statusWord),
              'FNSTSW observes pending status',
            );
            x87(cpu, expected);
            const report = await cpu.run({ budget: 1n });
            equal(report.reason, 'x87-exception', 'WAIT reports the producer');
            equal(report.retired, 0n, 'checker did not retire');
            pending(report, expected, 0x1004n);
            const staged = await cpu.run({ budget: 1n, exceptionDelivery: { budget: 0n } });
            delivery(staged, 'event-budget', 0n, 0n, 0n);
            same(
              staged.state.event,
              {
                kind: 'pending',
                exception: {
                  vector: 16,
                  instructionPc: 0x1004n,
                  returnPc: 0x1004n,
                  savedFlags: 0x10202n,
                },
              },
              'detector has no error slot and differs from FIP',
            );
            const resumed = await cpu.run({ budget: 3n, exceptionDelivery: { budget: 1n } });
            delivery(resumed, 'budget', 3n, 1n, 0n);
            ready(resumed);
            equal(resumed.state.rip, 0x1005n, 'FNCLEX and IRET precede a single WAIT retry');
            expected.statusWord &= ~0x80ff;
            x87(cpu, expected);
            other(cpu);
            same(
              cpu.readMemory(0x7ff4n, 12),
              image([0x1004n, 8n, 0x10202n], 4),
              'saved detector frame',
            );
          },
          {
            data: [
              { base: 0x4008n, bytes: CODE32 + DATA32 },
              { base: address, bytes: gate(32, 8, 0x2000n, 0), write: false },
              { base: 0x2000n, bytes: 'dbe2cf', execute: true },
              { base: STACK, bytes: FILL },
            ],
          },
        );
      },
    },
  ];
}
