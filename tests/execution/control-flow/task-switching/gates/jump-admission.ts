import { hex } from '../../../../support/fixture.js';
import {
  CALL,
  ENTRY_B,
  GDT,
  PD_A,
  TSS_A,
  TSS_B,
  check,
  dword,
  equal,
  same,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from '../fixture.js';

export function taskJumpAdmissionScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'far JMP admits both TSS busy writes before publishing either one or the outgoing save',
      async run() {
        for (const descriptor of [0x18, 0x20])
          await withGuest(compiler, async (cpu) => {
            await toCaller(cpu);
            cpu.writePhysicalMemory(CALL, Uint8Array.of(0xea));
            write32(cpu, TSS_B + 84n, 0x10);
            const denied = Number(GDT) + descriptor + 5;
            const span = (offset: number, length: number, write = true) => ({
              base: BigInt(offset),
              backing: 'ram',
              offset,
              length,
              permissions: { read: true, write, execute: true },
            });
            cpu.setMappings([
              span(0, denied),
              span(denied, 1, false),
              span(denied + 1, 0x110000 - denied - 1),
            ]);
            const before = cpu.readState(),
              context = cpu.readContext(),
              clock = cpu.readTimeState();
            const descriptors = cpu.readPhysicalMemory(GDT + 0x18n, 16),
              images = cpu.readPhysicalMemory(TSS_A, 208);
            for (let attempt = 0; attempt < 2; attempt++) {
              const result = await cpu.run({ budget: 1n });
              equal(result.reason, 'physical-memory-fault', 'busy update denied');
              equal(
                result.physicalMemoryFault?.failingPhysicalAddress,
                BigInt(denied),
                'selected access byte',
              );
              equal(result.retired, 0n, 'no debit');
              same(result.state, before, 'outgoing state unchanged');
              same(cpu.readContext(), context, 'outgoing hidden caches unchanged');
              equal(cpu.readPagingState().cr3, PD_A, 'outgoing root retained');
              same(cpu.readTimeState(), clock, 'outgoing clock retained');
              same(cpu.readPhysicalMemory(GDT + 0x18n, 16), descriptors, 'both busy bits retained');
              same(cpu.readPhysicalMemory(TSS_A, 208), images, 'neither TSS changed');
            }
            cpu.setMappings([span(0, 0x110000)]);
            const repaired = await cpu.run({ budget: 2n, stop: ENTRY_B });
            equal(repaired.reason, 'stop', 'repair reaches B');
            equal(repaired.retired, 1n, 'one completed far JMP');
            equal(repaired.remaining, 1n, 'B did not consume the remaining budget');
            equal(cpu.readContext().tr.selector, 0x20, 'B owns the repaired transfer');
            equal(cpu.readPhysicalMemory(GDT + 0x1dn, 1)[0], 0x89, 'A released');
            equal(cpu.readPhysicalMemory(GDT + 0x25n, 1)[0], 0x8b, 'B acquired');
          });
      },
    },
    {
      name: 'late incoming DS fault after JMP belongs to B with A already available',
      async run() {
        await withGuest(compiler, async (cpu) => {
          await toCaller(cpu);
          cpu.writePhysicalMemory(CALL, Uint8Array.of(0xea)); // B starts with a faulty DS selector.
          write32(cpu, TSS_B, 0xbadc7117);
          const clock = cpu.readTimeState().tsc;
          const zero = await cpu.run({ budget: 0n });
          equal(zero.reason, 'budget', 'no allowance for JMP');
          equal(zero.retired, 0n, 'zero work');
          equal(cpu.readContext().tr.selector, 0x18, 'A remains current');
          const staged = await cpu.run({ budget: 3n, eventDelivery: { budget: 0n } });
          equal(staged.reason, 'event-budget', 'selector fault pending in B');
          equal(staged.retired, 1n, 'only JMP was charged');
          equal(cpu.readTimeState().tsc, clock + 1n, 'one switch tick');
          equal(cpu.readContext().tr.selector, 0x20, 'B current');
          equal(staged.state.rip, ENTRY_B, 'B EIP');
          check(staged.state.event.kind === 'pending', 'incoming selector fault');
          const exception = staged.state.event.exception;
          equal(exception.vector, 10, 'invalid incoming DS raises #TS');
          equal(exception.errorCode, 0x38, 'B.DS selector with EXT clear');
          equal(exception.returnPc, ENTRY_B, 'guest fault resumes B');
          equal(dword(cpu, TSS_A + 32n), Number(CALL + 7n), 'A saved post-JMP EIP');
          equal(cpu.readPhysicalMemory(GDT + 0x1dn, 1)[0], 0x89, 'A now available');
          equal(cpu.readPhysicalMemory(GDT + 0x25n, 1)[0], 0x8b, 'B now busy');
          equal(dword(cpu, TSS_B), 0xbadc7117, 'no backlink write');
        });
      },
    },
    {
      name: 'a far JMP pointer split across pages faults before its read watch and combines B0 with incoming BT on repair',
      async run() {
        await withGuest(compiler, async (cpu) => {
          await toCaller(cpu);
          // m16:16 begins at 0x77ffe; the entire selector is on a separately mapped page.
          cpu.writePhysicalMemory(CALL, hex('66ff2dfe7f0700'));
          cpu.writePhysicalMemory(0x77ffen, hex('efbe'));
          cpu.writePhysicalMemory(0x7d000n, hex('2000'));
          const pte = 0x26000n + 0x78n * 4n;
          write32(cpu, pte, 0);
          write32(cpu, TSS_B + 84n, 0x10);
          write32(cpu, TSS_B, 0xbeef7117);
          cpu.writePhysicalMemory(TSS_B + 100n, Uint8Array.of(1));
          cpu.setDebugState({ addresses: [0x78000n, 0n, 0n, 0n], dr6: 0n, dr7: 0x70403n });
          const refused = await cpu.run({ budget: 1n });
          equal(refused.reason, 'page-fault', 'unmapped selector halfword');
          equal(refused.pageFault?.address, 0x78000n, 'first missing pointer byte');
          equal(refused.pageFault?.access, 'read', 'selector read');
          equal(refused.pageFault?.errorCode, 0, 'nonpresent supervisor read');
          equal(refused.retired, 0n, 'faulting jump not retired');
          equal(refused.state.rip, CALL, 'outgoing pointer owner');
          equal(cpu.readContext().tr.selector, 0x18, 'A is still current');
          equal(cpu.readDebugState().dr6 & 0x800fn, 0n, 'no read or task observation yet');
          write32(cpu, pte, 0x7d003);
          const trap = await cpu.run({ budget: 1n });
          equal(trap.reason, 'debug-exception', 'one combined debug trap');
          equal(trap.retired, 1n, 'JMP completed');
          equal(
            cpu.readDebugState().dr6 & 0xe00fn,
            0x8001n,
            'selector read and task T both reported',
          );
          equal(cpu.readDebugState().dr7 & 3n, 2n, 'local watch cleared, global watch retained');
          equal(trap.debugException?.cause, 'task-switch', 'incoming task cause');
          equal(trap.debugException?.breakpoints, 1, 'far pointer selected slot 0');
          equal(trap.debugException?.resumePc, ENTRY_B, 'B resumes before its first instruction');
          equal(trap.debugException?.resumeFlags, 2n, 'B is non-nested');
          equal(dword(cpu, TSS_A + 32n), Number(CALL + 7n), 'saved full 16:16 indirect JMP length');
          equal(dword(cpu, TSS_B), 0xbeef7117, 'no backlink on the repaired JMP');
        });
      },
    },
  ];
}
