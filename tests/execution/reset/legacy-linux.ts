import type { Cpu, ExecutionContextSnapshot, Options, PagingState } from '../../../host/index.js';
import { LIMITS } from '../../support/fixture.js';
import { same } from '../far-control/fixture.js';
import { createCpu, equal, hex, put, resetContext, type Scenario } from './fixture.js';

// Linux v0.11, archive 7aadf253807264ad5561479e2a8b9de9602f966d.
// boot/setup.s at end_move and boot/head.s at zero, with the original instructions
// and tables reassembled. The rest of each image is zero-filled.
const setup =
  'b820908ed80f011e94000f01169a00e85f00b0d1e664e85800b0dfe660e85100b011e620eb00eb00e6a0eb00eb00b020e621eb00eb00b028e6a1eb00eb00b004e621eb00eb00b002e6a1eb00eb00b001e621eb00eb00e6a1eb00eb00b0ffe621eb00eb00e6a1b801000f01f0ea00000800eb00eb00e464a80275f6c30000000000000000ff070000009ac000ff0700000092c00000000000000000087c020900';
const head = [
  [
    0x0000,
    'b8100000008ed88ec08ee08ee80fb225007f0000e85a000000e885000000b8100000008ed88ec08ee08ee80fb225007f000031c040a30000000039050000100074f20f20c0251100008083c8020f22c0e805000000e9a6530000dbe39bdfe03c00740d0f20c083f0060f22c0c30f1f00dbe4c38d1528540000b8000008006689d066ba008e8d3db8540000b900010000890789570483c7084975f50f011daa540000c30f0115b2540000c3',
  ],
  [
    0x5400,
    '6a006a006a0068125400006800800000eb3eebfe556e6b6e6f776e20696e746572727570740a0d005051521e060fa0b8100000008ed88ec08ee06814540000e8bd2b0000580fa1071f5a5958cf0f1f00b90014000031c031fffcf3abc7050000000007100000c7050400000007200000c7050800000007300000c7050c00000007400000bffc4f0000b807f0ff00fdab2d001000007df831c00f22d80f20c00d000000800f22c0c30000ff07b85400000000ff07b85c',
  ],
  [0x5cc0, 'ff0f0000009ac000ff0f00000092c0'],
] as const;

const entry = 0x8000n;
const RAM_SIZE = 0x110000;
const paging = { cr0: 0x10n, cr2: 0n, cr3: 0n, cr4: 0n, efer: 0n };
const PORTS = [
  ['read', 0x64, 8],
  ['write', 0x64, 8, 0xd1],
  ['read', 0x64, 8],
  ['write', 0x60, 8, 0xdf],
  ['read', 0x64, 8],
  ...[
    [0x20, 0x11],
    [0xa0, 0x11],
    [0x21, 0x20],
    [0xa1, 0x28],
    [0x21, 4],
    [0xa1, 2],
    [0x21, 1],
    [0xa1, 1],
    [0x21, 0xff],
    [0xa1, 0xff],
  ].map(([port, byte]) => ['write', port, 8, byte]),
];

async function linuxCpu(
  compiler: WebAssembly.Module,
  ram: Uint8Array,
  context: ExecutionContextSnapshot,
  paging: PagingState,
  ip: bigint,
  io?: Options['io'],
) {
  return createCpu({
    compiler,
    memoryModel: 'physical',
    context,
    paging,
    entry: ip,
    ...(io && { io }),
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
    memoryLimits: { maxBackings: 1, maxMappings: 1, maxBackingBytes: ram.length },
    limits: { ...LIMITS, maxTranslationsPerRun: 1000, maxInstructions: 64, maxSourceBytes: 256 },
  });
}

export async function createLegacyLinuxHandoff(compiler: WebAssembly.Module) {
  const ram = new Uint8Array(RAM_SIZE);
  for (const [offset, bytes] of head) ram.set(hex(bytes), offset);
  ram.set(hex(setup), 0x90200);
  put(ram, 0x7f00, 0x7e00n, 4);
  put(ram, 0x7f04, 0x10n, 2);
  ram.set(hex('b84bf1aca7'), Number(entry));
  const context = resetContext();
  Object.assign(context.segments.cs, { selector: 0x9020, base: 0x90200n });
  Object.assign(context.segments.ss, { selector: 0x9000, base: 0x90000n });
  const ports: (string | number)[][] = [];
  const cpu = await linuxCpu(compiler, ram, context, paging, 0n, {
    read(port, width) {
      ports.push(['read', port, width]);
      return 0;
    },
    write(port, width, value) {
      ports.push(['write', port, width, value]);
    },
  });
  const registers = Array<bigint>(16).fill(0n);
  registers[4] = 0xff00n;
  cpu.setState({ registers, flags: 2n, rip: 0n });
  return { cpu, ports };
}

function dwordView(cpu: Cpu, offset: bigint, count: number): DataView {
  const bytes = cpu.readPhysicalMemory(offset, count * 4);
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
}

function landed(cpu: Cpu) {
  const context = cpu.readContext();
  equal(context.mode, 'protected', 'startup mode');
  equal(context.cpl, 0, 'startup privilege');
  const cs = context.segments.cs;
  equal(cs.selector, 8, 'far transfer loaded CS');
  equal(cs.base, 0n, 'far transfer loaded code base');
  equal(cs.limit, 0x7fffff, 'startup CS cache retains the setup GDT limit');
  equal(cs.default32, true, 'startup has 32-bit defaults');
  equal(cs.accessed, true, 'load acquired accessed CS');
  same(
    Array.from(cpu.readPhysicalMemory(0x90284n, 8)),
    Array.from(hex('ff070000009bc000')),
    'setup GDT code accessed bit',
  );
}

export function legacyLinuxScenarios(compiler: WebAssembly.Module): Scenario[] {
  return [
    {
      name: 'original Linux setup and startup reach paged C entry with the exact legacy handoff',
      async run() {
        for (const resumed of [false, true]) {
          const { cpu, ports } = await createLegacyLinuxHandoff(compiler);
          try {
            if (resumed) {
              const ctx = cpu.readContext();
              const lmsw = await cpu.run({ budget: 66n });
              equal(lmsw.reason, 'budget', 'run through LMSW');
              equal(lmsw.retired, 66n, 'LMSW included in count');
              equal(lmsw.state.rip, 0x6cn, 'far jump is next');
              const interim = cpu.readContext(),
                bank = cpu.readPagingState();
              same(bank, { ...paging, cr0: 0x11n }, 'PE changes without enabling paging');
              equal(interim.mode, 'protected', 'PE changes the context before the far jump');
              same(interim.segments.cs, ctx.segments.cs, 'old CS cache survives setting PE');
              same(interim.segments.ss, ctx.segments.ss, 'old SS cache survives setting PE');
              const restored = await linuxCpu(
                compiler,
                cpu.readPhysicalMemory(0n, RAM_SIZE),
                interim,
                bank,
                lmsw.state.rip,
              );
              try {
                restored.setState(lmsw.state);
                same(restored.readContext(), interim, 'intermediate snapshot imported');
                equal(
                  (await restored.run({ budget: 1n })).state.rip,
                  0n,
                  'far jump after reconstruction',
                );
                landed(restored);
              } finally {
                restored.dispose();
              }
              const far = await cpu.run({ budget: 1n });
              equal(far.retired, 1n, 'only the far jump retired');
              equal(far.state.rip, 0n, 'protected startup entry');
              landed(cpu);
            }
            const handoff = await cpu.run({ budget: 30000n, repeatBudget: 100000n, stop: entry });
            equal(handoff.reason, 'stop', 'original assembly reached C entry');
            equal(handoff.state.rip, entry, 'linked C entry PC');
            equal(handoff.retired, resumed ? 13701n - 67n : 13701n, 'original guest retirements');
            equal(handoff.repeatIterations, 5120n, 'five page-table pages cleared by REP STOSL');
            equal(cpu.readTimeState().tsc, 13701n, 'retirement clock at the handoff');
            equal(handoff.state.registers[4], 0x7df0n, 'C entry stack pointer');
            same(
              Array.from(cpu.readPhysicalMemory(0x7df0n, 16)),
              Array.from(hex('12540000000000000000000000000000')),
              'return and three entry arguments',
            );
            same(
              cpu.readPagingState(),
              { ...paging, cr0: 0x80000013n },
              'legacy non-PAE control bank',
            );
            landed(cpu);
            same(ports, PORTS, 'ordered byte-size 8042 and PIC actions');
            const context = cpu.readContext();
            same(context.gdtr, { base: 0x5cb8n, limit: 0x7ff }, 'startup GDT');
            same(context.idtr, { base: 0x54b8n, limit: 0x7ff }, 'startup IDT');
            same(
              Array.from(cpu.readPhysicalMemory(context.gdtr.base, 24)),
              Array.from(hex('0000000000000000ff0f0000009ac000ff0f00000093c000')),
              'current GDT fields',
            );
            same(
              Array.from(cpu.readPhysicalMemory(context.idtr.base, 2048)),
              Array.from(hex('28540800008e0000'.repeat(256))),
              'all startup IDT gates',
            );
            const reset = cpu.readX87State();
            equal(reset.controlWord, 0x37f, 'startup FNINIT defaults');
            equal(reset.tagWord, 0xffff, 'empty startup x87 stack');
            equal(reset.statusWord, 0, 'no pending x87 condition');
            equal(
              reset.instructionPointer | reset.dataPointer | BigInt(reset.opcode),
              0n,
              'x87 no-op kept environment pointers',
            );
            const tables = dwordView(cpu, 0n, 5120);
            for (let i = 0; i < 1024; i++) {
              const pde = i < 4 ? ((i + 1) << 12) | 7 | (i === 0 ? 0x20 : 0) : 0;
              equal(tables.getUint32(i * 4, true), pde, `PDE ${i}`);
            }
            for (let i = 0; i < 4096; i++) {
              const pte = (i << 12) | 7 | (i === 5 || i === 7 ? 0x20 : 0);
              equal(tables.getUint32(4096 + i * 4, true), pte, `PTE ${i}`);
            }
            const first = await cpu.run({ budget: 1n });
            equal(first.retired, 1n, 'fixture-owned sentinel retired');
            equal(first.state.rip, 0x8005n, 'sentinel successor PC');
            equal(first.state.registers[0], 0xa7acf14bn, 'reached paged instruction');
            equal(dwordView(cpu, 0x1020n, 1).getUint32(0, true), 0x8027, 'paged entry A bit');
            equal(cpu.readTimeState().tsc, 13702n, 'sentinel added one retirement');
          } finally {
            cpu.dispose();
          }
        }
      },
    },
  ];
}
