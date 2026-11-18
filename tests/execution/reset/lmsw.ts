import { changed, equal, exit, hex, initial, type Scenario } from '../../support/fixture.js';
import { gate, snapshot } from '../exception-delivery/fixture.js';
import { LONG, using as linear } from '../modes/fixture.js';
import { BANK, little, PC as LONG_PC, usingPaged } from '../paging/fixture.js';
import { fixture, PC, preserved, same, using, word } from '../supervisor-access/fixture.js';
import { ONE, pending, seed, stack, x87 } from '../x87/fixture.js';
import { createCpu, physicalCpu, resetContext } from './fixture.js';

export function lmswScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'LMSW writes the low control bits and keeps PE set in every control-bank mode',
      async run() {
        for (const [family, bits] of [
          ['non-pae', 16],
          ['non-pae', 32],
          ['pae', 32],
          ['ia32e', 16],
          ['ia32e', 32],
          ['ia32e', 64],
          ['fixed', 64],
        ] as const) {
          const code = bits === 64 ? 'f34d0f01f4' : 'f2660f01f0';
          const f = fixture(side, family, code, { bits });
          f.paging.cr0 |= 0x50000n;
          await using(side, f, async (cpu) => {
            const context = cpu.readContext(),
              numeric = preserved(cpu);
            for (const [source, cr0] of [
              [0x0000n, 0x80050031n],
              [0xfffen, 0x8005003fn],
              [0xfff2n, 0x80050033n],
              [0xfff4n, 0x80050035n],
              [0xfff8n, 0x80050039n],
            ] as const) {
              const before = initial(
                PC,
                { [bits === 64 ? 12 : 0]: 0xfeedbeef01230000n | source },
                0x408d7n,
              );
              cpu.setPagingState(f.paging);
              cpu.setState(before);
              exit(
                await cpu.run({ budget: 1n }),
                'budget',
                1n,
                1n,
                changed(before, PC + BigInt(code.length / 2)),
              );
              same(cpu.readPagingState(), { ...f.paging, cr0 }, 'only selected CR0 bits changed');
              same(cpu.readContext(), context, 'loaded mode and segment caches preserved');
              numeric();
            }
          });
        }
      },
    },
    {
      name: 'LMSW privilege refusal precedes memory and debug effects while CPL0 can read odd m16',
      async run() {
        for (const mode of ['long64', 'virtual8086'] as const) {
          const f = fixture(
            side,
            mode === 'long64' ? 'ia32e' : 'non-pae',
            mode === 'long64' ? '0f0133' : '0f0137',
            { cpl: 3 },
          );
          if (mode === 'virtual8086') f.options.context = snapshot(mode);
          f.paging.cr0 |= 0x40000n;
          f.leaf(0x6000, 0n);
          await using(side, f, async (cpu) => {
            const before = initial(PC, { 3: 0x6001n }, 0x408d7n);
            cpu.setState(before);
            const report = await cpu.run({ budget: 1n });
            exit(report, 'protection-fault', 1n, 0n, before);
            equal(report.protectionFault?.vector, 13, 'privilege vector');
            equal(report.protectionFault?.phase, 'privilege', 'privilege before memory formation');
            equal(report.protectionFault?.errorCode, 0, 'privilege error code');
            equal(report.pageFault, undefined, 'absent source not accessed');
            same(cpu.readPagingState(), f.paging, 'controls and CR2 preserved');
          });
        }

        const f = fixture(side, 'ia32e', '6764480f0133');
        f.paging.cr0 |= 0x40000n;
        f.leaf(0x6000, 0n);
        f.leaf(0x4000, 0x9003n);
        f.leaf(0x5000, 0n);
        f.bytes.set(hex('08f0'), 0x9ffe);
        await using(side, f, async (cpu) => {
          const context = cpu.readContext();
          context.segments.fs.base = 0x1000n;
          cpu.setContext(context);
          const before = initial(PC, { 3: 0x1111000000003ffen }, 0x408d7n);
          cpu.setState(before);
          exit(await cpu.run({ budget: 1n }), 'budget', 1n, 1n, changed(before, PC + 6n));
          same(
            cpu.readPagingState(),
            { ...f.paging, cr0: 0x80040039n },
            'FS, address32, fixed m16 span',
          );
          equal(word(cpu, f.pt + 4 * f.width), 0x9023n, 'source read set A without D');
          equal(word(cpu, f.pt + 5 * f.width), 0n, 'wider source would fault');
          same(cpu.readPhysicalMemory(0x9ffen, 2), hex('08f0'), 'source bytes untouched');

          cpu.writePhysicalMemory(0x9001n, hex('0200'));
          cpu.setDebugState({ addresses: [0x4001n, 0n, 0n, 0n], dr6: 0n, dr7: 0x30401n });
          cpu.setState(initial(PC, { 3: 0x3001n }, 0x408d7n));
          const odd = await cpu.run({ budget: 1n });
          equal(odd.retired, 1n, 'odd supervisor word is admitted with AM and AC set');
          equal(odd.reason, 'debug-exception', 'operand read produces a completion trap');
          equal(odd.debugException?.cause, 'data-breakpoint', 'read watchpoint');
          equal(odd.debugException?.resumePc, PC + 6n, 'debug sees the completed instruction');
          equal(odd.debugException?.breakpoints, 1, 'one matching slot');
          equal(cpu.readPagingState().cr0, 0x80040033n, 'odd source MP value');
        });
      },
    },
    {
      name: 'LMSW split data read stages a precise PF and a real handler retries it exactly once',
      async run() {
        const f = fixture(side, 'non-pae', '0f013390');
        f.leaf(0x4000, 0x9003n);
        f.leaf(0x5000, 0n);
        f.leaf(0xa000, BigInt(f.pt) | 0x63n);
        f.bytes[0x9fff] = 4;
        f.bytes[0xe000] = 0x7a;
        f.bytes.set(hex(gate(32, 8, 0x1800n, 0)), 0xc000 + 14 * 8);
        f.bytes.set(hex('c70514a0000003e0000083c404cf'), 0x8800);
        await using(side, f, async (cpu) => {
          const before = initial(PC, { 3: 0x4fffn, 4: 0x9ff0n }, 0x8d7n);
          cpu.setState(before);
          const numeric = preserved(cpu);
          const report = await cpu.run({ budget: 1n });
          exit(report, 'page-fault', 1n, 0n, before);
          same(
            report.pageFault,
            { vector: 14, instructionPc: PC, address: 0x5000n, errorCode: 0, access: 'read' },
            'second source byte PF',
          );
          same(cpu.readPagingState(), { ...f.paging, cr2: 0x5000n }, 'only CR2 updated on failure');
          equal(word(cpu, f.pt + 16, 4), 0x9023n, 'successful first translation is accessed');
          equal(word(cpu, f.pt + 20, 4), 0n, 'missing second translation untouched');

          const staged = await cpu.run({ budget: 1n, exceptionDelivery: { budget: 0n } });
          equal(staged.reason, 'event-budget', 'architectural access fault staged');
          equal(staged.retired, 0n, 'no fault retirement');
          same(
            staged.state.event,
            {
              kind: 'pending',
              exception: {
                vector: 14,
                instructionPc: PC,
                returnPc: PC,
                savedFlags: before.flags | 0x10000n,
                errorCode: 0,
                pageFaultAddress: 0x5000n,
              },
            },
            'PF saved boundary',
          );
          const resume = await cpu.run({ budget: 4n, exceptionDelivery: { budget: 1n } });
          exit(resume, 'budget', 4n, 4n, changed(before, PC + 3n));
          equal(resume.deliveryAttempts, 1n, 'one actual handler entry');
          equal(cpu.readTimeState().tsc, 4n, 'handler and retried LMSW retire once');
          equal(cpu.readPagingState().cr0, 0x80000035n, 'retried write published EM');
          equal(word(cpu, f.pt + 20, 4), 0xe023n, 'repaired source page read without D');
          equal(word(cpu, 0x9fff, 1), 4n, 'first input unchanged');
          equal(word(cpu, 0xe000, 1), 0x7an, 'second input unchanged');
          numeric();
        });
      },
    },
    {
      name: 'LMSW preserves captured PAE roots and retained data across same-value and numeric-only writes',
      async run() {
        for (const [source, cr0] of [
          [2n, 0x80000033n],
          [8n, 0x80000039n],
        ] as const) {
          const f = fixture(side, 'pae', '8a13c70530a00000676001000f01f08a0b0f013b8a0b');
          f.bytes.set(little([0x20n]), 0x1000);
          f.leaf(0xa000, BigInt(f.pt) | 0x63n);
          f.bytes[0x16000] = 0x5a;
          await using(side, f, async (cpu) => {
            cpu.setState(initial(PC, { 0: source, 3: 0x6000n }));
            const first = await cpu.run({ budget: 2n });
            equal(first.state.registers[2]! & 255n, 0x12n, 'old retained read');
            equal(first.retired, 2n, 'read and guest PTE write completed');
            equal(word(cpu, f.pt + 6 * 8), 0x16067n, 'replacement PTE in memory');
            const write = await cpu.run({ budget: 1n });
            equal(write.retired, 1n, 'unchanged or numeric-only write publishes');
            equal(write.state.rip, PC + 15n, 'serializing boundary');
            same(
              cpu.readPagingState(),
              { ...f.paging, cr0 },
              'captured quartet survives backing poison',
            );
            const old = await cpu.run({ budget: 1n });
            equal(old.state.registers[1]! & 255n, 0x12n, 'data TLB unaffected by LMSW');
            const updated = await cpu.run({ budget: 2n });
            equal(updated.retired, 2n, 'INVLPG and new read complete');
            equal(
              updated.state.registers[1]! & 255n,
              0x5an,
              'subsequent targeted invalidation exposes new PTE',
            );
          });
        }
      },
    },
    {
      name: 'LMSW can change live numeric availability without consuming a pending x87 producer',
      async run() {
        const f = fixture(side, 'ia32e', '0f01f0d9d00f01f3d9d0');
        await using(side, f, async (cpu) => {
          const old = stack({ 0: ONE }, 6, 0x37e, 0x8081);
          seed(cpu, old);
          const before = initial(PC, { 0: 8n, 3: 0n }, 0x8d7n);
          cpu.setState(before);
          const fault = await cpu.run({ budget: 2n });
          exit(fault, 'device-not-available', 2n, 1n, changed(before, PC + 3n));
          equal(fault.deviceNotAvailable?.instructionPc, PC + 3n, 'successor uses changed TS');
          equal(cpu.readPagingState().cr0, 0x80000039n, 'TS is visible');
          x87(cpu, old);
          cpu.setState(changed(before, PC + 5n));
          const released = await cpu.run({ budget: 2n });
          equal(
            released.reason,
            'x87-exception',
            'unmasked producer remains pending after TS clears',
          );
          equal(released.retired, 1n, 'only the second LMSW retired');
          pending(released, old, PC + 8n);
          equal(cpu.readPagingState().cr0, 0x80000031n, 'PE remained set');
          x87(cpu, old);
        });
      },
    },
    {
      name: 'LMSW setting PE retires before a missing successor with the old loaded CS retained',
      async run() {
        const context = resetContext();
        Object.assign(context.segments.cs, { selector: 0x100, base: 0x1000n });
        const ram = new Uint8Array(0x1003);
        ram.set(hex('0f01f0'), 0x1000);
        const cpu = await physicalCpu(
          compiler,
          ram,
          new Uint8Array(0x10000),
          { cr0: 0x10n, cr2: 0xdeadn, cr3: 0n, cr4: 0n, efer: 0n },
          context,
        );
        try {
          const before = initial(0n, { 0: 1n });
          cpu.setState(before);
          const missing = await cpu.run({ budget: 2n });
          exit(missing, 'physical-memory-fault', 2n, 1n, changed(before, 3n));
          equal(missing.physicalMemoryFault?.instructionPc, 3n, 'missing instruction follows LMSW');
          equal(
            missing.physicalMemoryFault?.physicalAddress,
            0x1003n,
            'old CS base forms successor',
          );
          equal(missing.physicalMemoryFault?.access, 'execute', 'reached successor was a fetch');
          equal(cpu.readPagingState().cr0, 0x11n, 'completed PE retained');
          equal(cpu.readPagingState().cr2, 0xdeadn, 'private fetch does not change CR2');
          const now = cpu.readContext();
          equal(now.mode, 'protected', 'completed transition retained');
          same(now.segments.cs, context.segments.cs, 'old loaded code cache');
        } finally {
          cpu.dispose();
        }
      },
    },
    {
      name: 'LMSW required group and address bytes precede LOCK and profile refusal',
      async run() {
        const end = LONG_PC + 0x1000n;
        await usingPaged(compiler, { bank: { ...BANK, efer: BANK.efer | 0x800n } }, async (cpu) => {
          for (const [bytes, reason, vector, retired] of [
            ['f00f01', 'page-fault', 14, 0n],
            ['f00f0134', 'page-fault', 14, 0n],
            ['f00f013425006000', 'page-fault', 14, 0n],
            ['f00f01342500600000', 'invalid-encoding', 6, 0n],
            ['f0f30f01', 'unsupported', 0, 0n],
            ['f2f00f01', 'unsupported', 0, 0n],
            ['f0f0', 'unsupported', 0, 0n],
            ['66'.repeat(12) + '0f01f0', 'budget', 0, 1n],
            ['66'.repeat(13) + '0f01', 'invalid-encoding', 13, 0n],
          ] as const) {
            const code = hex(bytes),
              entry = end - BigInt(code.length);
            cpu.writePhysicalMemory(0xa000n - BigInt(code.length), code);
            const before = initial(entry, { 0: 2n });
            cpu.setState(before);
            const result = await cpu.run({ budget: 1n });
            exit(result, reason, 1n, retired, retired ? changed(before, end) : before);
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
                'exact unavailable byte',
              );
              equal(cpu.readPagingState().cr2, end, 'CR2 names first unavailable byte');
            } else if (vector !== 0) {
              const staged = await cpu.run({ budget: 1n, exceptionDelivery: { budget: 0n } });
              equal(staged.reason, 'event-budget', 'admitted decode fault staged');
              equal(
                staged.state.event.kind === 'pending' && staged.state.event.exception.vector,
                vector,
                'LOCK UD or 16-byte GP',
              );
              equal(
                staged.state.event.kind === 'pending' && staged.state.event.exception.errorCode,
                vector === 13 ? 0 : undefined,
                'decode error code',
              );
            }
          }
        });
        for (const code of ['0f01f0', '0f0134', 'f00f0134', '0f01e0'])
          await linear(compiler, code, LONG, async (cpu) => {
            const before = initial();
            cpu.setState(before);
            exit(await cpu.run({ budget: 1n }), 'unsupported', 1n, 0n, before);
          });
      },
    },
  ];
}
