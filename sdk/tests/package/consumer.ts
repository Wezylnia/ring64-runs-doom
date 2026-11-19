import { createCpu, loadCpuCompiler, type Limits, type Options } from 'ring64';
import type { MachineOptions } from 'ring64/parallel';

const entry = 0x1000n;
const program = Uint8Array.of(0xb8, 0x2a, 0, 0, 0);
const run = { budget: 2n, stop: entry + BigInt(program.length) };
const limits: Limits = {
  maxInstructions: 128,
  maxSourceBytes: 2048,
  maxArtifactBytes: 262144,
  maxCachedArtifacts: 32,
  maxCachedBytes: 4194304,
  maxTranslationsPerRun: 32,
};

export async function checkCpu(): Promise<void> {
  const options = {
    compiler: await loadCpuCompiler(),
    code: { base: entry, bytes: program },
    ram: { base: 0x2000n, bytes: new Uint8Array(16) },
    limits,
  } satisfies Options;
  const invalid = await createCpu({ ...options, compiler: {} }).then(
    (cpu) => {
      cpu.dispose();
      return undefined;
    },
    (error: unknown) => error,
  );
  if (!(invalid instanceof TypeError) || !invalid.message.includes('compiler must be bytes'))
    throw new Error('standalone API accepted or misclassified an invalid compiler');
  const cpu = await createCpu(options);
  try {
    const result = await cpu.run(run);
    if (result.reason !== 'stop' || result.retired !== 1n || result.state.registers[0] !== 42n)
      throw new Error('unexpected standalone CPU result');
  } finally {
    cpu.dispose();
  }
}

export async function checkParallel(): Promise<void> {
  const { createMachine, loadParallelCompiler } = await import('ring64/parallel');
  const machineOptions = {
    compiler: await loadParallelCompiler(),
    limits,
    sharedRamCapacity: 4096,
    memoryModel: 'linear',
    memory: {
      backings: [{ id: 'code', bytes: program }],
      mappings: [
        {
          base: entry,
          backing: 'code',
          offset: 0,
          length: program.length,
          permissions: { read: true, write: false, execute: true },
        },
      ],
    },
    memoryLimits: { maxBackings: 1, maxMappings: 1, maxBackingBytes: 4096 },
    cpus: [{ entry }, { entry }],
  } satisfies MachineOptions;
  const machine = await createMachine(machineOptions);
  try {
    const results = await Promise.all(machine.cpus.map((vcpu) => vcpu.run(run)));
    if (
      results.length !== 2 ||
      results.some(
        (result) =>
          result.reason !== 'stop' || result.retired !== 1n || result.state.registers[0] !== 42n,
      )
    )
      throw new Error('unexpected parallel CPU result');
  } finally {
    await machine.dispose();
  }
}
