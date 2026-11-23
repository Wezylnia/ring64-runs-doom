# architecture

ring64 runs x86 code by translating it to wasm.

## rust core

- `src/x86/` decodes instructions and implements cpu behavior, including paging and faults. `src/x86/memory/paging/cache/mod.rs` owns the translation records. `cache/guarded.rs` and `cache/copy.rs` use them to authorize memory accesses.
- `src/memory/` owns backings and mappings. `Image` checks access permissions and tracks writes that invalidate translated code. `src/memory/parallel/ram.rs` handles shared ram locks and write notifications.
- `src/dbt/` discovers code to translate, tracks what each translation depends on and emits wasm. in `src/dbt/wasm/`, `artifact/placement.rs` selects candidate lowerings. `artifact/mod.rs` assembles the module and retries when it exceeds the byte budget. `execution/` emits control flow and `memory/` emits checked accesses and their fast paths.
- `src/embedding/` exposes the core to the sdk and defines the layout of the state they exchange.

rust modules with children keep their entry point in `mod.rs` alongside those children. `src/x86/mod.rs` re-exports the public x86 modules from their physical owners. opcode dispatch, numeric decoders and system decoders live under `src/x86/decode/`.

## typescript sdk

`ring64` runs a single cpu. `sdk/src/cpu/` compiles, caches and runs the wasm emitted by the core. `sdk/src/wasm/` loads the core and reads its state layout. `wasm/artifact-imports.ts` binds core helpers for generated code. `cpu/run-result.ts` maps stop reasons to the diagnostics exposed by the sdk.

`ring64/parallel` runs one worker per vcpu. workers share guest ram and keep their own cpu state and translation caches. `sdk/src/parallel/cpu/engine.ts` runs the cpu and `sdk/src/parallel/cpu/core.ts` connects it to shared ram. `sdk/src/machine.ts` coordinates mapping changes, i/o and code invalidation. it waits for workers to reach a safe point before reclaiming watches on bytes used by translations. closing the machine releases its shared ram and i/o references even if a caller keeps a cpu handle.

## build and tests

`npm run build` stages the npm package in `build/npm/ring64/` and the runtime for benchmarks in `build/host/`.

`npm run site:build` builds the website with the staged sdk. `npm run site:dev` serves the existing output in `build/website/`.

rust unit tests live with the code they cover. `sdk/tests/` covers the api, wasm integration and npm package. `tests/execution/` groups guest scenarios by instruction, control-flow, numeric, memory, event, processor, translation, i/o and parallel behavior. script tests live in `tests/tooling/`. verification drivers in `scripts/verification/` share the frozen-build checks in `artifacts.mjs`.
