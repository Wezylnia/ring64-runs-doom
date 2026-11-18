import {
  CALL,
  ENTRY_C,
  GDT,
  PD_A,
  TSS_B,
  TSS_C,
  check,
  dword,
  equal,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from './fixture.js';

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
  ];
}
