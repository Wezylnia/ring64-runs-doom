import { BANK, initial, little, PC, same, usingPaged } from '../../memory/paging/fixture.js';
import { physicalCpu, resetContext } from '../../processor/reset/fixture.js';
import {
  changed,
  context,
  environment,
  equal,
  hex,
  input,
  LONG,
  ONE,
  other,
  outcome,
  pending,
  profiles,
  seed,
  stack,
  using,
  x87,
  type Scenario,
} from './fixture.js';

export function fsetpmScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'DB E4 preserves a ready or pending x87 bank at a retained middle entry in every imported mode',
      async run() {
        for (const [index, profile] of profiles.entries()) {
          const execution =
            profile.mode === 'long64'
              ? LONG
              : context(profile.mode, profile.code32, profile.stack32);
          const prefix =
            (index % 2 === 0 ? 'f2' : 'f3') + '646667' + (profile.mode === 'long64' ? '4f' : '');
          const next = 0x1001n + BigInt(prefix.length / 2 + 2);
          await using(compiler, '90' + prefix + 'dbe4d9d0', execution, async (cpu) => {
            cpu.setState(input());
            const warm = await cpu.run({ budget: 1n });
            equal(warm.state.rip, 0x1001n, 'predecessor retires');
            for (const pendingIE of [false, true]) {
              const before = input(0x1001n);
              const old = stack(
                { 0: ONE, 1: 0n },
                index & 7,
                pendingIE ? 0x37e : 0x37f,
                pendingIE ? 0x8081 : 0x4000,
              );
              cpu.setState(before);
              seed(cpu, old);
              const zero = await cpu.run({ budget: 0n });
              outcome(zero, 'budget', 0n, 0n, 0n, 0n, before);
              const stop = await cpu.run({ budget: 1n, stop: before.rip });
              outcome(stop, 'stop', 1n, 0n, 1n, 0n, before);
              x87(cpu, old);
              const result = await cpu.run({ budget: 1n });
              outcome(result, 'budget', 1n, 1n, 1n, 0n, changed(before, next));
              equal(result.resources.compilations, 0, 'x87 state did not replace the warmed code');
              equal(result.x87Exception, undefined, 'no wait at the compatibility opcode');
              x87(cpu, old);
              other(cpu);
              const nextOp = await cpu.run({ budget: 1n });
              if (pendingIE) {
                equal(nextOp.reason, 'x87-exception', 'ordinary FNOP still waits');
                equal(nextOp.retired, 0n, 'ordinary FNOP suppressed');
                pending(nextOp, old, next);
                x87(cpu, old);
              } else {
                equal(nextOp.retired, 1n, 'ready FNOP retires');
                x87(cpu, environment(old, next));
              }
            }
          });
        }
      },
    },
    {
      name: 'DB E4 fetch decoding precedes availability and a repaired selector leaves pending x87 intact',
      async run() {
        const end = PC + 0x1000n;
        const selector = new Uint8Array(4096);
        selector.set(hex('e4d9d0'));
        const bank = { ...BANK, cr0: BANK.cr0 | 8n, efer: BANK.efer | 0x800n };
        await usingPaged(
          compiler,
          { bank, spans: [{ physical: 0xc000n, bytes: selector, execute: true }] },
          async (cpu) => {
            const old = stack({ 0: ONE, 1: 0n }, 7, 0x37e, 0x8081);
            seed(cpu, old);
            for (const [code, reason, vector] of [
              ['db', 'page-fault', 14],
              ['f0db', 'page-fault', 14],
              ['f0dbe4', 'invalid-encoding', 6],
              ['f0f3db', 'unsupported', 0],
              ['f3f0db', 'unsupported', 0],
              ['f0f0', 'unsupported', 0],
              ['dbe0', 'unsupported', 0],
              ['dbe1', 'unsupported', 0],
              ['66'.repeat(13) + 'dbe4', 'device-not-available', 7],
              ['66'.repeat(14) + 'db', 'invalid-encoding', 13],
            ] as const) {
              const bytes = hex(code),
                entry = end - BigInt(bytes.length);
              cpu.writePhysicalMemory(0xa000n - BigInt(bytes.length), bytes);
              const before = initial(entry, {}, 0x10002n);
              cpu.setState(before);
              const result = await cpu.run({ budget: 1n });
              equal(result.reason, reason, code + ' first refusal');
              equal(result.retired, 0n, 'no retirement');
              equal(result.state.rip, entry, 'restart at instruction prefix');
              equal(result.state.flags, before.flags, 'pre-begin refusal preserves RF');
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
                  'only reached missing selector',
                );
                equal(cpu.readPagingState().cr2, end, 'missing byte in CR2');
              } else if (vector === 6 || vector === 13 || vector === 7) {
                const event = await cpu.run({ budget: 1n, exceptionDelivery: { budget: 0n } });
                equal(event.reason, 'event-budget', 'architectural refusal selected');
                same(
                  event.state.event,
                  {
                    kind: 'pending',
                    exception: {
                      vector,
                      instructionPc: entry,
                      returnPc: entry,
                      savedFlags: before.flags,
                      ...(vector === 13 ? { errorCode: 0 } : {}),
                      ...(vector === 7
                        ? {
                            deviceNotAvailable: {
                              vector: 7,
                              instructionPc: entry,
                              family: 'x87',
                              controls: {
                                mp: true,
                                em: false,
                                ts: true,
                                osfxsr: true,
                                osxmmexcpt: true,
                              },
                            },
                          }
                        : {}),
                    },
                  },
                  'staged prefix-start trap',
                );
              }
              x87(cpu, old);
              other(cpu);
            }
            cpu.writePhysicalMemory(0x9fffn, hex('db'));
            cpu.writePhysicalMemory(0x4008n, little([0xc007n]));
            const before = initial(end - 1n, {}, 0x10002n);
            for (const value of [4n, 8n]) {
              cpu.setPagingState({ ...bank, cr0: BANK.cr0 | value });
              cpu.setState(before);
              const fault = await cpu.run({ budget: 1n });
              equal(fault.reason, 'device-not-available', 'EM or TS gates a repaired instruction');
              equal(fault.retired, 0n, 'resumable numeric refusal');
              equal(fault.deviceNotAvailable?.instructionPc, end - 1n, 'numeric prefix start');
              equal(fault.deviceNotAvailable?.family, 'x87', 'x87 availability owner');
              equal(fault.state.flags, before.flags, 'numeric admission preserves RF');
              x87(cpu, old);
            }
            cpu.setPagingState({ ...bank, cr0: BANK.cr0 });
            const repaired = await cpu.run({ budget: 1n });
            equal(repaired.reason, 'budget', 'repaired enabled DB E4 bypasses pending');
            equal(repaired.retired, 1n, 'repair executes once');
            equal(repaired.state.rip, end + 1n, 'DB E4 spans the page');
            equal(repaired.state.flags, 2n, 'successful begin consumed RF');
            x87(cpu, old);
            const later = await cpu.run({ budget: 1n });
            pending(later, old, end + 1n);
            x87(cpu, old);
          },
        );
      },
    },
    {
      name: 'DB E4 stays independent of a following FWAIT and retires before its own single-step trap',
      async run() {
        const context = resetContext();
        Object.assign(context.segments.cs, { selector: 0, base: 0n });
        const ram = new Uint8Array(0x10000);
        ram.set(hex('dbe49bdbe4'), 0x1000);
        const bank = { cr0: 0x10n, cr2: 0n, cr3: 0n, cr4: 0n, efer: 0n };
        const cpu = await physicalCpu(compiler, ram, new Uint8Array(0x10000), bank, context);
        try {
          const old = stack({ 0: ONE, 1: 0n }, 4, 0x37e, 0x8081);
          seed(cpu, old);
          cpu.setState(input(0x1000n, {}, 0x10102n));
          const traced = await cpu.run({ budget: 1n });
          equal(traced.reason, 'debug-exception', 'trap after no-op');
          equal(traced.retired, 1n, 'no-op retired before TF trap');
          equal(traced.state.rip, 0x1002n, 'the FWAIT boundary is next');
          equal(traced.debugException?.cause, 'single-step', 'ordinary single-step provenance');
          equal(traced.debugException?.resumePc, 0x1002n, 'FWAIT resume PC');
          x87(cpu, old);
          for (const [cr0, reason] of [
            [0x3an, 'device-not-available'],
            [0x38n, 'x87-exception'],
            [0x18n, 'unsupported'],
          ] as const) {
            cpu.setPagingState({ ...bank, cr0 });
            cpu.setState(input(0x1002n, {}, 2n));
            const wait = await cpu.run({ budget: 1n });
            equal(wait.reason, reason, 'FWAIT uses its own MP/TS and NE policy');
            equal(wait.retired, 0n, 'separate WAIT did not retire');
            if (reason === 'x87-exception') pending(wait, old, 0x1002n);
            x87(cpu, old);
          }
          cpu.setPagingState({ ...bank, cr0: 0x30n });
          cpu.setState(input(0x1003n, {}, 2n));
          equal(
            (await cpu.run({ budget: 1n })).retired,
            1n,
            'explicitly reached bare pair retires',
          );
          x87(cpu, old);
          other(cpu);
        } finally {
          cpu.dispose();
        }
      },
    },
  ];
}
