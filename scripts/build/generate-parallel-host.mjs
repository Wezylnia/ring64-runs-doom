import { mkdir, readFile, writeFile } from 'node:fs/promises';

const directory = new URL('../../build/sdk/src/', import.meta.url);
const output = new URL('parallel/', directory);

function change(source, before, after) {
  const start = source.indexOf(before);
  if (start < 0 || source.indexOf(before, start + before.length) >= 0)
    throw new Error(`parallel host derivative lost its unique anchor: ${before}`);
  return source.slice(0, start) + after + source.slice(start + before.length);
}

function coreBody(input) {
  let source = change(
    input,
    'constructor(exports, isPaged = false, physicalControls = false, io) {',
    'constructor(exports, isPaged = false, physicalControls = false, io, binding) {\n        this.parallelBinding = binding;',
  );
  source = change(
    source,
    'static async create(compiler, memory, context, ioBinding) {',
    'static async create(compiler, memory, context, ioBinding, binding) {',
  );
  source = change(
    source,
    'const instance = await WebAssembly.instantiate(module, { io: io.imports });',
    `const instance = await WebAssembly.instantiate(module, {
                io: io.imports,
                env: { memory: binding.privateMemory },
                ring64_ram: { ...binding.ramImports, ...binding.imports },
            });`,
  );
  source = change(
    source,
    'const exports = instance.exports;',
    `const exports = instance.exports;
            if (typeof exports.core_parallel_abi !== 'function' || ![1, 2, 3].includes(exports.core_parallel_abi()) ||
                exports.memory !== binding.privateMemory)
                throw new Error('parallel core ABI mismatch');`,
  );
  source = change(
    source,
    `                backings: memory.backings.map(({ id, bytes }) => ({
                    id: backingKey(id),
                    length: bytes.length,
                })),
                mappings: mappingInput(memory.mappings),
            }, memory.backings.map(({ bytes }) => bytes));`,
    `                shared: binding.layout,
                backings: binding.backings.map(({ id, length, offset, incarnation }) => ({
                    id: backingKey(id), length, offset, incarnation: incarnation.toString(),
                })),
                mappings: mappingInput(memory.mappings),
            });`,
  );
  source = change(
    source,
    '    reason(value, generated) {',
    "    reason(value, generated) {\n        if (value === 29) return 'parallel-boundary';",
  );
  source = change(
    source,
    'new Core(exports, isPaged, physicalControls, io)',
    'new Core(exports, isPaged, physicalControls, io, binding)',
  );
  source = change(
    source,
    '        const instance = new WebAssembly.Instance(module, { env });',
    `        Object.assign(env, {
            shared_ram: this.parallelBinding.shared,
            parallel_signal: this.parallelBinding.signal,
            parallel_begin: this.parallelBinding.imports.enter_exclusive,
            parallel_end: this.parallelBinding.imports.leave_exclusive,
            parallel_resolve: this.#exports.core_parallel_resolve,
            parallel_vector_dword: this.#exports.core_parallel_vector_dword,
            parallel_vector_dword_span: this.#exports.core_parallel_vector_dword_span,
            parallel_guard: this.#exports.core_parallel_guard,
            parallel_unguard: this.#exports.core_parallel_unguard,
        });
        const instance = new WebAssembly.Instance(module, { env });`,
  );
  source = change(
    source,
    '    release(owner) {\n        this.#exports.core_release(owner);',
    '    release(owner) {\n        this.parallelBinding.unwatch(owner);\n        this.#exports.core_release(owner);',
  );
  return change(
    source,
    '    memoryResult(status) {',
    `    parallelGetIngress() {
        return readInterrupts(this.stateView(), this.#layout.interrupts, this.#layout.interrupts.offset);
    }
    parallelSetIngress(irq, nmiPending) {
        const view = this.stateView();
        const layout = this.#layout.interrupts;
        const previous = this.parallelGetIngress();
        writeInterrupts(new DataView(view.buffer, view.byteOffset + layout.offset, layout.bytes),
            layout, { ...previous, irq, nmiPending });
    }
    parallelProof(owner) {
        if (this.#exports.core_parallel_proof(owner) !== 0)
            throw new Error('parallel core refused source proof');
        const bytes = this.output();
        const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.length);
        if (view.byteLength % 8 !== 0) throw new Error('malformed parallel source proof');
        const ranges = [];
        for (let at = 0; at < view.byteLength; at += 8)
            ranges.push({ offset: view.getUint32(at, true), length: view.getUint32(at + 4, true) });
        return ranges;
    }
    parallelInvalidate(offset, length) {
        this.#exports.core_parallel_invalidate(offset, length);
    }
    parallelAddBacking({ id, length, offset, incarnation }) {
        writeInput(this.#exports, backingKey(id));
        this.memoryResult(this.#exports.core_parallel_add_backing(length, offset, incarnation));
    }
    parallelHostMemoryChanged() {
        this.#exports.core_parallel_admin_changed();
    }
    parallelFinishSlice() {
        if (this.isPaged) this.#exports.core_parallel_finish_slice();
        this.parallelBinding.finishSlice();
    }
    memoryResult(status) {`,
  );
}

function cpuBody(input) {
  let source = change(input, " from '../wasm/core.js';", " from './core.generated.js';");
  source = change(source, " from './translations.js';", " from '../cpu/translations.js';");
  source = change(
    source,
    'export async function createCpu(options) {',
    'export async function createCpu(options, binding) {',
  );
  source = change(
    source,
    `const memory = captureInitialMemory(options);
    if (memory.memoryModel !== 'linear') {
        memory.paging = capturePagingState(options.paging);
        memory.fastSystem =
            'fastSystem' in options ? captureFastSystemState(options.fastSystem) : zeroFastSystemState();
    }`,
    'const memory = options.memory;',
  );
  source = change(
    source,
    'await Core.create(options.compiler, memory, context, io)',
    'await Core.create(options.compiler, memory, context, io, binding)',
  );
  source = change(
    source,
    'new Map(memory.backings.map(({ id, bytes }) => [id, bytes.length]))',
    'new Map(binding.backings.map(({ id, length }) => [id, length]))',
  );
  source = change(
    source,
    '    pruneTranslations() {',
    `    get parallelOwner() { return this.#engine; }
    parallelAdmin(method, args) {
        switch (method) {
            case 'addBacking':
                this.#engine.parallelAddBacking(args[0]);
                this.#backings.set(args[0].id, args[0].length);
                break;
            case 'removeBacking':
                this.#engine.removeBacking(args[0]);
                this.#backings.delete(args[0]);
                break;
            case 'setMappings':
                this.#engine.setMappings(args[0]);
                this.pruneTranslations();
                break;
            case 'readMemory':
            case 'readPhysicalMemory':
                return this.#engine.readMemory(...args);
            case 'writeMemory':
            case 'writePhysicalMemory':
                this.#engine.writeOwnedMemory(...args);
                this.pruneTranslations();
                break;
            case 'hostMemoryChanged':
                this.#engine.parallelHostMemoryChanged();
                this.pruneTranslations();
                break;
            default:
                throw new Error('unknown parallel administration command');
        }
    }
    pruneTranslations() {`,
  );
  source = change(
    source,
    '            execution: for (;;) {',
    `            execution: for (;;) {
                if (await this.#engine.parallelBinding.checkpoint(this.#engine)) {
                    if (!servicingEvent && remaining === 0n) return finish('budget');
                    if (!servicingEvent && stop !== undefined && this.#engine.current().rip === stop)
                        return finish('stop');
                    return finish('paused');
                }
                queuedInterrupts = this.#engine.hasQueuedInterrupts();`,
  );
  source = change(
    source,
    '                    const output = this.#engine.compile(pc, this.#limits);',
    `                    const binding = this.#engine.parallelBinding;
                    const token = binding.imports.enter_exclusive();
                    let output;
                    try {
                        output = this.#engine.compile(pc, this.#limits);
                        if (output) binding.watch(this.#engine, output.metadata.owner);
                    } finally {
                        binding.imports.leave_exclusive(token);
                    }`,
  );
  source = change(
    source,
    '                        const module = await WebAssembly.compile(new Uint8Array(output.bytes));',
    `                        const module = await binding.compile(new Uint8Array(output.bytes));
                        if (module === undefined || await binding.checkpoint(this.#engine)) return finish('paused');
                        queuedInterrupts = this.#engine.hasQueuedInterrupts();`,
  );
  source = change(
    source,
    'const resumed = this.#engine.resumeTask();',
    'const resumed = this.#engine.resumeTask();\n                this.#engine.parallelFinishSlice();',
  );
  source = change(
    source,
    'const reason = this.#engine.deliverException();',
    'const reason = this.#engine.deliverException();\n                    this.#engine.parallelFinishSlice();',
  );
  source = change(
    source,
    'const reason = this.#engine.execute(entry.artifact.run, entry.index, remaining, stop, remainingRepeatBudget);',
    `const binding = this.#engine.parallelBinding;
                const sliceBudget = binding.sliceLimit(remaining);
                const sliceRepeats = binding.sliceLimit(remainingRepeatBudget);
                let reason = this.#engine.execute(entry.artifact.run, entry.index, sliceBudget, stop, sliceRepeats);
                this.#engine.parallelFinishSlice();`,
  );
  source = change(
    source,
    `                remaining = current.remaining;
                remainingRepeatBudget = current.repeatRemaining;`,
    `                remaining = current.remaining + (before - sliceBudget);
                remainingRepeatBudget = current.repeatRemaining + (repeatBefore - sliceRepeats);
                if ((reason === 'budget' && remaining !== 0n) ||
                    (reason === 'repeat-budget' && remainingRepeatBudget !== 0n))
                    reason = 'parallel-boundary';`,
  );
  const restart = `                if (reason === 'paging-restart') {
                    resources.pagingRestarts = (resources.pagingRestarts ?? 0) + 1;`;
  return change(
    source,
    restart,
    `                if (reason === 'parallel-boundary') continue execution;\n${restart}`,
  );
}

const [core, cpu] = await Promise.all(
  ['wasm/core.js', 'cpu/cpu.js'].map((file) => readFile(new URL(file, directory), 'utf8')),
);
const header = '// Generated from the standalone host during the build.\n';
const files = [
  ['core.generated.js', header + coreBody(core)],
  ['engine.generated.js', header + cpuBody(cpu)],
];
await mkdir(output, { recursive: true });
await Promise.all(files.map(([name, contents]) => writeFile(new URL(name, output), contents)));
