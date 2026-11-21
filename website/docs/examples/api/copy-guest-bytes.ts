import type { Cpu } from 'ring64';

export function copyGuestBytes(cpu: Cpu, from: bigint, to: bigint, length: number) {
  const bytes = cpu.readMemory(from, length);
  cpu.writeMemory(to, bytes);
}
