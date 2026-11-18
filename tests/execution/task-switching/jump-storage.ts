import {
  CALL,
  ENTRY_B,
  GDT,
  PD_B,
  RESUME_A,
  TSS_A,
  TSS_B,
  dword,
  equal,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from './fixture.js';

export function taskJumpStorageScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'task JMP captures an overlapping incoming TSS before saving through the outgoing TR cache',
      async run() {
        await withGuest(compiler, async (cpu) => {
          await toCaller(cpu);
          const initial = cpu.readState();
          const shared = cpu.readPhysicalMemory(TSS_B, 104);
          cpu.writePhysicalMemory(TSS_A, shared);
          write32(cpu, TSS_A, 0xa55a7117);
          write32(cpu, TSS_A + 36n, 0x4002);
          write32(cpu, TSS_A + 84n, 0x10);

          // B overlaps the cached A image. A's current GDT descriptor points somewhere else.
          const unused = 0x60000n;
          for (const [selector, base] of [
            [0x18n, unused],
            [0x20n, TSS_A],
          ] as const) {
            const descriptor = cpu.readPhysicalMemory(GDT + selector, 8);
            descriptor[2] = Number(base & 0xffn);
            descriptor[3] = Number((base >> 8n) & 0xffn);
            descriptor[4] = Number(base >> 16n);
            cpu.writePhysicalMemory(GDT + selector, descriptor);
          }
          cpu.writePhysicalMemory(CALL, Uint8Array.of(0xea));
          write32(cpu, unused + 32n, 0xabcd7654);
          const span = (offset: number, length: number) => ({
            base: BigInt(offset),
            offset,
            length,
            backing: 'ram',
            permissions: { read: true, write: true, execute: true },
          });
          // Both tasks' backlink slots are wholly inaccessible during this switch.
          const hole = Number(TSS_A);
          cpu.setMappings([span(0, hole), span(hole + 4, 0x110000 - hole - 4)]);
          const result = await cpu.run({ budget: 1n });
          equal(result.reason, 'budget', 'JMP completed despite the missing backlink');
          equal(result.retired, 1n, 'one transfer');
          equal(result.state.rip, ENTRY_B, 'captured B.EIP before storing A.EIP');
          equal(result.state.flags & 0x4000n, 0x4000n, 'captured B.NT');
          equal(result.state.registers[0], 0x55667788n, 'captured B.EAX');
          equal(cpu.readPagingState().cr3, PD_B, 'captured B page root');
          equal(cpu.readContext().tr.base, TSS_A, 'new TR addresses the shared image');
          equal(dword(cpu, TSS_A + 32n), Number(RESUME_A), 'post-JMP EIP saved through cached A');
          equal(
            dword(cpu, TSS_A + 40n),
            Number(initial.registers[0]! & 0xffffffffn),
            'old EAX saved',
          );
          equal(
            dword(cpu, unused + 32n),
            0xabcd7654,
            'current GDT A base did not redirect the save',
          );
          equal(cpu.readPhysicalMemory(GDT + 0x1dn, 1)[0], 0x89, 'live A descriptor released');
          equal(cpu.readPhysicalMemory(GDT + 0x25n, 1)[0], 0x8b, 'live B descriptor acquired');
          cpu.setMappings([span(0, 0x110000)]);
          equal(dword(cpu, TSS_A), 0xa55a7117, 'shared backlink and its neighbor intact');
        });
      },
    },
  ];
}
