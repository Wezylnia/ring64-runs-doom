import { ONE, banks, stack } from '../x87/fixture.js';
import {
  DATA,
  FULL,
  PC,
  PHYS,
  enable,
  position,
  put,
  run,
  same,
  withImage,
  type Scenario,
} from './fixture.js';
const MASK128 = (1n << 128n) - 1n;
const IMAGE_BYTES = 584;
const X87 = stack({ 0: ONE, 2: 0n, 6: 0x7fff8000000000000000n }, 5, 0x037f, 1);

export function imageScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'Guest code saves A, creates B, switches both contexts and exports restored FX images without host intervention',
      async run() {
        const program = [
          '480fae23', // XSAVE64 [RBX] saves A
          'dbe3',
          'd9ee',
          '660f76c0',
          '0fae16', // FNINIT, FLDZ, PCMPEQD XMM0,XMM0, LDMXCSR [RSI]
          'b810000000',
          '0f01ef',
          'b803020000',
          '480fae27', // WRPKRU then XSAVE64 [RDI] saves B
          '480fae2b',
          '480fae4640',
          '0f01ee',
          '4189c0', // Restore A, FXSAVE64 [RSI+40h], retain RDPKRU
          'b803020000',
          '480fae2f',
          '480fae8640030000',
          '0f01ee', // Restore B and observe its FX image and PKRU
        ];
        await withImage(compiler, program.join(''), async (cpu) => {
          await enable(cpu);
          cpu.writePhysicalMemory(BigInt(PHYS), new Uint8Array(0xb00));
          cpu.writePhysicalMemory(BigInt(PHYS + 0x500), Uint8Array.of(0x80, 0x5f, 0, 0));
          const va = banks(),
            a = stack({ 0: ONE }, 1, 0x37b);
          cpu.setX87State(a);
          cpu.setXmmState(va);
          cpu.setMxcsr(0x3f80);
          cpu.setProtectionKeysState({ pkru: 0x40, pkrs: 0 });
          position(cpu, PC, { 0: FULL, 1: 0n, 2: 0n, 3: DATA, 6: DATA + 0x500n, 7: DATA + 640n });
          const done = await run(cpu, program.length);
          same(
            [done.state.registers[8], done.state.registers[0]],
            [0x40n, 0x10n],
            'RDPKRU returned each restored process key',
          );
          for (const [offset, cw, ftw, mxcsr, xmm] of [
            [0x540, 0x37b, 2, 0x3f80, va[0]!],
            [0x840, 0x37f, 128, 0x5f80, MASK128],
          ] as const) {
            const fx = cpu.readPhysicalMemory(BigInt(PHYS + offset), 512);
            const view = new DataView(fx.buffer, fx.byteOffset, fx.byteLength);
            same(
              [
                view.getUint16(0, true),
                fx[4],
                view.getUint32(24, true),
                view.getBigUint64(160, true) | (view.getBigUint64(168, true) << 64n),
              ],
              [cw, ftw, mxcsr, xmm],
              'FXSAVE validates incoming x87/XMM/MXCSR',
            );
          }
        });
      },
    },
    {
      name: 'XSAVE merges every component mask against poisoned images and stores the two pointer formats',
      async run() {
        for (const wide of [false, true]) {
          await withImage(compiler, (wide ? '48' : '') + '0fae23', async (cpu) => {
            await enable(cpu);
            const xmm = banks();
            cpu.setX87State(X87);
            cpu.setXmmState(xmm);
            cpu.setMxcsr(0xffc5);
            cpu.setProtectionKeysState({ pkru: 0x76543210, pkrs: 0 });
            for (const mask of [0, 1, 2, 3, 512, 513, 514, 515]) {
              const bytes = new Uint8Array(IMAGE_BYTES).fill(0xad);
              put(bytes, 512, 0x8000000000100000n);
              cpu.writePhysicalMemory(BigInt(PHYS), bytes);
              const expected = bytes.slice();
              put(expected, 512, 0x8000000000100000n | BigInt(mask));
              if (mask & 1) {
                put(expected, 0, BigInt(X87.controlWord), 2);
                put(expected, 2, BigInt(X87.statusWord), 2);
                expected[4] = 0xa8; // Physical tag bits, while ST slots use logical TOP=5.
                put(expected, 6, BigInt(X87.opcode & 0x7ff), 2);
                put(expected, 8, X87.instructionPointer, wide ? 8 : 4);
                put(expected, 16, X87.dataPointer, wide ? 8 : 4);
                if (!wide) {
                  put(expected, 12, 0n, 2);
                  put(expected, 20, 0n, 2);
                }
                for (let i = 0; i < 8; i++)
                  put(expected, 32 + 16 * i, X87.registers[(i + 5) % 8]!, 10);
              }
              if (mask & 2) {
                put(expected, 24, 0xffc5n, 4);
                put(expected, 28, 0xffffn, 4);
                for (let i = 0; i < 16; i++) put(expected, 160 + 16 * i, xmm[i]!, 16);
              }
              if (mask & 512) put(expected, 576, 0x76543210n, 4);
              position(cpu, PC, {
                0: 0xdeadbeef00000000n | BigInt(mask),
                2: 0xcafebabe00000000n,
                3: DATA,
              });
              await run(cpu, 1);
              same(
                cpu.readPhysicalMemory(BigInt(PHYS), IMAGE_BYTES),
                expected,
                'sparse image with mask ' + mask + ' wide=' + wide,
              );
            }
          });
        }
      },
    },
    {
      name: 'XRSTOR initializes absent components but reads MXCSR and validates mandatory header bytes even on mask zero',
      async run() {
        await withImage(compiler, '480fae2b', async (cpu) => {
          await enable(cpu);
          const clean = new Uint8Array(IMAGE_BYTES).fill(0xee);
          clean.fill(0, 512, 536);
          put(clean, 24, 0x5f81n, 4);
          cpu.writePhysicalMemory(BigInt(PHYS), clean);
          cpu.setX87State(X87);
          cpu.setXmmState(banks());
          cpu.setMxcsr(0x1f80);
          cpu.setProtectionKeysState({ pkru: 0x76543210, pkrs: 0 });
          position(cpu, PC, { 0: FULL, 2: 0n, 3: DATA });
          await run(cpu, 1);
          same(cpu.readMxcsr(), 0x5f81, 'init SSE imports stored MXCSR and ignores MXCSR_MASK');
          same(
            cpu.readXmmState(),
            Array<bigint>(16).fill(0n),
            'init SSE clears all visible registers',
          );
          same(
            cpu.readX87State(),
            {
              registers: Array<bigint>(8).fill(0n),
              controlWord: 0x37f,
              statusWord: 0,
              tagWord: 0xffff,
              opcode: 0,
              instructionPointer: 0n,
              dataPointer: 0n,
            },
            'x87 init',
          );
          same(cpu.readProtectionKeysState(), { pkru: 0, pkrs: 0 }, 'PKRU init');
          for (const [offset, mask] of [
            [513, 0],
            [520, 0],
            [527, 0],
            [528, 0],
            [535, 0],
            [26, 2],
          ] as const) {
            const poisoned = clean.slice();
            poisoned[offset] = offset === 513 ? 4 : 0x80;
            cpu.writePhysicalMemory(BigInt(PHYS), poisoned);
            cpu.setXmmState(banks());
            cpu.setX87State(X87);
            cpu.setProtectionKeysState({ pkru: 0x98765430, pkrs: 0 });
            cpu.setMxcsr(0x3f80);
            position(cpu, PC, { 0: BigInt(mask), 2: 0n, 3: DATA });
            const result = await cpu.run({ budget: 1n });
            same([result.retired, result.state.rip], [0n, PC], 'invalid image does not retire');
            const staged = await cpu.run({ budget: 1n, eventDelivery: { budget: 0n } });
            same(
              staged.state.event?.kind === 'pending'
                ? staged.state.event.exception.vector
                : undefined,
              13,
              'invalid image produces GP ' + offset,
            );
            same(
              [
                cpu.readMxcsr(),
                cpu.readXmmState(),
                cpu.readX87State(),
                cpu.readProtectionKeysState(),
              ],
              [0x3f80, banks(), X87, { pkru: 0x98765430, pkrs: 0 }],
              'invalid restore staged publication',
            );
          }
        });
      },
    },
    {
      name: 'Guest XSAVE and XRSTOR exchange two adjacent live MMX, XMM and PKRU contexts',
      async run() {
        // XMM0 and MM0 are changed by the guest. Restore is immediately followed by a generated XOR that consumes XMM0.
        const save = '660f76c0' + '0f74c0' + '480fae23';
        const restore = '480fae2b' + '660fefc8' + '0f01ee';
        await withImage(compiler, save + restore, async (cpu) => {
          await enable(cpu);
          const page = BigInt(PHYS);
          const blanks = new Uint8Array(640 * 2);
          cpu.writePhysicalMemory(page, blanks);
          const vector = banks(),
            other = vector.map((v) => v ^ MASK128);
          const second = stack({ 1: ONE }, 2);
          for (const [idx, reg, x87, key] of [
            [0, vector, X87, 0x24],
            [1, other, second, 0x84],
          ] as const) {
            cpu.setXmmState(reg);
            cpu.setX87State(x87);
            cpu.setProtectionKeysState({ pkru: key, pkrs: 0x4000 });
            position(cpu, PC, { 0: FULL, 2: 0n, 3: DATA + 640n * BigInt(idx) });
            const out = await run(cpu, 3);
            same(out.resources.nativeEntries, 1, 'dirty XMM/MMX and save share a generated entry');
            same(
              cpu.readPhysicalMemory(page + 640n * BigInt(idx) + 160n, 16),
              new Uint8Array(16).fill(255),
              'dirty generated XMM published',
            );
          }
          for (const [idx, reg, key] of [
            [1, other, 0x84],
            [0, vector, 0x24],
          ] as const) {
            const image = cpu.readPhysicalMemory(page + 640n * BigInt(idx), IMAGE_BYTES);
            position(cpu, PC + BigInt(save.length / 2), {
              0: FULL,
              1: 0n,
              2: 0n,
              3: DATA + 640n * BigInt(idx),
            });
            const got = await run(cpu, 3);
            same(
              got.state.registers[0],
              BigInt(key),
              'RDPKRU immediately observes incoming context',
            );
            same(
              cpu.readXmmState(),
              [MASK128, reg[1]! ^ MASK128, ...reg.slice(2)],
              'dependent generated SIMD sees restored XMM',
            );
            same(
              cpu.readX87State().registers[0],
              0xffffffffffffffffffffn,
              'MM0 raw80 alias context',
            );
            same(cpu.readMxcsr(), Number(image[24]) + (Number(image[25]) << 8), 'saved MXCSR');
            same(
              cpu.readProtectionKeysState(),
              { pkru: key, pkrs: 0x4000 },
              'independent supervisor bank',
            );
          }
        });
      },
    },
  ];
}
