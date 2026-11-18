import { equal, type Scenario } from '../../support/fixture.js';
import { PC, B_PC, STACK, addresses, withFarGate, type GateCase } from './far-gate-fixture.js';

export function farTaskGateScenarios(compiler: WebAssembly.Module): Scenario[] {
  async function refused(test: GateCase, error?: readonly [number, number]) {
    await withFarGate(compiler, test, async (cpu, _next, read) => {
      const before = cpu.readContext(),
        r = await cpu.run({ budget: 1n });
      equal(r.reason, error ? 'protection-fault' : 'unsupported', 'refused transfer');
      if (error) {
        equal(r.protectionFault?.vector, error[0], 'fault vector');
        equal(r.protectionFault?.errorCode, error[1], 'outer or target selector including TI');
      }
      equal(r.state.rip, PC, 'original RIP');
      equal(r.retired, 0n, 'no switch retired');
      equal(cpu.readContext().tr.selector, before.tr.selector, 'unchanged TR');
      equal(read(addresses.oldBusy, 1), 0x8b, 'A stays busy');
      equal(read(addresses.newBusy, 1), test.tssAccess ?? 0x89, 'B descriptor unchanged');
      equal(read(addresses.oldLink, 2), 0x7171, 'unchanged A link');
      equal(read(addresses.newLink, 2), 0x8282, 'unchanged B link');
    });
  }
  return [
    {
      name: 'GDT and LDT task gates delegate CPL3 far CALL and JMP to a DPL0 TSS for both pointer widths',
      async run() {
        for (const source of ['gdt', 'ldt'] as const)
          for (const transfer of ['call', 'jump'] as const)
            for (const word of [true, false])
              for (const indirect of [false, true]) {
                const nested = transfer === 'call';
                await withFarGate(
                  compiler,
                  { source, transfer, word, indirect, target: indirect ? 0x23 : 0x20 },
                  async (cpu, next, read) => {
                    const entered = await cpu.run({ budget: 1n }),
                      current = cpu.readContext();
                    equal(entered.reason, 'budget', 'entered B');
                    equal(entered.retired, 1n, 'one switch');
                    equal(current.cpl, 0, 'B.CS owns incoming CPL');
                    equal(
                      current.tr.selector,
                      indirect ? 0x23 : 0x20,
                      'TR preserves the target RPL',
                    );
                    equal(entered.state.rip, B_PC, 'incoming EIP ignores pointer offset');
                    equal(entered.state.registers[0], 0x22334455n, 'B.EAX');
                    equal(entered.state.registers[4], 0x5000n, 'B.ESP without far frame');
                    equal(read(addresses.savedPc, 4), Number(next), 'A saved post-transfer EIP');
                    equal(read(addresses.oldBusy, 1), nested ? 0x8b : 0x89, 'A busy transition');
                    equal(read(addresses.newBusy, 1), 0x8b, 'B became busy');
                    equal(read(addresses.oldLink, 2), 0x7171, 'A backlink sentinel');
                    equal(read(addresses.newLink, 2), nested ? 0x18 : 0x8282, 'B backlink');
                    equal(Boolean(entered.state.flags & 0x4000n), nested, 'incoming NT');
                    if (nested) {
                      const back = await cpu.run({ budget: 1n });
                      equal(back.reason, 'budget', 'B IRETD');
                      equal(back.retired, 1n, 'one nested return');
                      equal(back.state.rip, next, 'returned to A');
                      equal(cpu.readContext().tr.selector, 0x18, 'A.TR');
                      equal(cpu.readContext().cpl, 3, 'A.CPL');
                      equal(back.state.registers[0], 0x11112222n, 'restored A.EAX');
                      equal(back.state.registers[4], STACK, 'restored A.ESP');
                      equal(read(addresses.newBusy, 1), 0x89, 'IRETD released B');
                      equal(
                        read(addresses.newLink, 2),
                        0x18,
                        'nested return retained the backlink',
                      );
                    }
                  },
                );
              }
      },
    },
    {
      name: 'far task gates check the outer selector before the selected GDT TSS and never substitute RETF for IRET',
      async run() {
        await refused({ source: 'direct', transfer: 'jump' }, [13, 0x20]); // Same DPL0 TSS that the previous ring-3 gate admitted.
        await refused({ source: 'direct', transfer: 'jump', cpl: 0, outer: 0x23 }, [13, 0x20]);
        await refused({ source: 'ldt', transfer: 'jump', noLdt: true }, [13, 0x0c]);
        await refused({ source: 'gdt', transfer: 'jump', outer: 0x53 }, [13, 0x50]);
        await refused({ source: 'gdt', transfer: 'jump', tssAccess: 0x81, tssLimit: 0x2b });
        for (const source of ['gdt', 'ldt'] as const) {
          const outer = source === 'gdt' ? 0x38 : 0x0c;
          const rows: [Partial<GateCase>, number, number][] = [
            [{ gateAccess: 0x05, target: 0xf0 }, 13, outer], // DPL before presence and target.
            [{ gateAccess: 0x85, cpl: 0 }, 13, outer], // Outer RPL alone denies the caller.
            [{ gateAccess: 0x65, target: 0xf0, noTr: true }, 11, outer], // Before inner lookup and outgoing TR.
            [{ target: 0 }, 13, 0],
            [{ target: 0x0f }, 13, 0x0c],
            [{ target: 0x93 }, 13, 0x90],
            [{ target: 0x13 }, 13, 0x10],
            [{ tssAccess: 0x0b }, 13, 0x20],
            [{ tssAccess: 0x09 }, 11, 0x20],
            [{ tssLimit: 0x66 }, 10, 0x20],
            [{ cpl: 0, transfer: 'ret' }, 13, outer],
          ];
          for (const [options, vector, error] of rows)
            await refused({ source, transfer: 'jump', ...options }, [vector, error]);
        }
      },
    },
    {
      name: 'far task selector, privilege and presence checks precede use of the outgoing TR',
      async run() {
        await refused({ source: 'direct', transfer: 'jump', noTr: true }, [13, 0x20]);
        await refused(
          { source: 'direct', transfer: 'jump', noTr: true, cpl: 0, tssAccess: 0x09 },
          [11, 0x20],
        );
        for (const source of ['gdt', 'ldt'] as const) {
          await refused({ source, transfer: 'call', target: 0x0f, noTr: true }, [13, 0x0c]);
          await refused({ source, transfer: 'jump', tssAccess: 0x09, noTr: true }, [11, 0x20]);
          await refused({ source, transfer: 'jump', noTr: true }, [10, 0]);
        }
      },
    },
    {
      name: 'fixed IA32e and private-linear profiles keep their legacy far task frontiers',
      async run() {
        for (const source of ['direct', 'gdt', 'ldt'] as const) {
          const outer = { direct: 0x20, gdt: 0x38, ldt: 0x0c }[source];
          await refused({ model: 'ia32e-paged', source, transfer: 'jump', indirect: true }, [
            13,
            outer,
          ]);
          await refused({ model: 'linear', source, transfer: 'jump', cpl: 0 });
        }
        for (const source of ['gdt', 'ldt'] as const) {
          await refused({
            model: 'linear',
            source,
            transfer: 'call',
            gateAccess: 0x05,
            target: 0xf0,
          });
        }
      },
    },
  ];
}
