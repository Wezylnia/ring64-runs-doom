import { mkdir, readFile, writeFile } from 'node:fs/promises';

const directory = new URL('../../build/sdk/src/', import.meta.url);
const output = new URL('parallel/', directory);

function change(source, before, after) {
  const start = source.indexOf(before);
  if (start < 0 || source.indexOf(before, start + before.length) >= 0)
    throw new Error(`parallel host derivative lost its unique anchor: ${before}`);
  return source.slice(0, start) + after + source.slice(start + before.length);
}

function cpuBody(input) {
  let source = change(
    input,
    "import { Core, } from '../wasm/core.js';",
    "import { ParallelCore as Core } from './core.js';",
  );
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
    'await Core.createWorker(options.compiler, memory, context, io, binding)',
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

const cpu = await readFile(new URL('cpu/cpu.js', directory), 'utf8');
const header = '// Generated from the standalone host during the build.\n';
await mkdir(output, { recursive: true });
await writeFile(new URL('engine.generated.js', output), header + cpuBody(cpu));
