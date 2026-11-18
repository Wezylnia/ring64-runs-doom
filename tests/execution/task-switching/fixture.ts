import { createCpu, type Cpu } from '../../../host/index.js';
import { protectedSnapshot } from '../../support/context.js';
import { equal, LIMITS } from '../../support/fixture.js';
import { image } from './program.js';

export { check, equal, throws, type Scenario } from '../../support/fixture.js';
export { same } from '../far-control/fixture.js';

// Physical/linear addresses from the flat NASM listing in repair.asm.
export const CALL = 0x100131n,
  RESUME_A = 0x100138n,
  ENTRY_B = 0x1001e0n,
  ENTRY_C = 0x1002b3n;
export const IRET_B = 0x1002adn,
  IRET_C = 0x10036cn,
  AFTER_EXIT = 0x10037an;
export const GDT = 0x100398n,
  TSS_A = 0x100430n,
  TSS_B = 0x100498n,
  TSS_C = 0x100500n;
export const PD_A = 0x20000n,
  PD_B = 0x21000n,
  PAGE_A = 0x24000n,
  PAGE_B = 0x25000n;
export type PortWrite = [number, number, number];

export async function withGuest(
  compiler: WebAssembly.Module,
  run: (cpu: Cpu, ports: PortWrite[]) => Promise<void>,
): Promise<void> {
  const ram = new Uint8Array(0x110000);
  ram.set(image, 0x100000);
  const ports: PortWrite[] = [];
  const cpu = await createCpu({
    compiler,
    entry: BigInt(new DataView(image.buffer, image.byteOffset).getUint32(0x1c, true)),
    memoryModel: 'physical',
    context: protectedSnapshot(),
    paging: { cr0: 0x33n, cr2: 0n, cr3: 0n, cr4: 0n, efer: 0n },
    io: {
      read() {
        throw new Error('unexpected port read in task-switch guest');
      },
      write(port, width, value) {
        ports.push([port, width, value]);
      },
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
    memoryLimits: { maxBackings: 1, maxMappings: 3, maxBackingBytes: ram.length },
    limits: {
      ...LIMITS,
      maxCachedArtifacts: 256,
      maxCachedBytes: 8 * 1024 * 1024,
      maxTranslationsPerRun: 512,
    },
  });
  try {
    await run(cpu, ports);
  } finally {
    cpu.dispose();
  }
}

export function dword(cpu: Cpu, address: bigint): number {
  const b = cpu.readPhysicalMemory(address, 4);
  return new DataView(b.buffer, b.byteOffset, b.byteLength).getUint32(0, true);
}

export function markers(text: string): PortWrite[] {
  return Array.from(text, (s) => [0xe9, 8, s.charCodeAt(0)]);
}

export async function toCaller(cpu: Cpu): Promise<void> {
  const result = await cpu.run({ budget: 20000n, repeatBudget: 7168n, stop: CALL });
  equal(result.reason, 'stop', 'reached outgoing CALL');
  equal(result.state.rip, CALL, 'outgoing EIP');
  equal(cpu.readContext().tr.selector, 0x18, 'LTR installed A');
  equal(cpu.readPagingState().cr3, PD_A, 'bootstrap switched on non-PAE paging');
}

export function write32(cpu: Cpu, address: bigint, value: number): void {
  const bytes = new Uint8Array(4);
  new DataView(bytes.buffer).setUint32(0, value, true);
  cpu.writePhysicalMemory(address, bytes);
}
