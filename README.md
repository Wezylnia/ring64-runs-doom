# ring64

an x86 and x86-64 cpu emulator for the browser

## build

build from the repository with Node.js 22.12 or newer and rustup.

```sh
npm ci
npm run build
npm pack ./build/npm/ring64 --pack-destination ./build
```

in your app, run `npm install /path/to/ring64/build/ring64-0.1.0.tgz`.

## usage

runs `mov eax, 1`. requires WebAssembly SIMD.

```js
import { createCpu, loadCpuCompiler } from 'ring64';

const cpu = await createCpu({
  compiler: await loadCpuCompiler(),
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

## parallel

import `createMachine` and `loadParallelCompiler` from `ring64/parallel`.
browser workers require a cross-origin isolated page.
[example](https://github.com/fatihguzeldev/ring64/blob/main/sdk/tests/package/consumer.ts).
