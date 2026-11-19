# ring64

an x86 and x86-64 cpu emulator for the browser

## installation

requires node 22.12 or newer and rustup.

```sh
npm ci
npm run build
```

## usage

this Node.js example runs `mov eax, 1`. the runtime requires WebAssembly SIMD support.

```js
import { readFile } from 'node:fs/promises';
import { createCpu } from './build/host/index.js';

const cpu = await createCpu({
  compiler: await readFile('build/ring64.wasm'),
  code: { base: 0x1000n, bytes: Uint8Array.of(0xb8, 1, 0, 0, 0) },
  ram: { base: 0x8000n, bytes: new Uint8Array(32) },
  limits: {
    maxInstructions: 128,
    maxSourceBytes: 2048,
    maxArtifactBytes: 262144,
    maxCachedArtifacts: 32,
    maxCachedBytes: 4194304,
    maxTranslationsPerRun: 32,
  },
});

try {
  const result = await cpu.run({ budget: 2n, stop: 0x1005n });
  console.log(result.reason, result.state.registers[0]); // stop, 1n
} finally {
  cpu.dispose();
}
```

## parallel vCPUs

`createMachine` from `build/host/machine.js` takes `build/ring64-parallel.wasm`,
one explicit guest memory map, and a `cpus` array with the initial entry and
execution context of each vCPU. Use `sharedRamCapacity` to reserve the backing
arena in multiples of a Wasm page. In a browser, serve the page and its workers
in a cross-origin isolated context. Node uses worker threads.

```js
const results = await Promise.all(
  machine.cpus.map((cpu) => cpu.run({ budget: 100_000n })),
);
```

Each handle owns its registers, queued interrupts and execution lifecycle.
The machine owns backing allocations and mappings. Its memory methods return
copies, and its writes are visible to all vCPUs. Call `await machine.dispose()`
when the embedding is finished. Use `createCpu` and `build/ring64.wasm` for the
private single-vCPU runtime.
