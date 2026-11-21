import type { Cpu } from 'ring64';

export async function runTo(cpu: Cpu, stop: bigint) {
  const result = await cpu.run({ budget: 1_000n, stop });
  if (result.reason !== 'stop') throw new Error(result.reason);
  return result.state.registers[0]; // rax
}
