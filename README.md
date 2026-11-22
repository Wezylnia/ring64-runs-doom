# ring64

the fastest x86 and x86-64 cpu emulator for JavaScript runtimes

## install

```sh
npm install ring64
```

## usage

runs `mov eax, 1`. requires WebAssembly SIMD.

```js
import { createCpu, loadCpuCompiler } from 'ring64';

const entry = 0x1000n;
const program = Uint8Array.of(0xb8, 1, 0, 0, 0);
const run = { budget: 2n, stop: entry + BigInt(program.length) };
const limits = {
  maxInstructions: 128,
  maxSourceBytes: 2048,
  maxArtifactBytes: 262144,
  maxCachedArtifacts: 32,
  maxCachedBytes: 4194304,
  maxTranslationsPerRun: 32,
};

const cpu = await createCpu({
  compiler: await loadCpuCompiler(),
  code: { base: entry, bytes: program },
  ram: { base: 0x8000n, bytes: new Uint8Array(32) },
  limits,
});

try {
  const result = await cpu.run(run);
  console.log(result.reason, result.state.registers[0]); // stop, 1n
} finally {
  cpu.dispose();
}
```

## parallel

reuses the program, limits and run options above. browser workers require a cross-origin isolated page.

```js
import { createMachine, loadParallelCompiler } from 'ring64/parallel';

const machine = await createMachine({
  compiler: await loadParallelCompiler(),
  limits,
  memoryModel: 'linear',
  sharedRamCapacity: 4096,
  memoryLimits: { maxBackings: 1, maxMappings: 1, maxBackingBytes: program.length },
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
  cpus: [{ entry }, { entry }],
});

try {
  const results = await Promise.all(machine.cpus.map((cpu) => cpu.run(run)));
  console.log(results.map((result) => result.state.registers[0])); // [1n, 1n]
} finally {
  await machine.dispose();
}
```
