import { createCpu, loadCpuCompiler, type Limits, type Options, type RunResult } from 'ring64';
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

function checkFetchFault(result: RunResult): void {
  if (result.reason !== 'fetch-fault' || result.fault.address !== run.stop)
    throw new Error('expected a fetch fault after the end of code');
}

// Some hosts accept both memory types but reject memory.copy between them.
const memory64Probe = [
  [0, 97, 115, 109, 1, 0, 0, 0],
  [1, 4, 1, 0x60, 0, 0], // type () -> ()
  [3, 2, 1, 0], // one function of that type
  [5, 5, 2, 0, 0, 4, 0], // one memory32 and one memory64
  [10, 33, 1, 31, 0], // one function body with no locals
  [0x41, 0, 0x42, 0, 0x41, 0, 0xfc, 10, 0, 1], // copy into memory32
  [0x42, 0, 0x41, 0, 0x41, 0, 0xfc, 10, 1, 0], // copy into memory64
  [0x42, 0, 0x41, 0, 0x42, 0, 0xfc, 11, 1], // fill memory64
  [0x0b],
];
export const supportsWide = WebAssembly.validate(Uint8Array.from(memory64Probe.flat()));

export async function checkCpu(profile?: 'memory32' | 'memory64'): Promise<void> {
  const compiler = await loadCpuCompiler(profile);
  const expectWide = profile ? profile === 'memory64' : supportsWide;
  if (
    WebAssembly.Module.imports(compiler).some((entry) => entry.module === 'guest64') !== expectWide
  )
    throw new Error('unexpected CPU compiler memory profile');
  const options = {
    compiler,
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
    checkFetchFault(await cpu.run({ budget: 1n }));
  } finally {
    cpu.dispose();
  }
}

export async function checkParallel(profile?: 'memory32' | 'memory64'): Promise<void> {
  const { createMachine, loadParallelCompiler } = await import('ring64/parallel');
  const compiler = await loadParallelCompiler(profile);
  const machineOptions = {
    compiler,
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
    for (const result of await Promise.all(machine.cpus.map((vcpu) => vcpu.run({ budget: 1n }))))
      checkFetchFault(result);
  } finally {
    await machine.dispose();
  }
}
