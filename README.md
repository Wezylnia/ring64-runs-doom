<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="./assets/logo-dark.svg">
    <img alt="ring64" src="./assets/logo.svg" width="240">
  </picture>
</h1>

the fastest x86 and x86-64 cpu emulator for JavaScript runtimes

[ring64.fatihguzel.dev](https://ring64.fatihguzel.dev) · [docs](https://ring64.fatihguzel.dev/docs/)

## install

```sh
npm install ring64
```

## quickstart

run two x86-64 instructions and read `42n` from `rax`. save as `add.ts` and run `node add.ts`.

```ts
import { createCpu, loadCpuCompiler, type Limits } from 'ring64';

const entry = 0x1000n;
// mov eax, 40; add eax, 2
const code = Uint8Array.of(0xb8, 0x28, 0x00, 0x00, 0x00, 0x83, 0xc0, 0x02);
const limits = {
  maxInstructions: 128,
  maxSourceBytes: 2048,
  maxArtifactBytes: 262144,
  maxCachedArtifacts: 32,
  maxCachedBytes: 4194304,
  maxTranslationsPerRun: 32,
} satisfies Limits;

const cpu = await createCpu({
  compiler: await loadCpuCompiler(),
  code: { base: entry, bytes: code },
  ram: { base: 0x8000n, bytes: new Uint8Array(32) },
  limits,
});

try {
  const result = await cpu.run({ budget: 10n, stop: entry + BigInt(code.length) });
  if (result.reason !== 'stop') throw new Error(`guest stopped with ${result.reason}`);
  console.log(result.state.registers[0]); // rax = 42n
} finally {
  cpu.dispose();
}
```

## parallel

run each vcpu on its own worker with shared guest memory. pass a machine created by `createMachine` and the guest address where each vcpu should stop.

```ts
import type { Machine } from 'ring64/parallel';

export function runTogether(machine: Machine, stop: bigint) {
  const options = { budget: 1_000n, stop };
  return Promise.all(machine.cpus.map((cpu) => cpu.run(options)));
}
```

[complete parallel example](https://ring64.fatihguzel.dev/docs/#parallel-example)
