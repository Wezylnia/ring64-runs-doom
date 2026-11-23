import {
  CALL,
  ENTRY_B,
  ENTRY_C,
  GDT,
  PD_A,
  TSS_A,
  TSS_B,
  TSS_C,
  check,
  dword,
  equal,
  same,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from '../fixture.js';

export function taskEntryFaultScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'an incoming CPL3 task error push preserves paging priority and automatic #AC EXT',
      async run() {
        for (const stackPresent of [false, true]) {
          await withGuest(compiler, async (cpu) => {
            await toCaller(cpu);
            const ctx = cpu.readContext();
            const gdt = 0x77000n;
            cpu.writePhysicalMemory(gdt, cpu.readPhysicalMemory(GDT, 48));
            const user = cpu.readPhysicalMemory(GDT + 8n, 16);
            user[5] = 0xfb; // present DPL3 code, readable and accessed
            user[13] = 0xf3; // present DPL3 data, writable and accessed
            cpu.writePhysicalMemory(gdt + 48n, user);
            cpu.setContext({ ...ctx, gdtr: { base: gdt, limit: 63 } });
            write32(cpu, TSS_B + 84n, 0x48); // #TS in B selects C via the original IDT gate.
            for (const offset of [72, 76, 80, 84, 88, 92])
              write32(cpu, TSS_C + BigInt(offset), offset === 76 ? 0x33 : 0x3b);
            write32(cpu, TSS_C + 36n, 0x40002); // AC
            write32(cpu, TSS_C + 56n, 0x9bfff);
            write32(cpu, PD_A, 0x26007);
            write32(cpu, 0x26000n + 0x9bn * 4n, stackPresent ? 0x9b007 : 0);
            const bank = cpu.readPagingState();
            cpu.setPagingState({ ...bank, cr2: 0xabcden, cr0: bank.cr0 | 0x40000n });
            const result = await cpu.run({ budget: 1n, eventDelivery: { budget: 1n } });
            equal(result.reason, 'event-budget', 'secondary fault staged in C');
            equal(result.retired, 1n, 'outgoing CALL retired once');
            equal(result.deliveryAttempts, 1n, 'one #TS entry attempt');
            equal(cpu.readContext().cpl, 3, 'incoming stack uses user permissions');
            equal(result.state.registers[4], 0x9bfffn, 'failed push retains C.ESP');
            check(result.state.event.kind === 'pending', 'incoming event');
            const fault = result.state.event.exception;
            equal(fault.vector, stackPresent ? 17 : 14, 'translation precedes alignment');
            equal(fault.instructionPc, ENTRY_C, 'fault belongs to C');
            equal(fault.returnPc, ENTRY_C, 'handler resumes C');
            equal(fault.errorCode, stackPresent ? 1 : 6, 'automatic EXT or user nonpresent write');
            equal(fault.pageFaultAddress, stackPresent ? undefined : 0x9bffbn, 'fault slot');
            equal(cpu.readPagingState().cr2, stackPresent ? 0xabcden : 0x9bffbn, 'CR2');
            equal(dword(cpu, 0x9bffbn), 0, 'original #TS error code was never written');
          });
        }
      },
    },
    {
      name: 'precommit task-gate failure preserves both PCs from an accepted single-step trap',
      async run() {
        await withGuest(compiler, async (cpu) => {
          await toCaller(cpu);
          const idt = cpu.readContext().idtr.base;
          cpu.writePhysicalMemory(idt + 8n, cpu.readPhysicalMemory(idt + 80n, 8));
          cpu.writePhysicalMemory(GDT + 0x2dn, Uint8Array.of(0x09)); // absent C TSS descriptor
          cpu.writePhysicalMemory(CALL, Uint8Array.of(0x90));
          const state = cpu.readState();
          cpu.setState({ ...state, flags: state.flags | 0x100n });
          const result = await cpu.run({ budget: 1n, eventDelivery: { budget: 1n } });
          equal(result.reason, 'event-budget', 'failed #DB delivery');
          equal(result.retired, 1n, 'only NOP retired');
          equal(cpu.readContext().tr.selector, 0x18, 'A remains current');
          check(result.state.event.kind === 'pending', 'secondary #NP');
          const fault = result.state.event.exception;
          equal(fault.vector, 11, 'C absent');
          equal(fault.errorCode, 0x29, 'TSS selector with EXT');
          equal(fault.instructionPc, CALL, 'completed single-step producer');
          equal(fault.returnPc, CALL + 1n, 'old task resumes after NOP');
          equal(fault.savedFlags, state.flags | 0x10100n, 'original TF plus saved RF');
        });
      },
    },
    {
      name: 'late task selector faults promote #GP to #DF in C and shut down in B during #DF entry',
      async run() {
        await withGuest(compiler, async (cpu) => {
          await toCaller(cpu);
          const ctx = cpu.readContext();
          const idt = 0x77000n;
          const gate = cpu.readPhysicalMemory(ctx.idtr.base + 80n, 8);
          cpu.writePhysicalMemory(idt + 13n * 8n, gate); // #GP selects C
          gate[2] = 0x20;
          cpu.writePhysicalMemory(idt + 8n * 8n, gate); // #DF selects B
          cpu.setContext({ ...ctx, idtr: { base: idt, limit: 14 * 8 - 1 } });
          write32(cpu, TSS_C + 36n, 0x47);
          write32(cpu, TSS_C + 84n, 0x38); // B already has this invalid DS.
          cpu.setState({
            ...cpu.readState(),
            flags: 0x202n,
            event: {
              kind: 'pending',
              exception: {
                vector: 13,
                instructionPc: CALL,
                returnPc: CALL,
                savedFlags: 0x10202n,
                errorCode: 0x30,
              },
            },
          });
          const clock = cpu.readTimeState().tsc;
          const allowance = { budget: 1n, eventDelivery: { budget: 1n } };
          const first = await cpu.run(allowance);
          equal(first.reason, 'event-budget', 'one #GP task entry attempted');
          equal(first.deliveryAttempts, 1n, '#GP consumed once');
          equal(first.retired, 0n, 'no guest instruction retired');
          equal(cpu.readContext().tr.selector, 0x28, 'C is current');
          equal(first.state.rip, ENTRY_C, 'C entry PC');
          equal(first.state.flags, 0x4047n, 'nested C flags');
          same(
            first.state.event,
            {
              kind: 'pending',
              exception: {
                vector: 8,
                instructionPc: ENTRY_C,
                returnPc: ENTRY_C,
                savedFlags: 0x14047n,
                errorCode: 0,
              },
            },
            'double fault belongs to C',
          );
          equal(dword(cpu, TSS_A + 32n), Number(CALL), 'A saved the source PC');
          equal(dword(cpu, TSS_A + 36n), 0x10202, 'A saved the source fault flags');
          equal(dword(cpu, TSS_C), 0x18, 'C links to A');

          write32(cpu, TSS_A + 32n, 0xaabbccdd);
          const second = await cpu.run(allowance);
          equal(second.reason, 'shutdown', '#TS during #DF entry');
          equal(second.deliveryAttempts, 1n, 'only the pending #DF attempted');
          equal(second.retired, 0n, 'no handler instruction retired');
          equal(cpu.readContext().tr.selector, 0x20, 'B is current at shutdown');
          equal(second.state.rip, ENTRY_B, 'B entry PC');
          same(second.state.event, { kind: 'shutdown' }, 'no further pending exception');
          equal(dword(cpu, TSS_B), 0x28, 'B links to C');
          equal(dword(cpu, TSS_C + 32n), Number(ENTRY_C), 'C saved its own entry PC');
          equal(dword(cpu, TSS_C + 36n), 0x14047, 'C saved the #DF flags');
          equal(dword(cpu, TSS_A + 32n), 0xaabbccdd, 'A save was not replayed');

          write32(cpu, TSS_B, 0x55667788);
          const stopped = await cpu.run(allowance);
          equal(stopped.reason, 'shutdown', 'shutdown persists');
          equal(stopped.deliveryAttempts, 0n, 'shutdown consumes no delivery');
          equal(stopped.retired, 0n, 'shutdown consumes no instruction');
          equal(dword(cpu, TSS_B), 0x55667788, 'B backlink was not replayed');
          equal(cpu.readTimeState().tsc, clock, 'fault deliveries consumed no guest ticks');
        });
      },
    },
  ];
}
