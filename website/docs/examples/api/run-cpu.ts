import { createCpu, type Options } from 'ring64';

export async function runCpu(options: Options, stop: bigint) {
  const cpu = await createCpu(options);
  try {
    return await cpu.run({ budget: 1_000n, stop });
  } finally {
    cpu.dispose();
  }
}
