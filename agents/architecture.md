# architecture

Ring64 translates guest x86 instructions into WebAssembly. The Rust core owns guest CPU semantics. The TypeScript SDK runs the core, installs translated code and exposes the CPU to JavaScript applications.

## SDK entry points

- `ring64` exports `createCpu` and `loadCpuCompiler` from `sdk/src/index.ts`. `sdk/src/cpu/` coordinates runs and owns the installed translation cache.
- `ring64/parallel` exports `createMachine` and `loadParallelCompiler` from `sdk/src/machine.ts`. A machine assigns each vCPU to a dedicated worker. Each worker has private CPU state and a private translation cache. The machine coordinates shared RAM access, code invalidation, memory reconfiguration and caller-supplied I/O.
- `sdk/src/wasm/` loads the packaged core and exchanges state and data through the Wasm ABI. `sdk/src/state/` describes the state exposed by the SDK. `sdk/src/embedding/` validates and prepares memory mappings, backings and I/O supplied by the caller.

`sdk/package.json` defines the two public entry points. `npm run build` builds the standalone and shared-memory cores, compiles the SDK and stages the npm package in `build/npm/ring64/`. The package contains JavaScript, TypeScript declarations, Wasm and worker modules. The build also stages the SDK in `build/host/` so benchmarks can load current and archived builds using the same layout.

`scripts/build/generate-parallel-host.mjs` derives the worker's CPU and core modules from the emitted standalone JavaScript. It checks that each replacement has a unique anchor. The two paths share ABI type declarations in `sdk/src/wasm/abi.ts`.

## Dependency direction

The SDK calls the Wasm ABI in `src/embedding/`. The ABI exchanges canonical state through descriptors from `src/embedding/layout/` and delegates guest operations to their Rust owners. The SDK reads those descriptors to interpret the exchanged data.

The translator discovers a unit from executable bytes, decoded instructions, architectural context and translation limits. Register values, operand data and per-run budgets stay runtime inputs. Instruction lowering lives under `src/dbt/wasm/`. Its `artifact.rs` assembles the module, binds imports and selects what to emit within the artifact size limit. The SDK compiles and installs the module and retires its entries when it replaces or evicts the artifact.

## Inside a component

| Source                                | Responsibility                                                                                      |
| ------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `src/x86/`                            | Instruction definitions and decoding, architectural state, paging, protection and numeric semantics |
| `src/memory/`                         | Physical backing and mapped access, span validation and the Rust shared-memory access primitives    |
| `src/dbt/discovery/`, `src/dbt/unit/` | Discover, admit and describe translation units                                                      |
| `src/dbt/ownership/`, `src/dbt/wasm/` | Track unit ownership and lower units into Wasm                                                      |
| `src/embedding/exports/`              | Adapt guest execution, events, memory, numeric and system operations to the Wasm ABI                |

Keep an instruction's architectural meaning with its Rust owner and its Wasm realization with the lowering that implements it. Keep transport layout with the ABI. A generated fast path preserves the access ordering, permissions, faults and effects of the shared memory contract.

## Memory and translation ownership

Guest paging and protection checks live in Rust. `Image` owns the mappings, permissions and byte dependencies that must agree when mappings change or guest writes invalidate translated code. The shared-memory implementation projects admitted accesses onto backing bytes. Its RAM module owns stripe promotion, lock lifetimes and write notifications. The SDK prepares the caller's physical memory and coordinates worker access to shared backings.

The Rust translator owns unit metadata and source dependencies. The SDK owns the compiled module and installed cache entries. In parallel mode, source watches in `sdk/src/parallel/memory/` track shared executable ranges. The machine coordinates invalidation and reclaims watches during its closed phase, when workers have reached a safe point.

## Tests and tooling

Rust unit tests live with their components. `sdk/tests/` covers SDK internals, the public API, the Wasm and worker integration, and installation of the actual npm archive. `tests/execution/` exercises guest x86 behavior through the SDK, with expected architectural results checked by the fixtures. `tests/tooling/` covers the development tools.

`scripts/build/` produces the build and package. `scripts/check/` runs browser and parallel mutation checks. `scripts/bench/` measures the workloads in `benchmarks/` using built artifacts. `scripts/verification/` holds the spec-specific probes and replay tools. Source proofs that compare against a pinned snapshot belong with the recorded candidate checkout, alongside the artifacts for historical measurements.
