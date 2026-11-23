import { createCpu, type Cpu, type MemoryModel } from '../../../../../sdk/src/index.js';
import { protectedSnapshot } from '../../../../support/context.js';
import { initial, LIMITS } from '../../../../support/fixture.js';

export type Source = 'direct' | 'gdt' | 'ldt';
export interface GateCase {
  source: Source;
  transfer: 'jump' | 'call' | 'ret';
  cpl?: 0 | 3;
  outer?: number;
  target?: number;
  gateAccess?: number;
  tssAccess?: number;
  tssLimit?: number;
  indirect?: boolean;
  word?: boolean;
  noLdt?: boolean;
  noTr?: boolean;
  model?: MemoryModel;
}

export const PC = 0x4000n,
  B_PC = 0x4100n,
  STACK = 0x6000n;
const GDT = 0x1000,
  LDT = 0x2000,
  A_TSS = 0x3000,
  B_TSS = 0x3100,
  POINTER = 0x4300;

function descriptor(base: number, limit: number, access: number) {
  // Every fixture system descriptor lies below 64 KiB.
  return (BigInt(access) << 40n) | (BigInt(base) << 16n) | BigInt(limit);
}

export async function withFarGate(
  compiler: WebAssembly.Module,
  test: GateCase,
  run: (cpu: Cpu, next: bigint, read: (at: number, width: number) => number) => Promise<void>,
) {
  const model = test.model ?? 'physical',
    cpl = test.cpl ?? 3;
  const ram = new Uint8Array(0xe000),
    view = new DataView(ram.buffer);
  const put64 = (at: number, value: bigint) => view.setBigUint64(at, value, true);
  const code = 0x00cf9b000000ffffn,
    data = 0x00cf93000000ffffn;
  put64(GDT + 8, code);
  put64(GDT + 16, data);
  put64(GDT + 0x18, descriptor(A_TSS, 0x67, 0x8b));
  put64(GDT + 0x20, descriptor(B_TSS, test.tssLimit ?? 0x67, test.tssAccess ?? 0x89));
  put64(GDT + 0x28, code | (3n << 45n));
  put64(GDT + 0x30, data | (3n << 45n));
  put64(GDT + 0x40, descriptor(LDT, 0x1f, 0x82));
  const gate = (BigInt(test.gateAccess ?? 0xe5) << 40n) | (BigInt(test.target ?? 0x23) << 16n);
  put64(GDT + 0x38, gate);
  put64(LDT + 8, gate);

  view.setUint16(A_TSS, 0x7171, true);
  view.setUint16(A_TSS + 96, 0x40, true); // Static LDTR when B returns to A.
  view.setUint16(B_TSS, 0x8282, true);
  for (const [at, value] of [
    [32, Number(B_PC)],
    [36, 2],
    [40, 0x22334455],
    [56, 0x5000],
    [72, 16],
    [76, 8],
    [80, 16],
    [84, 16],
  ] as const)
    view.setUint32(B_TSS + at, value, true);
  ram[Number(B_PC)] = 0xcf; // IRETD

  const selector = test.outer ?? ({ direct: 0x20, gdt: 0x3b, ldt: 0x0f } as const)[test.source];
  const operandWidth = test.word ? 2 : 4;
  const operand = [...Array<number>(operandWidth).fill(0xef), selector & 255, selector >>> 8];
  const opcode =
    test.transfer === 'ret'
      ? [0xcb]
      : test.indirect
        ? [0xff, test.transfer === 'call' ? 0x1f : 0x2f] // [EDI]
        : [test.transfer === 'call' ? 0x9a : 0xea, ...operand];
  if (test.word) opcode.unshift(0x66);
  ram.set(opcode, Number(PC));
  ram.set(operand, POINTER);
  view.setUint32(Number(STACK), 0x1234, true);
  view.setUint32(Number(STACK) + 4, selector, true);
  const context = protectedSnapshot();
  context.cpl = cpl;
  if (model === 'ia32e-paged') context.mode = 'compatibility';
  for (const [name, seg] of Object.entries(context.segments)) {
    const index = name === 'cs' ? (cpl ? 0x28 : 8) : cpl ? 0x30 : 16;
    seg.selector = index | cpl;
    seg.dpl = cpl;
  }
  context.gdtr = { base: BigInt(GDT), limit: 0x47 };
  if (!test.noLdt)
    context.ldtr = {
      selector: 0x40,
      valid: true,
      base: BigInt(LDT),
      limit: 0x1f,
      attributes: 0x82,
    };
  if (!test.noTr)
    context.tr = {
      selector: 0x18,
      valid: true,
      base: BigInt(A_TSS),
      limit: 0x67,
      attributes: 0x8b,
    };
  if (model === 'ia32e-paged') {
    put64(0x9000, 0xa007n);
    put64(0xa000, 0xb007n);
    put64(0xb000, 0xc007n);
    for (let page = 0; page < 8; page++) put64(0xc000 + page * 8, BigInt(page * 4096 + 7));
  }
  const cpu = await createCpu({
    compiler,
    context,
    limits: LIMITS,
    entry: PC,
    ...(model === 'linear'
      ? { memoryModel: model }
      : {
          memoryModel: model,
          paging:
            model === 'physical'
              ? { cr0: 0x33n, cr2: 0n, cr3: 0n, cr4: 0n, efer: 0n }
              : { cr0: 0x80000033n, cr2: 0n, cr3: 0x9000n, cr4: 0x628n, efer: 0x500n },
        }),
    memory: {
      backings: [{ id: 'ram', bytes: ram }],
      mappings: [
        {
          backing: 'ram',
          base: 0n,
          offset: 0,
          length: ram.length,
          permissions: { read: true, write: true, execute: true },
        },
      ],
    },
    memoryLimits: { maxBackings: 1, maxMappings: 1, maxBackingBytes: ram.length },
  });
  try {
    cpu.setState(initial(PC, { 0: 0x11112222n, 4: STACK, 7: BigInt(POINTER) }));
    await run(cpu, PC + BigInt(opcode.length), (at, width) => {
      const b =
        model === 'linear'
          ? cpu.readMemory(BigInt(at), width)
          : cpu.readPhysicalMemory(BigInt(at), width);
      const d = new DataView(b.buffer, b.byteOffset, b.byteLength);
      if (width === 1) return d.getUint8(0);
      if (width === 2) return d.getUint16(0, true);
      return d.getUint32(0, true);
    });
  } finally {
    cpu.dispose();
  }
}

export const addresses = {
  oldBusy: GDT + 0x1d,
  newBusy: GDT + 0x25,
  oldLink: A_TSS,
  newLink: B_TSS,
  savedPc: A_TSS + 32,
};
