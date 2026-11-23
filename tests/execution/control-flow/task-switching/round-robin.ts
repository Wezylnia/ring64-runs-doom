import type { Cpu } from '../../../../sdk/src/index.js';
import { bytes, hex } from '../far/fixture.js';
import {
  CALL,
  ENTRY_B,
  ENTRY_C,
  GDT,
  PAGE_A,
  PAGE_B,
  PD_A,
  PD_B,
  RESUME_A,
  TSS_A,
  TSS_B,
  TSS_C,
  dword,
  equal,
  markers,
  same,
  toCaller,
  withGuest,
  write32,
  type Scenario,
} from './fixture.js';

const entry = 0x400100n;
type Route = 'direct' | 'GDT' | 'LDT';

// Each root maps the same program, counter and far-pointer addresses to its own page.
function schedulerCode(peer: number, taskA: boolean): Uint8Array {
  const leg = (delta: number, marker: string) =>
    '830580004000' +
    bytes(BigInt(delta), 1) +
    'b0' +
    bytes(BigInt(marker.charCodeAt(0)), 1) +
    'e6e9';
  return hex(
    (taskA ? 'bb01c0a1a1' : 'bb02c0b2b2') + // mov ebx, task-specific sentinel
      leg(taskA ? 1 : 16, taskA ? 'A' : 'B') +
      'ff2d90004000' + // jmp far [0x400090]
      leg(taskA ? 2 : 32, taskA ? 'C' : 'D') +
      'eadec0adde' +
      bytes(BigInt(peer), 2) + // jmp peer:0xdeadc0de
      leg(4, 'E') +
      'ebfe',
  );
}

function installGates(cpu: Cpu, route: Route): { gdt: bigint; setup: string } {
  if (route === 'direct') return { gdt: GDT, setup: '' };
  const gdt = 0x84000n,
    ldt = 0x85000n;
  cpu.writePhysicalMemory(gdt, cpu.readPhysicalMemory(GDT, 0x30));
  const first = route === 'GDT' ? gdt + 0x30n : ldt + 8n;
  cpu.writePhysicalMemory(first, hex('00002000008500000000180000850000')); // task gates to B and A
  cpu.writePhysicalMemory(0x83ff0n, hex('4f0000400800')); // GDTR
  let setup = '0f0115f03f0800'; // lgdt [0x83ff0]
  if (route === 'LDT') {
    cpu.writePhysicalMemory(gdt + 0x40n, hex('1700005008820000')); // LDT 0x85000, limit 0x17
    write32(cpu, TSS_A + 96n, 0x40);
    write32(cpu, TSS_B + 96n, 0x40);
    setup += '66b840000f00d0'; // mov ax,0x40; lldt ax
  }
  return { gdt, setup };
}

export function roundRobinScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'far JMP schedules protected tasks through direct TSS, GDT and LDT gates across private page roots',
      async run() {
        for (const [route, ntA, ntB] of [
          ['direct', 0, 0],
          ['direct', 1, 1],
          ['GDT', 0, 1],
          ['LDT', 1, 0],
        ] as const)
          await withGuest(compiler, async (cpu, ports) => {
            await toCaller(cpu);
            const { gdt, setup } = installGates(cpu, route);
            const selectorB = { direct: 0x20, GDT: 0x30, LDT: 0x0c }[route];
            const selectorA = { direct: 0x18, GDT: 0x38, LDT: 0x14 }[route];
            for (const [page, peer, initial] of [
              [PAGE_A, selectorB, 40],
              [PAGE_B, selectorA, 70],
            ] as const) {
              cpu.writePhysicalMemory(page + 0x100n, schedulerCode(peer, page === PAGE_A));
              write32(cpu, page + 0x80n, initial);
              write32(cpu, page + 0x90n, 0xdeadc0de);
              write32(cpu, page + 0x94n, peer);
            }
            write32(cpu, TSS_A, 0xa5a57117);
            write32(cpu, TSS_B, 0x5a5a8228);
            write32(cpu, TSS_B + 32n, Number(entry));
            write32(cpu, TSS_B + 36n, (ntB << 14) | 2);
            write32(cpu, TSS_B + 84n, 0x10);
            const stub = setup + '68' + bytes(BigInt((ntA << 14) | 2), 4) + '9d'; // push flags; popfd
            cpu.writePhysicalMemory(
              CALL,
              hex(stub + 'e9' + bytes(entry - CALL - BigInt(stub.length / 2) - 5n, 4)),
            );

            // Both tasks yield at +16 and +33, first through memory, then through an immediate.
            for (const [sourceOffset, length, intoB, incomingOffset] of [
              [16n, 6n, true, 0n],
              [16n, 6n, false, 22n],
              [33n, 7n, true, 22n],
              [33n, 7n, false, 40n],
            ] as const) {
              const before = await cpu.run({ budget: 32n, stop: entry + sourceOffset });
              equal(before.reason, 'stop', 'source reached its next yield');
              const result = await cpu.run({ budget: 1n });
              equal(result.reason, 'budget', 'switch used the last instruction');
              equal(result.retired, 1n, 'one far JMP');
              equal(
                result.state.rip,
                entry + incomingOffset,
                'incoming task resumes its own program',
              );
              equal(cpu.readContext().tr.selector, intoB ? 0x20 : 0x18, 'new TR');
              equal(cpu.readPagingState().cr3, intoB ? PD_B : PD_A, 'new page root');
              equal(cpu.readPagingState().cr0 & 8n, 8n, 'task switched flag');
              equal(result.state.flags & 0x4000n, BigInt((intoB ? ntB : ntA) << 14), 'incoming NT');
              equal(result.state.registers[4], intoB ? 0x98000n : 0x90000n, 'incoming stack');
              if (incomingOffset !== 0n)
                equal(
                  result.state.registers[3],
                  intoB ? 0xb2b2c002n : 0xa1a1c001n,
                  'private EBX restored',
                );
              const source = intoB ? TSS_A : TSS_B;
              equal(
                dword(cpu, source + 32n),
                Number(entry + sourceOffset + length),
                'saved post-JMP EIP',
              );
              equal(
                dword(cpu, source + 36n),
                Number(before.state.flags),
                'all outgoing flags saved',
              );
              equal(cpu.readPhysicalMemory(gdt + 0x1dn, 1)[0], intoB ? 0x89 : 0x8b, 'A busy');
              equal(cpu.readPhysicalMemory(gdt + 0x25n, 1)[0], intoB ? 0x8b : 0x89, 'B busy');
              equal(dword(cpu, TSS_A), 0xa5a57117, 'A backlink and neighbor');
              equal(dword(cpu, TSS_B), 0x5a5a8228, 'B backlink and neighbor');
            }
            const done = await cpu.run({ budget: 8n, stop: entry + 51n });
            equal(done.reason, 'stop', 'A finished the last leg');
            same(ports, markers('SABCDE'), 'both instruction streams advanced in order');
            equal(dword(cpu, PAGE_A + 0x80n), 47, 'private A counter');
            equal(dword(cpu, PAGE_B + 0x80n), 118, 'private B counter');
          });
      },
    },
    {
      name: 'CALL B, JMP C, nested IRET returns through the backlink selected by C',
      async run() {
        await withGuest(compiler, async (cpu, ports) => {
          await toCaller(cpu);
          cpu.writePhysicalMemory(RESUME_A, hex('b05ae6e9')); // Z
          cpu.writePhysicalMemory(ENTRY_B, hex('b042e6e9ea020000002800b044e6e9cf')); // B; JMP C; D; IRET
          cpu.writePhysicalMemory(ENTRY_C, hex('b043e6e9cf')); // C; IRET
          write32(cpu, TSS_B + 84n, 0x10);
          write32(cpu, TSS_C + 36n, 0x4002);
          write32(cpu, TSS_C, 0x71170018); // C is nested under A independently of B.
          const c = await cpu.run({ budget: 16n, stop: ENTRY_C });
          equal(c.reason, 'stop', 'reached C before its first instruction');
          equal(c.retired, 4n, 'CALL, marker pair, JMP');
          equal(cpu.readContext().tr.selector, 0x28, 'C current');
          equal(dword(cpu, TSS_B + 36n) & 0x4000, 0x4000, 'B saved its nested NT');
          equal(dword(cpu, TSS_C), 0x71170018, 'C chose A as its parent');
          equal(cpu.readPhysicalMemory(GDT + 0x1dn, 1)[0], 0x8b, 'A remains busy');
          equal(cpu.readPhysicalMemory(GDT + 0x25n, 1)[0], 0x89, 'B released');
          equal(cpu.readPhysicalMemory(GDT + 0x2dn, 1)[0], 0x8b, 'C busy');
          const a = await cpu.run({ budget: 8n, stop: RESUME_A + 4n });
          equal(a.reason, 'stop', 'A returned after the original CALL');
          equal(a.retired, 5n, 'C marker, IRET and A marker');
          equal(cpu.readContext().tr.selector, 0x18, 'IRET selected A');
          equal(cpu.readPhysicalMemory(GDT + 0x2dn, 1)[0], 0x89, 'IRET released C');
          equal(dword(cpu, TSS_C + 36n) & 0x4000, 0, 'IRET clears saved NT');
          same(ports, markers('SBCZ'), 'B continuation skipped');
        });
      },
    },
  ];
}
