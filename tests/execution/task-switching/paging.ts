import { preserveNumeric } from '../far-control/fixture.js';
import {
  ENTRY_B,
  PD_A,
  TSS_B,
  dword,
  equal,
  same,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from './fixture.js';

export function taskPagingScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: '32-bit task switch with PG clear retains live CR3 and preserves upper and numeric banks',
      async run() {
        await withGuest(compiler, async (cpu) => {
          await toCaller(cpu);
          cpu.setPagingState({ ...cpu.readPagingState(), cr0: 0x33n });
          write32(cpu, TSS_B + 84n, 0x10);
          const unchangedNumeric = preserveNumeric(cpu);
          const state = cpu.readState();
          const registers = state.registers.map((low, i) => (BigInt(i + 1) << 40n) | low);
          cpu.setState({ ...state, registers });
          const switched = await cpu.run({ budget: 1n });
          equal(switched.reason, 'budget', 'completed CALL with paging disabled');
          equal(switched.retired, 1n, 'one committed transfer');
          equal(switched.state.rip, ENTRY_B, 'B.EIP');
          equal(cpu.readPagingState().cr3, PD_A, 'PG0 retains outgoing PDBR');
          equal(cpu.readPagingState().cr0, 0x3bn, 'TS set without changing PG or PE');
          equal(dword(cpu, TSS_B + 28n), 0x21000, 'static incoming CR3 image intact');
          equal(
            switched.state.registers[0],
            0x10055667788n,
            'new low EAX retains old raw high bits',
          );
          same(
            switched.state.registers.slice(8),
            registers.slice(8),
            'banks outside the legacy image',
          );
          unchangedNumeric();
        });
      },
    },
    {
      name: '32-bit tasks reload a changed valid PAE quartet, refuse reserved PDPTEs and retain an unchanged root',
      async run() {
        for (const target of ['changed', 'unchanged', 'reserved'] as const)
          await withGuest(compiler, async (cpu) => {
            await toCaller(cpu);
            // First 2 MiB identity-mapped except linear 0x80000. Both tasks share the TSS/GDT pages.
            for (const [pdpt, pd, pt, physical, immediate] of [
              [0x28000, 0x29000, 0x2b000, 0x2d000, 0x11223344],
              [0x28020, 0x2a000, 0x2c000, 0x2e000, 0xaabbccdd],
            ] as const) {
              write32(cpu, BigInt(pdpt), pd | 1);
              write32(cpu, BigInt(pd), pt | 3);
              const bytes = new Uint8Array(4096),
                entries = new DataView(bytes.buffer);
              for (let page = 0; page < 512; ++page)
                entries.setUint32(page * 8, (page === 0x80 ? physical : page << 12) | 3, true);
              cpu.writePhysicalMemory(BigInt(pt), bytes);
              cpu.writePhysicalMemory(BigInt(physical), Uint8Array.of(0xb8));
              write32(cpu, BigInt(physical + 1), immediate);
            }
            cpu.setPagingState({
              ...cpu.readPagingState(),
              cr4: 0x20n,
              cr3: 0x28000n,
              pdpte: [0x29001n, 0n, 0n, 0n],
            });
            const unchanged = target === 'unchanged';
            const incomingRoot = unchanged ? 0x28000 : 0x28020;
            if (unchanged) write32(cpu, 0x28000n, 0x29003); // poison memory after the live quartet was captured
            if (target === 'reserved') write32(cpu, 0x28020n, 0x2a003);
            write32(cpu, TSS_B + 28n, incomingRoot);
            write32(cpu, TSS_B + 32n, 0x80000);
            write32(cpu, TSS_B + 84n, 0x10);
            const transferred = await cpu.run({ budget: 1n });
            if (target === 'reserved') {
              equal(
                transferred.reason,
                'protection-fault',
                'changed PAE root rejects reserved bit',
              );
              equal(transferred.protectionFault?.vector, 13, 'reserved PDPTE #GP');
              equal(transferred.protectionFault?.errorCode, 0, 'PDPTE fault has zero error');
              equal(cpu.readPagingState().cr3, 0x28000n, 'old valid root retained');
              same(
                cpu.readPagingState().pdpte,
                [0x29001n, 0n, 0n, 0n],
                'reserved quartet never becomes live',
              );
              return;
            }
            equal(transferred.retired, 1n, 'one completed transfer');
            equal(transferred.state.rip, 0x80000n, 'target fetch remains separate');
            same(
              cpu.readPagingState().pdpte,
              [unchanged ? 0x29001n : 0x2a001n, 0n, 0n, 0n],
              'valid PDPTEs from the selected root',
            );
            equal(
              cpu.readPagingState().cr3,
              BigInt(incomingRoot),
              'selected PDPT in same physical page',
            );
            const executed = await cpu.run({ budget: 1n });
            equal(executed.reason, 'budget', 'B executed first instruction');
            equal(
              executed.state.registers[0],
              unchanged ? 0x11223344n : 0xaabbccddn,
              'instruction bytes at same linear PC',
            );
          });
      },
    },
  ];
}
