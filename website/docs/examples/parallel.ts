import type { Limits } from 'ring64';
import { createMachine, loadParallelCompiler } from 'ring64/parallel';

const entry = 0x1000n;
const code = Uint8Array.of(0x48, 0x89, 0xf8, 0x48, 0x01, 0xf0); // mov rax, rdi; add rax, rsi
const inputs = [
  [19n, 23n],
  [5n, 7n],
] as const;
const limits = {
  maxInstructions: 128,
  maxSourceBytes: 2048,
  maxArtifactBytes: 262144,
  maxCachedArtifacts: 32,
  maxCachedBytes: 4194304,
  maxTranslationsPerRun: 32,
} satisfies Limits;

const machine = await createMachine({
  compiler: await loadParallelCompiler(),
  limits,
  memoryModel: 'linear',
  memory: {
    backings: [{ id: 'code', bytes: code }],
    mappings: [
      {
        base: entry,
        backing: 'code',
        offset: 0,
        length: code.length,
        permissions: { read: true, write: false, execute: true },
      },
    ],
  },
  memoryLimits: { maxBackings: 1, maxMappings: 1, maxBackingBytes: code.length },
  sharedRamCapacity: 4096,
  cpus: inputs.map(() => ({ entry })),
});

try {
  const sums = await Promise.all(
    machine.cpus.map(async (cpu) => {
      const input = inputs[cpu.id];
      if (!input) throw new Error(`missing input for vcpu ${cpu.id}`);
      const state = await cpu.readState();
      state.registers[7] = input[0]; // rdi
      state.registers[6] = input[1]; // rsi
      await cpu.setState(state);

      const result = await cpu.run({ budget: 10n, stop: entry + BigInt(code.length) });
      if (result.reason !== 'stop') throw new Error(`guest stopped with ${result.reason}`);
      return result.state.registers[0];
    }),
  );
  console.log(sums); // [42n, 12n]
} finally {
  await machine.dispose();
}
