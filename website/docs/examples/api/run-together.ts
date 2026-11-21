import type { Machine } from 'ring64/parallel';

export function runTogether(machine: Machine, stop: bigint) {
  const options = { budget: 1_000n, stop };
  return Promise.all(machine.cpus.map((cpu) => cpu.run(options)));
}
