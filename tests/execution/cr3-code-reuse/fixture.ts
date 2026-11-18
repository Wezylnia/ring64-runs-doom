import { createCpu, type Cpu } from '../../../host/index.js';
import { longSnapshot, protectedSnapshot } from '../../support/context.js';
import { LIMITS, equal, hex } from '../../support/fixture.js';
import { little } from '../legacy-paging/fixture.js';

export type Family = 'non-PAE' | 'PAE' | 'IA32e';
export const PC = 0x1000n,
  ROOT_A = 0x1000n,
  ROOT_B = 0x5000n;
export const CODE_A = 0x9000,
  CODE_B = 0xa000,
  NEXT_A = 0xb000,
  NEXT_B = 0xc000;
export const SHARED_PC = 0x7000n,
  SHARED = 0x11000;
export { check, equal, hex, type Scenario } from '../../support/fixture.js';
export { little, word } from '../legacy-paging/fixture.js';
export type { Cpu } from '../../../host/index.js';

export async function withRoots(
  compiler: WebAssembly.Module,
  family: Family,
  run: (cpu: Cpu) => Promise<void>,
  setup: {
    capacity?: number;
    translations?: number;
    restricted?: boolean;
    pcide?: boolean;
    pge?: boolean;
    prepare?: (ram: Uint8Array) => void;
  } = {},
): Promise<void> {
  const ram = new Uint8Array(0x14000);
  const width = family === 'non-PAE' ? 4 : 8;
  for (const [root, code, next, data, other] of [
    [Number(ROOT_A), CODE_A, NEXT_A, 0xf000, ROOT_B],
    [Number(ROOT_B), CODE_B, NEXT_B, 0x10000, ROOT_A],
  ] as const) {
    const table = root + 0x3000;
    if (family === 'non-PAE') ram.set(little(BigInt(table + 0x63), 4), root);
    else if (family === 'PAE') {
      ram.set(little(BigInt(root + 0x1001)), root);
      ram.set(little(BigInt(table + 0x63)), root + 0x1000);
    } else {
      ram.set(little(BigInt(root + 0x1063)), root);
      ram.set(little(BigInt(root + 0x2063)), root + 0x1000);
      ram.set(little(BigInt(table + 0x63)), root + 0x2000);
    }
    // 1–2 are private executable pages, 3 aliases A code, 4 aliases A's leaf table.
    for (const [index, page] of [code, next, CODE_A, 0x4000, 0x8000, data, SHARED].entries())
      ram.set(little(BigInt(page + 0x63), width), table + (index + 1) * width);
    ram.fill(0x82, code, code + 0x1000);
    ram.set(hex('8b142500600000b9'), code); // mov edx,[0x6000]; mov ecx,imm32
    ram.set(little(code === CODE_A ? 0x110011aan : 0x220022bbn, 4), code + 8);
    ram[code + 12] = 0xb8; // mov eax,other CR3; mov cr3,eax; jmp PC
    ram.set(little(other, 4), code + 13);
    ram.set(hex('0f22d8ebea'), code + 17);
    ram.set(little(code === CODE_A ? 0xaabbccddn : 0x44332211n, 4), data);
  }
  ram.set(hex('0f22d882'), SHARED); // standalone guest MOV CR3,EAX
  setup.prepare?.(ram);
  const ia32e = family === 'IA32e';
  const cpu = await createCpu({
    compiler,
    memoryModel: setup.restricted ? 'ia32e-paged' : 'physical',
    entry: PC,
    context: ia32e ? longSnapshot() : protectedSnapshot(),
    paging: {
      cr0: 0x80010033n,
      cr2: 0xabcden,
      cr3: ROOT_A,
      cr4:
        (family === 'non-PAE' ? 0x608n : 0x628n) |
        (setup.pcide ? 1n << 17n : 0n) |
        (setup.pge ? 128n : 0n),
      efer: ia32e ? 0xd00n : family === 'PAE' ? 0x800n : 0n,
      ...(family === 'PAE'
        ? { pdpte: [0x2001n, 0n, 0n, 0n] as [bigint, bigint, bigint, bigint] }
        : {}),
    },
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
    memoryLimits: { maxBackings: 1, maxMappings: 4, maxBackingBytes: ram.length },
    limits: {
      ...LIMITS,
      maxCachedArtifacts: setup.capacity ?? 8,
      maxTranslationsPerRun: setup.translations ?? LIMITS.maxTranslationsPerRun,
    },
  });
  try {
    await run(cpu);
  } finally {
    cpu.dispose();
  }
}

export function at(cpu: Cpu, pc: bigint, eax?: bigint): void {
  const state = cpu.readState();
  if (eax !== undefined) state.registers[0] = eax;
  cpu.setState({ ...state, rip: pc });
}

export async function switchRoot(cpu: Cpu, value: bigint): Promise<void> {
  at(cpu, SHARED_PC, value);
  const switched = await cpu.run({ budget: 1n });
  equal(switched.reason, 'budget', 'guest MOV CR3');
  equal(switched.retired, 1n, 'one root writer');
  equal(cpu.readPagingState().cr3, value & 0x7fff_ffff_ffff_ffffn, 'installed raw CR3');
}
