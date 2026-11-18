import { createCpu } from '../../../host/index.js';
import { longSnapshot } from '../../support/context.js';
import { check, equal, hex, initial, LIMITS, type Scenario } from '../../support/fixture.js';
import { little, same } from '../paging/fixture.js';
import { fixture, put, using, word } from '../supervisor-access/fixture.js';

// Supervisor boot. R13 values are OS opt-out, CPUID-dispatched, forced enable,
// and forced user instruction without enabling CR4. R14 communicates permission to CPL3.
const boot = [
  'b9810000c0b800000000ba080020000f30', // WRMSR STAR
  'b9820000c0b800104000ba000000000f30', // WRMSR LSTAR = kernel
  'b9840000c0b800030000ba000000000f30', // WRMSR FMASK = 0x300
  'b9020100c0b800404000ba000000000f30', // WRMSR KERNEL_GS_BASE
  'b80700000031c90fa24531f6', // CPUID.7.0; XOR R14D,R14D
  '4183fd03741e4585ed741f4183fd027408f7c3010000007411', // choose OS policy and test EBX[0]
  '0f20e00d000001000f22e0', // MOV RAX,CR4; OR EAX,10000h; MOV CR4,RAX
  '41be0100000048cf', // R14D=1; IRETQ
].join('');
const kernel = [
  '0f01f84889ce4c89dfbd00000000', // SWAPGS; preserve RCX/R11; zero RBP
  '654c8b55006548ff450049c1e205', // index the supervisor's log through GS
  '4585f67516', // direct user path skips the fallback setters
  'b9000100c04489c031d20f30b9020100c04489c80f30', // WRMSR active FS and parked user GS
  'b9000100c00f326549894220ffc10f326549894228ffc10f326549894230', // record RDMSR FS, GS, KERNEL_GS_BASE
  '4889f14989fb0f01f8480f07', // restore RCX/R11; SWAPGS; SYSRETQ
].join('');
const user = [
  '41bc40304000bd0000000064488b45004989042465488b45004989442408', // record both initial TLS markers
  '4585f67414f3480faec04989442430f3480faec84989442438', // if permitted, RDFSBASE and RDGSBASE
  '41b80080400041b9009040004585f6740af3490faed0f3490faed90f05', // switch to B with qword writes or syscall
  '64488b4500498944241065488b45004989442418', // read B through FS/GS
  '41b80060400041b9007040004585f6740af3410faed0f3410faed90f05', // restore A with dword writes or syscall
  '64488b4500498944242065488b4500498944242890', // read A through FS/GS; stop before NOP
].join('');
const gdt =
  '0000000000000000ffff0000009baf00ffff00000093cf0000000000000000000000000000000000ffff000000f3cf00ffff000000fbaf00';

// Clang -O2 -mfsgsbase bump_tls(newBase, offset, delta), through its original RET.
// save GS, set newBase, add delta to GS:[offset], store, restore GS.
const bumpTls = '4889d0f3480faec9f3480faedf6548030665488906f3480faed9c3';

export function lifecycleScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'CPL3 compiled TLS counter updates the alternate GS record and restores the original base and stack',
      async run() {
        for (const family of ['ia32e', 'fixed'] as const) {
          const side = { compiler, createCpu },
            f = fixture(side, family, bumpTls, { cpl: 3 });
          f.paging.cr4 |= 0x10000n;
          f.leaf(0x6000, 0xa067n);
          f.leaf(0x7000, 0xb067n);
          f.leaf(0x9000, 0xd067n);
          put(f.bytes, 0xa018, 777n);
          put(f.bytes, 0xb018, 41n);
          put(f.bytes, 0xdff0, 0x4000n);
          await using(side, f, async (cpu) => {
            const ctx = cpu.readContext();
            ctx.segments.gs.base = 0x6000n;
            cpu.setContext(ctx);
            cpu.setState(initial(0x1000n, { 2: 6n, 4: 0x9ff0n, 6: 24n, 7: 0x7000n }));
            const r = await cpu.run({ budget: 16n, stop: 0x4000n });
            equal(r.reason, 'stop', 'compiled function returned');
            equal(r.retired, 7n, 'exact compiler text');
            equal(r.state.registers[0], 47n, 'return value');
            equal(r.state.registers[4], 0x9ff8n, 'caller stack');
            equal(word(cpu, 0xa018), 777n, 'caller TLS unaffected');
            equal(word(cpu, 0xb018), 47n, 'new TLS counter');
            same(cpu.readContext(), ctx, 'caller context restored');
          });
        }
      },
    },
    {
      name: 'guest kernel negotiates FSGSBASE before IRET and checks the active and parked GS banks across two syscalls',
      async run() {
        for (const model of ['physical', 'ia32e-paged'] as const)
          for (const request of [0, 1, 2, 3]) {
            const ram = new Uint8Array(0x18000);
            for (const [a, v] of [
              [0x1000, 0x2027],
              [0x2000, 0x3027],
              [0x3010, 0x4027],
            ] as const)
              put(ram, a, BigInt(v));
            for (let i = 0; i < 10; i++)
              put(
                ram,
                0x4000 + i * 8,
                BigInt(0x9000 + i * 0x1000) | (i === 2 || i === 3 || i >= 6 ? 0x67n : 0x63n),
              );
            ram.set(hex(boot), 0x9000);
            ram.set(hex(kernel), 0xa000);
            ram.set(hex(user), 0xb000);
            ram.set(hex(gdt), 0xe000);
            ram.set(little([0x402000n, 0x33n, 0x202n, 0x403ff0n, 0x2bn]), 0xdfc0);
            for (const [i, v] of [0xaaa1n, 0xbbb1n, 0xaaa2n, 0xbbb2n].entries())
              put(ram, 0xf000 + i * 0x1000, v);
            const context = longSnapshot(0x406000n, 0x407000n);
            context.gdtr = { base: 0x405000n, limit: 55 };
            for (const name of ['fs', 'gs'] as const) {
              context.segments[name].selector = 0x2b;
              context.segments[name].dpl = 3;
            }
            const cpu = await createCpu({
              compiler,
              memoryModel: model,
              paging: { cr0: 0x80000033n, cr3: 0x1000n, cr2: 0xdeadn, cr4: 0x628n, efer: 0x501n },
              context,
              entry: 0x400000n,
              limits: LIMITS,
              memory: {
                backings: [{ id: 'ram', bytes: ram }],
                mappings: [
                  {
                    base: 0n,
                    backing: 'ram',
                    offset: 0,
                    length: ram.length,
                    permissions: { read: true, write: true, execute: true },
                  },
                ],
              },
              memoryLimits: { maxBackings: 4, maxBackingBytes: 0x40000, maxMappings: 16 },
            });
            try {
              cpu.setState(initial(0x400000n, { 4: 0x404fc0n, 13: BigInt(request) }));
              const r = await cpu.run({
                budget: 256n,
                stop: 0x402099n,
                eventDelivery: { budget: 0n },
              });
              const ctx = cpu.readContext();
              equal(ctx.cpl, 3, 'guest entered user mode');
              same(ctx.segments.fs, context.segments.fs, 'restored FS with original selector');
              same(ctx.segments.gs, context.segments.gs, 'restored GS with original selector');
              equal(cpu.readFastSystemState().kernelGsBase, 0x404000n, 'inactive kernel GS');
              if (request === 3) {
                check(
                  r.state.event.kind === 'pending',
                  'forced disabled instruction stages a fault',
                );
                equal(r.state.event.exception.vector, 6, '#UD');
                equal(r.state.rip, 0x402023n, 'first RDFSBASE');
                equal(word(cpu, 0xd000), 0n, 'no syscall');
                continue;
              }
              const direct = request !== 0;
              equal(r.reason, 'stop', 'thread A restored');
              equal(r.state.registers[14], BigInt(direct), 'OS permission observed by user');
              equal(cpu.readPagingState().cr4, direct ? 0x10628n : 0x628n, 'architectural gate');
              equal(word(cpu, 0xd000), 2n, 'two syscall round trips');
              same(
                cpu.readPhysicalMemory(0xc040n, 64),
                little([
                  0xaaa1n,
                  0xbbb1n,
                  0xaaa2n,
                  0xbbb2n,
                  0xaaa1n,
                  0xbbb1n,
                  direct ? 0x406000n : 0n,
                  direct ? 0x407000n : 0n,
                ]),
                'three views and saved original bases',
              );
              same(
                cpu.readPhysicalMemory(0xd020n, 56),
                little([0x408000n, 0x404000n, 0x409000n, 0n, 0x406000n, 0x404000n, 0x407000n]),
                'RDMSR active FS, active kernel GS and parked user GS',
              );
            } finally {
              cpu.dispose();
            }
          }
      },
    },
  ];
}
