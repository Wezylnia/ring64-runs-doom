import {
  DATA,
  FULL,
  OSXSAVE,
  PC,
  PHYS,
  enable,
  enterUser,
  hex,
  key,
  pageFault,
  position,
  put,
  run,
  same,
  withImage,
  word,
  type Scenario,
} from './fixture.js';
import { banks, stack } from '../x87/fixture.js';

export function boundaryScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'XSAVE publishes to captured physical targets when its first field clears the leaf for its header and PKRU',
      async run() {
        const base = 0x1bfe00n,
          header = 0x1c0000n,
          pkru = 0x13579bdc;
        await withImage(
          compiler,
          '480fae230f013f8b07',
          async (cpu) => {
            await enable(cpu);
            cpu.setX87State(stack({}, 0, 0x037e)); // FCW bit zero clears the PTE present bit.
            cpu.setProtectionKeysState({ pkru, pkrs: 0 });
            position(cpu, PC, { 0: 0x201n, 2: 0n, 3: base, 7: header });
            same(word(cpu, 0x4e00), 0xb063n, 'header PTE before XSAVE');
            await run(cpu, 1);
            same(word(cpu, 0x4e00, 2), 0x037en, 'first x87 field overwrote the header PTE');
            same(word(cpu, 0xb000), 0x601n, 'header merged into original frame');
            same(word(cpu, 0xb040, 4), BigInt(pkru), 'PKRU stored in original frame');
            same(word(cpu, 0xb044, 4), 0xc7c7c7c7n, 'unused upper PKRU bytes preserved');
            same(cpu.readPagingState().cr2, 0xdeadn, 'committed transfer did not fault');
            await run(cpu, 1); // INVLPG [RDI] before testing the new page-table contents.
            pageFault(await cpu.run({ budget: 1n }), header, 'read', 0, PC + 7n);
          },
          (f) => {
            // The last 512 bytes of the first linear page map to the actual PT. PTE[1c0h]
            // therefore lies at precisely the first destination byte of this image.
            f.leaf(0x1bf000, 0x4063n);
            f.leaf(Number(header), 0xb063n);
            f.bytes.fill(0xc7, 0xb000, 0xb048);
            put(f.bytes, 0xb000, 0x400n);
          },
        );
      },
    },
    {
      name: 'XSAVE retained middle entry uses live XCR0 with FS, SIB, REX.B/X/W and 32-bit address wrap',
      async run() {
        // FS:[R13D + R10D * 2 + 40h]. Effective offset 0x1100, FS.base 0x4f00.
        await withImage(
          compiler,
          '9064674b0fae645540',
          async (cpu) => {
            const context = cpu.readContext();
            context.segments.fs.base = 0x4f00n;
            cpu.setContext(context);
            await enable(cpu, 1n);
            cpu.setXmmState(banks());
            const params = { 0: 2n, 2: 0n, 13: 0xdeadbeef00001000n, 10: 0x7777777700000060n };
            position(cpu, PC, params);
            await run(cpu, 2);
            same(word(cpu, PHYS + 512), 0n, 'disabled SSE component left header untouched');
            cpu.setXcr0(3n);
            same(cpu.readXcr0(), 3n, 'live checkpoint control');
            position(cpu, PC + 1n, params);
            const warm = await run(cpu, 1);
            same(warm.resources.compilations, 0, 'warm middle consumes current XCR0');
            same(word(cpu, PHYS + 512), 2n, 'header effective address');
            same(
              word(cpu, PHYS + 160),
              BigInt.asUintN(64, banks()[0]!),
              'SIB and FS select expected destination',
            );
            cpu.setXcr0(1n);
            same(cpu.readXmmState(), banks(), 'disabling XSTATE.SSE preserves live SSE bank');
          },
          (f) => f.bytes.fill(0, PHYS + 512, PHYS + 536),
        );
      },
    },
    {
      name: 'XSAVE invalidates compiled executable aliases only for the selected XMM span',
      async run() {
        for (const offset of [160, 464, 544, 580]) {
          const target = 0x4000n + BigInt(offset);
          await withImage(
            compiler,
            '0fae23',
            async (cpu) => {
              await enable(cpu);
              for (let pass = 0; pass < 2; pass++) {
                position(cpu, target);
                const got = await run(cpu, 1);
                same(got.state.registers[0], 0x11111111n, 'initial executable bytes');
                same(got.resources.compilations, pass === 0 ? 1 : 0, 'one resident source owner');
              }
              const xmm = banks();
              xmm[0] = 0xfeeb22222222b8n;
              cpu.setXmmState(xmm);
              position(cpu, PC, { 0: 2n, 2: 0n, 3: DATA });
              await run(cpu, 1);
              position(cpu, target);
              const got = await run(cpu, 1);
              same(
                got.state.registers[0],
                offset === 160 ? 0x22222222n : 0x11111111n,
                'only selected alias contents replaced',
              );
              same(
                got.resources.compilations,
                offset === 160 ? 1 : 0,
                'only written alias loses its translation',
              );
            },
            (f) => {
              f.leaf(0x4000, 0xa063n); // Executable linear alias of the save area.
              f.bytes.set(hex('b811111111ebfe'), PHYS + offset);
            },
          );
        }
      },
    },
    {
      name: 'XSAVE and XRSTOR header-only masks skip an unmapped base page and never touch its contents',
      async run() {
        for (const code of ['0fae23', '0fae2b']) {
          await withImage(
            compiler,
            code,
            async (cpu) => {
              await enable(cpu);
              // Effective base 0x6e00 has its header at 0x7000. Page 6 is absent.
              position(cpu, PC, { 0: 0n, 2: 0n, 3: 0x6e00n });
              await run(cpu, 1);
              same(word(cpu, 0xb000), code === '0fae23' ? 0x100n : 0n, 'header-only read/merge');
              same(cpu.readPagingState().cr2, 0xdeadn, 'unused page never faults');
              same(
                word(cpu, 0x4038) & 0x60n,
                code === '0fae23' ? 0x60n : 0x20n,
                'only the selected header access promotes A/D',
              );
            },
            (f) => {
              f.leaf(0x6000, 0n);
              f.leaf(0x7000, 0xb003n);
              f.bytes.fill(0, 0xb000, 0xb040);
              if (code === '0fae23') put(f.bytes, 0xb000, 0x100n);
            },
          );
        }
      },
    },
    {
      name: 'XRSTOR reads with outgoing PKRU and the following load observes incoming rights on cached translations',
      async run() {
        await withImage(
          compiler,
          '900fae2b8b03',
          async (cpu) => {
            await enable(cpu);
            enterUser(cpu);
            const scalar = { 0: 512n, 2: 0n, 3: DATA };
            position(cpu, PC, scalar);
            const got = await cpu.run({ budget: 3n });
            pageFault(got, DATA, 'read', 0x25, PC + 4n);
            same(got.retired, 2n, 'NOP and successful self-restricting XRSTOR retired');
            same(
              cpu.readProtectionKeysState(),
              { pkru: 4, pkrs: 0 },
              'new key after successful restore',
            );
            putImageRights(cpu, 0);
            position(cpu, PC + 1n, scalar);
            const blocked = await cpu.run({ budget: 1n });
            pageFault(blocked, DATA + 512n, 'read', 0x25, PC + 1n);
            same(blocked.retired, 0n, 'denied image cannot grant its own access');
            same(cpu.readProtectionKeysState().pkru, 4, 'outgoing key retained on failure');
            cpu.setProtectionKeysState({ pkru: 0, pkrs: 0 });
            position(cpu, PC + 1n, scalar);
            const retried = await run(cpu, 2);
            same(retried.state.registers[0], 0x78563412n, 'next load observes permitted key');
          },
          (f) => {
            f.leaf(Number(PC), 0x8067n);
            f.leaf(Number(DATA), key(1) | 0xa067n);
            f.bytes.fill(0, PHYS + 512, PHYS + 584);
            put(f.bytes, PHYS + 512, 512n);
            put(f.bytes, PHYS + 576, 4n, 4);
          },
        );
      },
    },
    {
      name: 'XSAVE faults on a late PKRU component without writing reachable fields or reporting earlier watchpoints',
      async run() {
        const base = 0x6dc0n; // Header at 6fc0, PKRU at 7000.
        await withImage(
          compiler,
          '900fae2390',
          async (cpu) => {
            await enable(cpu);
            cpu.setDebugState({ addresses: [base + 160n, 0n, 0n, 0n], dr6: 0n, dr7: 0x10401n });
            const before = cpu.readPhysicalMemory(BigInt(PHYS) + 0xdc0n, 576),
              debug = cpu.readDebugState();
            position(cpu, PC, { 0: FULL, 2: 0n, 3: base });
            const r = await cpu.run({ budget: 2n });
            pageFault(r, 0x7000n, 'write', 2, PC + 1n);
            same(r.retired, 1n, 'earlier NOP retired');
            same(
              cpu.readPhysicalMemory(BigInt(PHYS) + 0xdc0n, 576),
              before,
              'early XSAVE fields preserved',
            );
            same(cpu.readDebugState(), debug, 'no early watch indication');
            cpu.writePhysicalMemory(0x4038n, new Uint8Array([3, 0xb0, 0, 0, 0, 0, 0, 0]));
            position(cpu, PC + 1n, { 0: FULL, 2: 0n, 3: base });
            const hit = await cpu.run({ budget: 1n });
            same(
              [hit.reason, hit.retired, hit.debugException?.breakpoints],
              ['debug-exception', 1n, 1],
              'committed watched store',
            );
          },
          (f) => f.leaf(0x7000, 0n),
        );
      },
    },
    {
      name: 'XSTATE memory admission precedes misalignment and can execute with CR0.EM, OSFXSR clear and pending x87 status',
      async run() {
        for (const code of ['0fae23', '0fae2b'])
          await withImage(
            compiler,
            code,
            async (cpu) => {
              await enable(cpu);
              cpu.setX87State(stack({}, 5, 0x37b, 0x8084));
              cpu.setXmmState(banks());
              const p = cpu.readPagingState(),
                masked = p.cr4 & ~0x200n;
              cpu.setPagingState({ ...p, cr0: p.cr0 | 12n, cr4: masked & ~OSXSAVE });
              position(cpu, PC, { 0: 0n, 2: 0n, 3: DATA + 1n });
              same(
                (await cpu.run({ budget: 1n })).reason,
                'invalid-encoding',
                'OSXSAVE before TS and address',
              );
              cpu.setPagingState({ ...p, cr0: p.cr0 | 12n, cr4: masked });
              position(cpu, PC, { 0: 0n, 2: 0n, 3: DATA + 1n });
              same(
                (await cpu.run({ budget: 1n })).reason,
                'device-not-available',
                'TS before address',
              );
              cpu.setPagingState({ ...p, cr0: (p.cr0 | 4n) & ~8n, cr4: masked });
              position(cpu, PC, { 0: 0n, 2: 0n, 3: DATA + 1n });
              const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
              same(
                staged.state.event?.kind === 'pending'
                  ? staged.state.event.exception.vector
                  : undefined,
                13,
                '64-byte GP',
              );
              position(cpu, PC, { 0: 0n, 2: 0n, 3: DATA });
              await run(cpu, 1);
            },
            (f) => f.bytes.fill(0, PHYS + 512, PHYS + 536),
          );
      },
    },
  ];
}
function putImageRights(cpu: import('../../../host/index.js').Cpu, keyValue: number) {
  const p = new Uint8Array(4);
  put(p, 0, BigInt(keyValue), 4);
  cpu.writePhysicalMemory(BigInt(PHYS) + 576n, p);
}
