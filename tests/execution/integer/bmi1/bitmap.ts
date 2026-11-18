import { initial, equal, type Scenario } from '../../../support/fixture.js';
import { PC, little, same, usingPaged } from '../../paging/fixture.js';

// R8 = occupied, R9 = eligible. R15 requests the hardware path when CPUID permits it.
// The guest finds candidates in a 16-slot window and publishes the first allocation,
// its mask, and the remaining candidates. Both paths use the same output buffer.
const allocator = [
  'b80700000031c90fa2f7c308000000742b', // CPUID.(7,0).EBX[3]; JZ generic
  '4585ff7426', // TEST R15D,R15D; JZ generic
  'c4c2b8f2c1', // ANDN RAX,R8,R9
  '41ba08100000c4e2a8f7f0', // MOV R10D,0x1008; BEXTR RSI,RAX,R10
  'c4e2a0f3dec4e298f3d6c4e290f3ce', // BLSI R11,RSI; BLSMSK R12,RSI; BLSR R13,RSI
  'f34c0fbcf6eb3e', // TZCNT R14,RSI; JMP store
  '4531ff4c89c048f7d04c21c84889c648c1ee0881e6ffff0000', // generic: R15=0; RSI=((~R8 & R9)>>8)&0xffff
  '4989f349f7db4921f34c8d6eff4d89ec4931f44921f5', // R11=RSI & -RSI; R12=(RSI-1)^RSI; R13=(RSI-1)&RSI
  '41be400000004885f674044c0fbcf6', // default index 64; BSF on nonzero
  '488907488977084c895f104c8967184c896f204c897728c3', // store six qwords and RET
].join('');

export function bitmapScenario(compiler: WebAssembly.Module): Scenario {
  return {
    name: 'guest bitmap allocator compares a portable path with the advertised six-instruction BMI1 path',
    async run() {
      const stop = PC + 0x3000n,
        sp = PC + 0x1100n;
      for (const optimized of [false, true]) {
        for (const [occupied, eligible] of [
          [0n, 0n],
          [0xffffn, 0x1ff00n],
          [0n, 0x800000n],
          [0xfedcba9876543210n, 0xff00ffffff00ff00n],
        ] as const) {
          const data = new Uint8Array(512).fill(0xa5);
          data.set(little([stop]), 0x100);
          await usingPaged(
            compiler,
            {
              code: allocator,
              words: [[0x4008n, 0xa063n]],
              spans: [{ physical: 0xa000n, bytes: data }],
            },
            async (cpu) => {
              cpu.setState(
                initial(PC, {
                  4: sp,
                  7: PC + 0x1010n,
                  8: occupied,
                  9: eligible,
                  15: BigInt(optimized),
                }),
              );
              const clock = cpu.readTimeState().tsc;
              const result = await cpu.run({ budget: 64n, stop });
              equal(result.reason, 'stop', 'allocator returned to caller');
              equal(result.state.registers[4], sp + 8n, 'restored stack');
              const available = ~occupied & eligible,
                window = (available >> 8n) & 0xffffn;
              const first = Array.from({ length: 16 }, (_, i) => i).find(
                (i) => (window & (1n << BigInt(i))) !== 0n,
              );
              const bit = first === undefined ? 0n : 1n << BigInt(first);
              const inspected = first === undefined ? 0xffffffffffffffffn : (bit << 1n) - 1n;
              data.set(
                little([available, window, bit, inspected, window ^ bit, BigInt(first ?? 64)]),
                0x10,
              );
              same(
                cpu.readPhysicalMemory(0xa000n, data.length),
                data,
                'allocation, remaining candidates and guards',
              );
              equal(cpu.readTimeState().tsc - clock, result.retired, 'completed guest work');
              equal(
                result.state.registers[15],
                BigInt(optimized),
                'CPUID selected the requested available path',
              );
            },
          );
        }
      }
    },
  };
}
