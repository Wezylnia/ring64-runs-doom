import { check, equal, type Scenario } from '../../support/fixture.js';
import { initial, little, PC, same, usingPaged } from '../paging/fixture.js';

// Clang -target x86_64-unknown-linux-gnu -O2 -mcx16:
// _Bool replace_pair(unsigned __int128 *p, unsigned __int128 old, unsigned __int128 next) {
//   return __atomic_compare_exchange_n(p, &old, next, 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST);
// }
const replacePair = '534889cb4889f04c89c1f0480fc70f0f94c05bc3';

// R8/R10 are record indices, R9/R11 are generations. Dispatch before choosing the head's layout.
const taggedHead = [
  'b8010000000fa24183ff10752c', // CPUID.1; CMP R15D,16; JNE compact
  'f7c1002000007424', // TEST ECX,CX16; JZ compact
  '4a8d04c64a8d1cd64c89ca4c89d9', // RAX/RBX=&records[old/new]; RDX/RCX=old/new generation
  'f0480fc70f410f94c4', // LOCK CMPXCHG16B [RDI]; SETZ R12B
  '488b074c8b3041bf10000000c3', // load selected pointer, dereference to R14; width 16, RET
  'f7c2000100007421', // compact: TEST EDX,CX8; JZ unavailable
  '4489c04489d34489ca4489d9', // EAX/EBX=old/new index; EDX/ECX=old/new generation
  'f00fc70f410f94c4', // LOCK CMPXCHG8B [RDI]; SETZ R12B
  '8b074c8b34c641bf08000000c3', // read selected index, load records[index] to R14; width 8, RET
  '4531ffc3', // neither feature available: width 0, RET
].join('');

const slot = PC + 0x1010n,
  sp = PC + 0x1100n,
  stop = PC + 0x3000n;

export function taggedHeadScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'original Clang mcx16 compare/exchange returns to its caller on matches and mismatches in either half',
      async run() {
        const lo = 0x8123456789abcdefn,
          hi = 0xfedcba9876543210n;
        const next = [0xabcdef0123456789n, 0xab3489fade789012n];
        for (const [oldLo, oldHi] of [
          [lo, hi],
          [lo, hi ^ (1n << 61n)],
          [lo + 1n, hi],
        ] as const) {
          const hit = oldLo === lo && oldHi === hi;
          const data = new Uint8Array(512).fill(0xa5);
          data.set(little([lo, hi]), 0x10);
          data.set(little([stop]), 0x100);
          await usingPaged(
            compiler,
            {
              code: replacePair,
              words: [[0x4008n, 0xa063n]],
              spans: [{ physical: 0xa000n, bytes: data }],
            },
            async (cpu) => {
              const input = initial(
                PC,
                { 1: next[0]!, 2: oldHi, 4: sp, 6: oldLo, 7: slot, 8: next[1]! },
                0xed7n,
              );
              cpu.setState(input);
              const result = await cpu.run({ budget: 16n, stop });
              equal(result.reason, 'stop', 'unmodified function returned');
              equal(result.retired, 8n, 'entire function including RET');
              equal(result.state.registers[0]! & 255n, BigInt(hit), 'SysV boolean result in AL');
              equal(result.state.registers[3], input.registers[3], 'callee-saved RBX');
              equal(result.state.registers[4], sp + 8n, 'caller stack');
              equal(result.state.flags, (input.flags & ~64n) | (hit ? 64n : 0n), 'only ZF changes');
              data.set(little([input.registers[3]!]), 0xf8); // PUSH RBX
              if (hit) data.set(little(next), 0x10);
              same(
                cpu.readPhysicalMemory(0xa000n, data.length),
                data,
                'slot, stack and untouched guards',
              );
            },
          );
        }
      },
    },
    {
      name: 'CPUID selects a compact index and generation or a wide pointer and generation for a tagged record head',
      async run() {
        for (const width of [8, 16]) {
          const generation = width === 16 ? 0x12345678fffffff0n : 0xfffffff0n;
          const records = PC + 0x1040n + (width === 16 ? 1n << 32n : 0n);
          const data = new Uint8Array(512).fill(0xa5);
          const values = [0n, 0xbbaa000011112222n, 0xddcc333344445555n];
          data.set(little(values), 0x40);
          data.set(little([stop]), 0x100);
          const head = (id: number, tag: bigint) =>
            width === 16
              ? little([records + BigInt(8 * id), tag])
              : little([(tag << 32n) | BigInt(id)]);
          data.set(head(1, generation), 0x10);
          await usingPaged(
            compiler,
            {
              code: taggedHead,
              // PDPT[4] gives the wide representation a genuine pointer above 4 GiB.
              words: [
                [0x2020n, 0x3007n],
                [0x4008n, 0xa063n],
              ],
              spans: [{ physical: 0xa000n, bytes: data }],
            },
            async (cpu) => {
              let id = 1,
                tag = generation;
              // Publish record 2, reject an obsolete generation with the right index, then restore record 1.
              const stale = width === 16 ? 1n << 36n : 1n << 20n;
              for (const [expected, replacement] of [
                [generation, 2],
                [(generation + 1n) ^ stale, 1],
                [generation + 1n, 1],
              ] as const) {
                const match = tag === expected;
                const next = tag + 1n;
                cpu.setState(
                  initial(PC, {
                    4: sp,
                    6: records,
                    7: slot,
                    8: BigInt(id),
                    9: expected,
                    10: BigInt(replacement),
                    11: next,
                    12: 0x77n,
                    15: BigInt(width),
                  }),
                );
                const result = await cpu.run({ budget: 32n, stop });
                equal(result.reason, 'stop', 'guest returned');
                equal(
                  result.state.registers[15],
                  BigInt(width),
                  'negotiated feature and representation',
                );
                equal(result.state.registers[12], BigInt(match), 'guest match result');
                equal(result.state.registers[4], sp + 8n, 'guest RET');
                if (match) {
                  id = replacement;
                  tag = next;
                }
                equal(result.state.registers[14], values[id], 'selected immutable record payload');
                data.set(head(id, tag), 0x10);
                same(
                  cpu.readPhysicalMemory(0xa000n, data.length),
                  data,
                  'entire head, records and guards',
                );
              }
              check(
                id === 1 && tag === generation + 2n,
                'two accepted publications and one rejection',
              );
            },
          );
        }
      },
    },
  ];
}
