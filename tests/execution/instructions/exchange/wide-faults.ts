import { createCpu } from '../../../../sdk/src/index.js';
import { changed, check, equal, exit, initial, type Scenario } from '../../../support/fixture.js';
import { preserveNumeric } from '../../far-control/fixture.js';
import { fixture, same, using, word } from '../../supervisor-access/fixture.js';
import { little } from '../../paging/fixture.js';

const pc = 0x1000n,
  slot = 0x6000n,
  lo = 0x8123456789abcdefn,
  hi = 0xfedcba9876543210n;
const nextLo = 0x0123456789abcdefn,
  nextHi = 0x8877665544332211n;
const watch = { addresses: [slot + 15n, 0n, 0n, 0n], dr6: 0n, dr7: 0x10401n } as const;

function mapping(start: number, end: number, read = true, write = true) {
  return {
    base: BigInt(start),
    backing: 'ram',
    offset: start,
    length: end - start,
    permissions: { read, write, execute: true },
  };
}

export function wideFaultScenarios(compiler: WebAssembly.Module): Scenario[] {
  const side = { createCpu, compiler };
  return [
    {
      name: 'CMPXCHG16B read-only miss and dirty-bit refusal leave both halves and ZF untouched until repair',
      async run() {
        for (const hit of [false, true]) {
          const f = fixture(side, 'ia32e', 'f0480fc70f', { cpl: 3 });
          f.paging.cr2 = 0xabcden;
          f.leaf(Number(slot), 0xa025n); // Present, user, read-only, A=1 and D=0.
          f.bytes.set(little([lo, hi]), 0xa000);
          const pte = f.pt + 6 * 8;
          await using(side, f, async (cpu) => {
            const before = initial(
              pc,
              { 0: lo, 1: nextHi, 2: hi ^ BigInt(!hit), 3: nextLo, 7: slot },
              hit ? 0xe97n : 0xed7n,
            );
            cpu.setState(before);
            cpu.setDebugState({ ...watch, addresses: [...watch.addresses] });
            const clock = cpu.readTimeState().tsc;
            const pf = await cpu.run({ budget: 1n });
            exit(pf, 'page-fault', 1n, 0n, before);
            equal(pf.pageFault?.address, slot, 'failed write to pair');
            equal(pf.pageFault?.errorCode, 7, 'CPL3 write-protection fault, including mismatch');
            equal(cpu.readDebugState().dr6 & 1n, 0n, 'failed page check produces no watch');
            cpu.writePhysicalMemory(BigInt(pte), little([0xa027n]));
            cpu.setMappings([
              mapping(0, pte),
              mapping(pte, pte + 8, true, false),
              mapping(pte + 8, f.bytes.length),
            ]);
            const denied = await cpu.run({ budget: 1n });
            exit(denied, 'physical-memory-fault', 1n, 0n, before);
            equal(
              denied.physicalMemoryFault?.phase,
              'dirty-write',
              'page metadata refuses D update',
            );
            equal(denied.physicalMemoryFault?.failingPhysicalAddress, BigInt(pte), 'dirty PTE');
            equal(cpu.readPagingState().cr2, slot, 'private refusal leaves CR2 alone');
            same(cpu.readPhysicalMemory(0xa000n, 16), little([lo, hi]), 'both halves preserved');
            equal(word(cpu, pte), 0xa027n, 'D remained clear');
            equal(cpu.readDebugState().dr6 & 1n, 0n, 'tentative read did not select debug');
            cpu.setMappings([mapping(0, f.bytes.length)]);
            const done = await cpu.run({ budget: 1n });
            exit(
              done,
              'debug-exception',
              1n,
              1n,
              changed(
                before,
                pc + 5n,
                hit ? {} : { 0: lo, 2: hi },
                (before.flags & ~64n) | (hit ? 64n : 0n),
              ),
            );
            equal(cpu.readDebugState().dr6 & 1n, 1n, 'last byte selects a write breakpoint');
            equal(word(cpu, pte), 0xa067n, 'writeback sets D even on a miss');
            same(
              cpu.readPhysicalMemory(0xa000n, 16),
              little(hit ? [nextLo, nextHi] : [lo, hi]),
              'repaired writeback',
            );
            equal(cpu.readTimeState().tsc - clock, 1n, 'failed attempts did not retire');
          });
        }
      },
    },
    {
      name: 'CMPXCHG16B preflights inaccessible backing fragments inside an aligned page and repairs once',
      async run() {
        for (const [split, tail, hit] of [
          [1, 'hole', true],
          [8, 'write-only', true],
          [15, 'read-only', false],
        ] as const) {
          const f = fixture(side, 'ia32e', '480fc70f');
          f.paging.cr2 = 0x9123n;
          f.leaf(Number(slot), 0xa023n);
          f.bytes.set(little([lo, hi]), 0xa000);
          const seam = 0xa000 + split;
          await using(side, f, async (cpu) => {
            cpu.setMappings([
              mapping(0, seam),
              ...(tail === 'hole'
                ? []
                : [mapping(seam, seam + 1, tail === 'read-only', tail === 'write-only')]),
              mapping(seam + 1, f.bytes.length),
            ]);
            const before = initial(
              pc,
              { 0: lo, 1: nextHi, 2: hi ^ BigInt(!hit), 3: nextLo, 7: slot },
              hit ? 0xe97n : 0xed7n,
            );
            cpu.setState(before);
            const refused = await cpu.run({ budget: 1n });
            exit(refused, 'physical-memory-fault', 1n, 0n, before);
            const fault = refused.physicalMemoryFault;
            check(fault?.phase === 'final-access', 'data backing failure');
            equal(fault.address, slot, 'one pair');
            equal(fault.access, 'write', 'combined RMW request');
            equal(fault.width, 16, 'nominal wide operand');
            equal(fault.failingPhysicalAddress, BigInt(seam), 'first refusing backing byte');
            equal(
              fault.cause,
              tail === 'hole' ? 'unmapped' : 'permission',
              'actual backend barrier',
            );
            equal(cpu.readPagingState().cr2, 0x9123n, 'private failure preserves CR2');
            equal(word(cpu, f.pt + 6 * 8), 0xa023n, 'no premature D');
            cpu.setMappings([mapping(0, f.bytes.length)]);
            same(cpu.readPhysicalMemory(0xa000n, 16), little([lo, hi]), 'no early low-half write');
            const done = await cpu.run({ budget: 1n });
            exit(
              done,
              'budget',
              1n,
              1n,
              changed(
                before,
                pc + 4n,
                hit ? {} : { 0: lo, 2: hi },
                (before.flags & ~64n) | (hit ? 64n : 0n),
              ),
            );
            same(
              cpu.readPhysicalMemory(0xa000n, 16),
              little(hit ? [nextLo, nextHi] : [lo, hi]),
              'entire pair after repair',
            );
          });
        }
      },
    },
    {
      name: 'CMPXCHG16B checks the final FS linear alignment before paging under active user alignment checking',
      async run() {
        const f = fixture(side, 'ia32e', '64480fc70e', { cpl: 3 }); // [FS:RSI]
        f.paging.cr0 |= 0x40000n | 0xcn; // AM, EM and TS. The instruction uses integer registers.
        f.paging.cr4 &= ~0x40600n; // OSFXSR, OSXMMEXCPT, OSXSAVE.
        f.paging.cr2 = 0x12345n;
        f.leaf(Number(slot), 0n);
        await using(side, f, async (cpu) => {
          const checkNumeric = preserveNumeric(cpu);
          const ctx = cpu.readContext();
          ctx.segments.fs.base = 8n;
          cpu.setContext(ctx);
          const before = initial(pc, { 6: slot }, 0x40ed7n);
          cpu.setState(before);
          const gp = await cpu.run({ budget: 1n });
          exit(gp, 'alignment-fault', 1n, 0n, before);
          same(
            gp.alignmentFault,
            {
              segment: 'fs',
              instructionPc: pc,
              offset: slot,
              address: slot + 8n,
              width: 16,
              access: 'write',
              alignment: 16,
            },
            'mandatory alignment checks the linear address',
          );
          equal(cpu.readPagingState().cr2, 0x12345n, 'mandatory #GP precedes page access');
          cpu.setState(initial(pc, { 6: slot - 8n }, 0x40ed7n));
          const pf = await cpu.run({ budget: 1n });
          equal(
            pf.reason,
            'page-fault',
            'misaligned offset plus FS base produces an aligned linear operand',
          );
          equal(pf.pageFault?.address, slot, 'linear page fault');
          equal(pf.pageFault?.errorCode, 6, 'nonpresent user write');
          cpu.writePhysicalMemory(BigInt(f.pt + 6 * 8), little([0xa067n]));
          equal(
            (await cpu.run({ budget: 1n })).reason,
            'budget',
            'aligned pair succeeds with TS, EM and AM/AC',
          );
          checkNumeric();
        });
      },
    },
  ];
}
