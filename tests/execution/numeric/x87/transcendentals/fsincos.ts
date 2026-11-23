import type { Cpu, X87State } from '../../../../../sdk/src/index.js';
import {
  CODE32,
  DATA32,
  FILL,
  STACK,
  delivery,
  gate,
  image,
  loadedCode,
  loadedData,
  snapshot,
  using as usingEvents,
  withGate,
} from '../../../exception-delivery/fixture.js';
import { BANK, PC, initial, little, same, usingPaged } from '../../../memory/paging/fixture.js';
import { physicalCpu, resetContext } from '../../../reset/fixture.js';
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
  profiles,
  raw,
  seed,
  slot,
  stack,
  using,
  x87,
  type Scenario,
} from '../fixture.js';

const SIGN = 1n << 79n;
const HALF = raw(0x3ffe);
const BELOW_ONE = raw(0x3ffe, 0xffffffffffffffffn);
const MIN_NORMAL = raw(1);
const C1 = 0x200,
  C2 = 0x400,
  PE = 0x20,
  UE = 0x10,
  DE = 2,
  IE = 1,
  SF = 0x40;
const PENDING = 0x8080;
const SENTINEL = 0x4007cafe12345678abcdn;
const HALF_SINE = 0x3ffdf57743a2582f7f44n;
const HALF_COSINE = 0x3ffee0a94032dbea7ceen;

// Expected physical changes at the opcode boundary. Unspecified slots and sticky flags stay live.
function produced(
  before: X87State,
  pc: bigint,
  status: number,
  sine?: bigint,
  cosine = sine,
): X87State {
  const expected = environment(before, pc, undefined, status & PENDING ? 0x1fb : undefined);
  expected.statusWord = (before.statusWord & ~(C1 | C2)) | status;
  if (sine !== undefined && cosine !== undefined) {
    const top = (before.statusWord >> 11) & 7;
    const pushed = (top + 7) & 7;
    slot(expected, top, sine);
    slot(expected, pushed, cosine);
    expected.statusWord = (expected.statusWord & ~0x3800) | (pushed << 11);
  }
  return expected;
}

async function onePair(cpu: Cpu, before: X87State, status: number, sine?: bigint, cosine = sine) {
  const caller = input();
  cpu.setState(caller);
  seed(cpu, before);
  const result = await cpu.run({ budget: 1n });
  outcome(result, 'budget', 1n, 1n, 1n, 0n, changed(caller, 0x1002n));
  noFault(result);
  x87(cpu, produced(before, caller.rip, status, sine, cosine));
  other(cpu);
}

export function fsincosScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'FSINCOS publishes exactly two physical slots or suppresses the pair using live tags and masks',
      async run() {
        const quiet = 0xffffc000000000000123n;
        const signaling = 0xffff8000000000000456n;
        const cutoff = raw(0x403e);
        const cases = [
          [SIGN, false, 0x37f, 0, SIGN, ONE],
          [quiet, false, 0x37e, 0, quiet],
          [signaling, false, 0x37f, IE, signaling | (1n << 62n)],
          [signaling, false, 0x37e, PENDING | IE],
          [raw(0x7fff), false, 0x37f, IE, INDEFINITE],
          [raw(0x7fff) | SIGN, false, 0x37e, PENDING | IE],
          [raw(0x4000, 1n), false, 0x37f, IE, INDEFINITE],
          [raw(0x4000, 1n), false, 0x37e, PENDING | IE],
          [1n, false, 0x37d, PENDING | DE],
          [1n << 63n, false, 0x37d, PENDING | DE],
          [undefined, false, 0x37f, IE | SF, INDEFINITE],
          [undefined, true, 0x37f, IE | SF, INDEFINITE],
          [undefined, true, 0x37e, PENDING | IE | SF],
          [HALF, true, 0x37f, IE | SF | C1, INDEFINITE],
          [HALF, true, 0x37e, PENDING | IE | SF | C1],
          [quiet, true, 0x37f, IE | SF | C1, INDEFINITE],
          [1n, true, 0x37d, IE | SF | C1, INDEFINITE],
          [cutoff, true, 0x37f, IE | SF | C1, INDEFINITE],
          [cutoff | SIGN, true, 0x37e, PENDING | IE | SF | C1],
          [cutoff, false, 0x37f, C2],
          [cutoff | SIGN, false, 0x37f, C2],
          [cutoff | 1n, false, 0x37f, C2],
          [cutoff | SIGN | 1n, false, 0x37f, C2],
        ] as const;
        await using(compiler, '90d9fb9b', LONG, async (cpu) => {
          cpu.setState(input());
          const warm = await cpu.run({ budget: 1n });
          equal(warm.state.rip, 0x1001n, 'predecessor leaves retained middle entry');
          equal(warm.resources.compilations, 1, 'one cold artifact');
          for (const [i, [source, occupied, cw, flags, sine, cosine]] of cases.entries())
            for (const oldC2 of [0, C2]) {
              const values: Record<number, bigint> = { 2: SENTINEL, 4: 0n, 5: quiet };
              if (source !== undefined) values[0] = source;
              if (occupied) values[7] = ONE;
              const before = stack(values, (7 + i) & 7, cw, 0x4304 | oldC2);
              const caller = input(0x1001n);
              cpu.setState(caller);
              seed(cpu, before);
              const result = await cpu.run({ budget: 1n });
              outcome(result, 'budget', 1n, 1n, 1n, 0n, changed(caller, 0x1003n));
              equal(
                result.resources.compilations,
                0,
                'retained code uses new CW, TOP, tags and values',
              );
              const expected = produced(before, caller.rip, flags, sine, cosine);
              x87(cpu, expected);
              other(cpu);
              if (flags & PENDING) {
                const wait = await cpu.run({ budget: 1n });
                equal(
                  wait.reason,
                  'x87-exception',
                  'suppressed pair reports through separate FWAIT',
                );
                equal(wait.retired, 0n, 'waiting detector does not retire');
                pending(wait, expected, 0x1003n);
                x87(cpu, expected);
              }
            }
        });
      },
    },
    {
      name: 'FSINCOS takes C1 from sine and honors independent signs, RC and binary80 precision',
      async run() {
        await using(compiler, 'd9fb', LONG, async (cpu) => {
          // Independent MPFR anchors from specs/fsincos/verification/numeric-reference.py.
          for (const [angle, sine, cosine, flags] of [
            [raw(0x3ffb), 0x3ffaffd557776a76d5a6n, 0x3ffeff800aaa4fa69a65n, PE | C1],
            [raw(0x3ffd), 0x3ffcfd5776a798abb5d4n, 0x3ffef80aa4fbef750ba8n, PE],
          ] as const)
            for (const sign of [0n, SIGN])
              for (const pc of [0, 0x200, 0x300])
                await onePair(
                  cpu,
                  stack({ 0: angle | sign }, 0, 0x7f | pc, 0x4704),
                  flags,
                  sine | sign,
                  cosine,
                );

          const axis = 0x3fffc90fdaa22168c235n;
          const nearZeroCosine = 0xbfbe8000000000000000n;
          for (const sign of [0n, SIGN])
            for (let rc = 0; rc < 4; rc++) {
              const increment = rc === 0 || rc === (sign ? 1 : 2);
              await onePair(
                cpu,
                stack({ 0: axis | sign }, 2, 0x37f | (rc << 10), 0x4704),
                PE | (increment ? C1 : 0),
                (increment ? ONE : BELOW_ONE) | sign,
                nearZeroCosine + (rc === 1 ? 1n : 0n),
              );
            }
        });
      },
    },
    {
      name: 'FSINCOS keeps cosine normal across gradual, wrapped and suppressed tiny sine outcomes',
      async run() {
        await using(compiler, 'd9fb', LONG, async (cpu) => {
          for (const sign of [0n, SIGN])
            for (const [value, extra, gradual, wrapped] of [
              [1n, DE, 0n, 0x5fc1ffffffffffffffffn],
              [MIN_NORMAL, 0, MIN_NORMAL - (1n << 64n) - 1n, 0x6000ffffffffffffffffn],
              [1n << 63n, DE, MIN_NORMAL - (1n << 64n) - 1n, 0x6000ffffffffffffffffn],
            ] as const) {
              const inward = sign ? 2 : 1;
              await onePair(
                cpu,
                stack({ 0: value | sign }, 3, 0x37f | (inward << 10)),
                extra | UE | PE,
                gradual | sign,
                sign ? ONE : BELOW_ONE,
              );
              await onePair(
                cpu,
                stack({ 0: value | sign }, 3, 0x36f | (inward << 10)),
                PENDING | extra | UE | PE,
                wrapped | sign,
                sign ? ONE : BELOW_ONE,
              );
              const normal = value === 1n ? 1n : MIN_NORMAL;
              await onePair(
                cpu,
                stack({ 0: value | sign }, 3, 0x37f),
                extra | PE | C1 | (value === 1n ? UE : 0),
                normal | sign,
                ONE,
              );
            }
        });
      },
    },
    {
      name: 'FSINCOS fixed register encoding saves prefix-start FIP in every imported mode',
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
            'd9fb';
          await using(
            compiler,
            code,
            execution,
            async (cpu) => {
              const caller = input(pc, { 3: 0x0000800000000000n });
              const before = stack({ 0: SIGN, 2: SENTINEL }, i & 7, 0x37f, 0x4700);
              cpu.setState(caller);
              seed(cpu, before);
              const zero = await cpu.run({ budget: 0n });
              outcome(zero, 'budget', 0n, 0n, 0n, 0n, caller);
              x87(cpu, before);
              const result = await cpu.run({ budget: 1n });
              outcome(
                result,
                'budget',
                1n,
                1n,
                1n,
                0n,
                changed(caller, pc + BigInt(code.length / 2)),
              );
              const saved =
                execution.mode === 'real' || execution.mode === 'virtual8086' ? linear : pc;
              x87(cpu, produced(before, saved, 0, SIGN, ONE));
              other(cpu);
            },
            { entry: pc, codeBase: linear },
          );
        }
      },
    },
    {
      name: 'FSINCOS reached LOCK and page fetch frontiers precede numeric admission and retain RF',
      async run() {
        const end = PC + 0x1000n;
        const tail = new Uint8Array(4096);
        tail[0] = 0xfb;
        const bank = { ...BANK, cr0: BANK.cr0 | 8n, efer: BANK.efer | 0x800n };
        await usingPaged(
          compiler,
          { bank, spans: [{ physical: 0xc000n, bytes: tail, execute: true }] },
          async (cpu) => {
            const before = stack({ 0: HALF }, 0, 0x37e, PENDING | C2 | IE);
            seed(cpu, before);
            for (const [code, reason, vector] of [
              ['d9', 'page-fault', 14],
              ['f0d9', 'page-fault', 14],
              ['f0d9fb', 'invalid-encoding', 6],
              ['f0f3d9', 'unsupported', 0],
              ['f3f0d9', 'unsupported', 0],
              ['f0f0', 'unsupported', 0],
              ['66'.repeat(12) + 'f0d9fb', 'invalid-encoding', 6],
              ['66'.repeat(13) + 'd9fb', 'device-not-available', 7],
              ['66'.repeat(14) + 'd9', 'invalid-encoding', 13],
            ] as const) {
              const bytes = hex(code);
              const entry = end - BigInt(bytes.length);
              cpu.writePhysicalMemory(0xa000n - BigInt(bytes.length), bytes);
              const caller = initial(entry, {}, 0x10002n);
              cpu.setState(caller);
              const result = await cpu.run({ budget: 1n });
              outcome(result, reason, 1n, 0n, 1n, 0n, caller);
              if (vector === 14) {
                same(
                  result.pageFault,
                  {
                    vector: 14,
                    instructionPc: entry,
                    address: end,
                    errorCode: 16,
                    access: 'execute',
                  },
                  'reached selector fetch',
                );
                equal(cpu.readPagingState().cr2, end, 'CR2');
              } else if (vector === 6 || vector === 13) {
                const staged = await cpu.run({ budget: 1n, exceptionDelivery: { budget: 0n } });
                same(
                  staged.state.event,
                  {
                    kind: 'pending',
                    exception: {
                      vector,
                      instructionPc: entry,
                      returnPc: entry,
                      savedFlags: caller.flags,
                      ...(vector === 13 ? { errorCode: 0 } : {}),
                    },
                  },
                  'exact staged invalid instruction',
                );
              }
              x87(cpu, before);
            }
            cpu.writePhysicalMemory(0x9fffn, hex('d9'));
            cpu.writePhysicalMemory(0x4008n, little([0xc007n]));
            const at = end - 1n;
            const caller = initial(at, {}, 0x10002n);
            for (const [cr0, reason] of [
              [BANK.cr0 | 8n, 'device-not-available'],
              [BANK.cr0 | 4n, 'device-not-available'],
              [BANK.cr0, 'x87-exception'],
            ] as const) {
              cpu.setState(caller);
              cpu.setPagingState({ ...bank, cr0 });
              const result = await cpu.run({ budget: 1n });
              outcome(result, reason, 1n, 0n, 1n, 0n, caller);
              if (reason === 'x87-exception') pending(result, before, at);
              x87(cpu, before);
            }
            const repaired = stack({ 0: SIGN });
            seed(cpu, repaired);
            const result = await cpu.run({ budget: 1n });
            outcome(result, 'budget', 1n, 1n, 1n, 0n, changed(caller, at + 2n, {}, 2n));
            x87(cpu, produced(repaired, at, 0, SIGN, ONE));
          },
        );
      },
    },
    {
      name: 'physical FSINCOS retains the NE=0 frontier and publishes before a single-step trap',
      async run() {
        const execution = resetContext();
        Object.assign(execution.segments.cs, { selector: 0, base: 0n });
        const ram = new Uint8Array(0x10000);
        ram.set(hex('d9fb9b'), 0x1000);
        const bank = { cr0: 0x12n, cr2: 0xdeadn, cr3: 0n, cr4: 0n, efer: 0n };
        const cpu = await physicalCpu(compiler, ram, new Uint8Array(0x10000), bank, execution);
        try {
          const old = stack({ 0: SIGN }, 4, 0x37e, PENDING | C2 | IE);
          seed(cpu, old);
          const at = 0x1000n;
          const caller = initial(at, {}, 0x10002n);
          cpu.setState(caller);
          const pendingNe0 = await cpu.run({ budget: 1n });
          outcome(pendingNe0, 'unsupported', 1n, 0n, 1n, 0n, caller);
          equal(pendingNe0.x87Exception, undefined, 'private NE=0 path has no detector report');
          x87(cpu, old);

          const before = stack({ 0: SIGN });
          seed(cpu, before);
          cpu.setDebugState({ addresses: [at, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
          cpu.setState(initial(at));
          const breakpoint = await cpu.run({ budget: 1n });
          equal(breakpoint.reason, 'debug-exception', 'execute breakpoint at FSINCOS');
          equal(breakpoint.retired, 0n, 'execute breakpoint precedes publication');
          x87(cpu, before);
          cpu.setState(initial(at, {}, 0x10102n));
          const stepped = await cpu.run({ budget: 1n });
          equal(stepped.reason, 'debug-exception', 'RF bypasses breakpoint but retains TF');
          equal(stepped.retired, 1n, 'single-step follows FSINCOS publication');
          equal(stepped.debugException?.cause, 'single-step', 'TF provenance');
          equal(stepped.debugException?.resumePc, at + 2n, 'next FWAIT remains separate');
          x87(cpu, produced(before, at, 0, SIGN, ONE));
          other(cpu);
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'FSINCOS publishes pending precision before FNSTSW and the #MF handler retries only FWAIT',
      async run() {
        const execution = snapshot('protected');
        execution.segments.cs = loadedCode(32);
        execution.segments.ss = loadedData(32);
        const gateAddress = withGate(execution, 16);
        await usingEvents(
          compiler,
          'd9fbdfe09b90',
          execution,
          async (cpu) => {
            const before = stack({ 0: HALF, 2: SENTINEL }, 0, 0x35f);
            seed(cpu, before);
            cpu.setState(initial(0x1000n, { 4: 0x8000n }, 0x202n));
            const result = await cpu.run({ budget: 1n });
            equal(result.reason, 'budget', 'unmasked PE still publishes both slots');
            equal(result.retired, 1n, 'producer retired once');
            const expected = produced(before, 0x1000n, PENDING | PE | C1, HALF_SINE, HALF_COSINE);
            x87(cpu, expected);
            cpu.setX87State(cpu.readX87State());
            const observer = await cpu.run({ budget: 1n });
            equal(observer.retired, 1n, 'FNSTSW bypasses pending PE');
            equal(
              observer.state.registers[0]! & 0xffffn,
              BigInt(expected.statusWord),
              'observer reads pending status',
            );
            const report = await cpu.run({ budget: 1n });
            equal(report.retired, 0n, 'FWAIT was not retired');
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
              'detector, not producer, has the no-error-code frame',
            );
            const resumed = await cpu.run({ budget: 3n, exceptionDelivery: { budget: 1n } });
            delivery(resumed, 'budget', 3n, 1n, 0n);
            equal(resumed.state.rip, 0x1005n, 'FNCLEX, IRET, FWAIT retire');
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
              { base: gateAddress, bytes: gate(32, 8, 0x2000n, 0), write: false },
              { base: 0x2000n, bytes: 'dbe2cf', execute: true },
              { base: STACK, bytes: FILL },
            ],
          },
        );
      },
    },
  ];
}
