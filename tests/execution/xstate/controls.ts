import { controls, input, using, type Profile } from '../identity-time/oracle.js';
import {
  CPUID,
  CONTROL,
  FULL,
  OSXSAVE,
  PC,
  XGET,
  XSET,
  createCpu,
  enable,
  enterUser,
  position,
  run,
  same,
  withImage,
  type Scenario,
} from './fixture.js';

export function controlScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'XSTATE discovery reflects guest CR4 and every legal XCR0, including readback after disable and re-enable',
      async run() {
        await withImage(compiler, '90', async (cpu) => {
          const cpuid = async (leaf: number, subleaf = 0) => {
            position(cpu, CPUID, { 0: BigInt(leaf), 1: BigInt(subleaf) });
            const regs = (await run(cpu, 1)).state.registers;
            return [regs[0], regs[3], regs[1], regs[2]];
          };
          same((await cpuid(0))[0], 13n, 'maximum basic leaf');
          same(
            (await cpuid(1))[2]! & 0x0c000000n,
            0x04000000n,
            'hardware support before OS enable',
          );
          same(await cpuid(13), [FULL, 576n, 584n, 0n], 'initial component size');
          for (const sub of [1, 2, 8, 10, 63])
            same(await cpuid(13, sub), [0n, 0n, 0n, 0n], 'absent component ' + sub);
          same(await cpuid(13, 9), [8n, 576n, 0n, 0n], 'PKRU layout');
          for (const mask of [1n, 3n, 0x201n, FULL]) {
            await enable(cpu, mask);
            same((await cpuid(1))[2]! & 0x0c000000n, 0x0c000000n, 'live OSXSAVE');
            same((await cpuid(13))[1], mask & 512n ? 584n : 576n, 'live enabled image size');
            position(cpu, XGET, {
              0: ~0n & 0xffffffffffffffffn,
              1: 0xfedcba9800000000n,
              2: 0xabcdef0123456789n,
            });
            const r = await run(cpu, 1);
            same(
              [r.state.registers[0], r.state.registers[2], r.state.flags],
              [mask, 0n, 0x202n],
              'zero-extended XGETBV',
            );
          }
          position(cpu, CONTROL, { 0: cpu.readPagingState().cr4 & ~OSXSAVE });
          await run(cpu, 1);
          position(cpu, XGET, { 1: 0n });
          same((await cpu.run({ budget: 1n })).reason, 'invalid-encoding', 'disabled XGETBV');
          position(cpu, CONTROL, { 0: cpu.readPagingState().cr4 | OSXSAVE });
          await run(cpu, 1);
          position(cpu, XGET, { 1: 0n });
          same((await run(cpu, 1)).state.registers[0], FULL, 'XCR0 survives CR4 toggle');
        });
      },
    },
    {
      name: 'XSTATE rejects illegal XCR0 and CPL3 writes without changing the enabled mask',
      async run() {
        await withImage(
          compiler,
          '90',
          async (cpu) => {
            await enable(cpu, 3n);
            for (const [low, index, high] of [
              [0n, 0n, 0n],
              [2n, 0n, 0n],
              [5n, 0n, 0n],
              [1n, 1n, 0n],
              [1n, 0n, 1n],
            ]) {
              position(cpu, XSET, { 0: low!, 1: index!, 2: high! });
              const r = await cpu.run({ budget: 1n });
              same(
                [r.reason, r.retired, r.state.rip],
                ['protection-fault', 0n, XSET],
                'invalid XSETBV',
              );
            }
            position(cpu, XGET, { 1: 1n });
            same((await cpu.run({ budget: 1n })).reason, 'protection-fault', 'XGETBV(1)');
            enterUser(cpu);
            position(cpu, XSET, { 0: FULL, 1: 0n, 2: 0n });
            same((await cpu.run({ budget: 1n })).reason, 'protection-fault', 'CPL3 XSETBV');
            position(cpu, XGET, { 1: 0n });
            same((await run(cpu, 1)).state.registers[0], 3n, 'CPL3 read after denied writes');
          },
          (f) => f.leaf(Number(PC), 0x8067n),
        );
      },
    },
    {
      name: 'XSTATE fixed32 control operands and memory forms across real, virtual8086, protected, compatibility and long mode',
      async run() {
        const profiles: Profile[] = [
          ...controls,
          { mode: 'virtual8086', bits: 16, cpl: 3, family: 'pg0' },
        ];
        for (const profile of profiles) {
          const addr = profile.bits === 16 ? '07' : '03'; // [BX] or [EBX/RBX].
          const code =
            '0f01d1' +
            '0f01d0' +
            '0fae' +
            (0x20 + parseInt(addr, 16)).toString(16) +
            '0fae' +
            (0x28 + parseInt(addr, 16)).toString(16);
          await using({ createCpu, compiler }, code, profile, async (cpu) => {
            cpu.setPagingState({
              ...cpu.readPagingState(),
              cr4: cpu.readPagingState().cr4 | OSXSAVE,
            });
            const before = input(profile);
            before.registers[0] = 0xdeadbeef00000003n;
            before.registers[1] = 0x1111111100000000n;
            before.registers[2] = 0x9999999900000000n;
            before.registers[3] = 0x4000n;
            cpu.setState(before);
            const v86 = profile.mode === 'virtual8086';
            const written = await cpu.run({ budget: 1n });
            same(written.reason, v86 ? 'protection-fault' : 'budget', 'mode-specific XSETBV');
            cpu.setState({ ...before, rip: PC + 3n });
            const got = await run(cpu, 1);
            same(got.state.registers[0]! & 0xffffffffn, v86 ? 1n : 3n, 'fixed32 XGETBV read');
            const image = { ...before, rip: PC + 6n };
            image.registers[0] = v86 ? 1n : 2n;
            cpu.setState(image);
            const bank = Array.from({ length: 16 }, (_, i) => BigInt(i + 1));
            cpu.setXmmState(bank);
            await run(cpu, 1);
            if (!v86) {
              const saved = cpu.readPhysicalMemory(0x4000n + 160n, 256);
              same(saved[112], 8, 'XMM7 transfer');
              same(saved[128], profile.bits === 64 ? 9 : 0, 'mode-specific XMM8');
              const dirty = bank.map((v) => v + 16n);
              cpu.setXmmState(dirty);
              await run(cpu, 1);
              same(
                cpu.readXmmState(),
                profile.bits === 64 ? bank : [...bank.slice(0, 8), ...dirty.slice(8)],
                'visible XMM restore',
              );
            } else await run(cpu, 1);
          });
        }
      },
    },
    {
      name: 'XSTATE unsupported extension encodings and disabled forms report UD before memory',
      async run() {
        for (const code of [
          '0f01d0',
          '0f01d1',
          '0fae23',
          '0fae2b',
          'f00fae23',
          'f00f01d0',
          'f00f01d1',
          '0fc723',
          '0fc72b',
          '0fc71b',
          '0fae33',
        ]) {
          await withImage(compiler, code, async (cpu) => {
            position(cpu, PC, { 1: 1n, 3: 0x800000000000n });
            const r = await cpu.run({ budget: 1n });
            same([r.reason, r.retired], ['invalid-encoding', 0n], code + ' disabled form');
          });
        }
      },
    },
  ];
}
