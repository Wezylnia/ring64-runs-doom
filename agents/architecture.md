# architecture

ring64 runs x86 code by translating it to wasm.

## rust core

- `src/x86/` decodes instructions and implements cpu behavior, including paging and faults.
- `src/memory/` owns backings and mappings. `Image` checks access permissions and tracks writes that invalidate translated code. `src/memory/parallel/ram.rs` handles shared ram locks and write notifications.
- `src/dbt/` discovers code to translate, tracks what each translation depends on and emits wasm.
- `src/embedding/` exposes the core to the sdk and defines the layout of the state they exchange.

## typescript sdk

`ring64` runs a single cpu. `sdk/src/cpu/` compiles, caches and runs the wasm emitted by the core. `sdk/src/wasm/` loads the core and reads its state layout.

`ring64/parallel` runs one worker per vcpu. workers share guest ram and keep their own cpu state and translation caches. `sdk/src/machine.ts` coordinates mapping changes, i/o and code invalidation. it waits for workers to reach a safe point before reclaiming watches on bytes used by translations.

## build and tests

`npm run build` stages the npm package in `build/npm/ring64/` and the runtime for benchmarks in `build/host/`. `scripts/build/generate-parallel-host.mjs` derives the worker modules by matching and replacing specific snippets in the emitted standalone js.

rust unit tests live with the code they cover. `sdk/tests/` covers the api, wasm integration and npm package. `tests/execution/` checks guest behavior through the sdk. script tests live in `tests/tooling/`.
