import { createHash } from 'node:crypto';
import { existsSync } from 'node:fs';
import { cp, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { cpus, tmpdir } from 'node:os';
import { resolve, sep } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { Worker } from 'node:worker_threads';
import { chromium } from 'playwright';
import { measureLatency } from '../../benchmarks/parallel-latency.mjs';
import { emittedModules } from './emitted-host.mjs';

const [runtime, candidate, output, count = '128', order] = process.argv.slice(2);
const samples = Number(count);
if (
  !['node', 'chromium'].includes(runtime) ||
  !candidate ||
  !output ||
  !Number.isInteger(samples) ||
  samples < 1 ||
  samples > 256 ||
  (order !== undefined && order !== '--reverse')
) {
  throw Error('usage: parallel-latency.mjs node|chromium BUILD_DIR OUTPUT [SAMPLES] [--reverse]');
}

const benchmark = resolve(fileURLToPath(new URL('../../benchmarks/', import.meta.url)));
const build = resolve(candidate);
const traceFile = resolve(benchmark, 'parallel-latency-trace.mjs');
const wasm = await readFile(resolve(build, 'ring64-parallel.wasm'));
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const scratch = await mkdtemp(resolve(tmpdir(), 'ring64-parallel-latency-'));

async function graphDigest(root) {
  const files = await emittedModules(root);
  return sha256(
    JSON.stringify(
      await Promise.all(
        files.map(async (name) => [name, sha256(await readFile(resolve(root, name)))]),
      ),
    ),
  );
}

async function patch(file, edits) {
  const destination = resolve(scratch, 'host', file);
  let source = await readFile(destination, 'utf8');
  for (const [anchor, replacement] of edits) {
    const matches =
      typeof anchor === 'string'
        ? source.split(anchor).length - 1
        : [...source.matchAll(anchor)].length;
    if (matches !== 1) throw Error(`unexpected trace anchor in ${file}: ${anchor}`);
    source = source.replace(anchor, replacement);
  }
  await writeFile(destination, source);
}

async function instrument() {
  await cp(resolve(build, 'host'), resolve(scratch, 'host'), { recursive: true });
  await writeFile(resolve(scratch, 'package.json'), '{"type":"module"}\n');
  await cp(traceFile, resolve(scratch, 'host/parallel/latency-trace.js'));
  await patch('machine.js', [
    [
      'class ParallelMachine {',
      "import { LatencyTrace, latencyBuffer, latencyEvents as traceEvents } from './parallel/latency-trace.js';\nclass ParallelMachine {",
    ],
    [
      '    constructor(input, memory, storeLane) {',
      `    constructor(input, memory, storeLane) {
        this.latencyTraceMemory = latencyBuffer(input.cpus.length + 1);
        this.latencyTrace = new LatencyTrace(this.latencyTraceMemory, 0);`,
    ],
    [
      "                kind: 'start',",
      "                kind: 'start',\n                latencyTraceMemory: this.latencyTraceMemory,",
    ],
    [
      /^            const phase = (this\.#[\w.]+\.transition\(true\));$/gm,
      `            const closing = this.latencyTrace.now();
            const phase = $1;
            this.latencyTrace.write(traceEvents.close, closing, 0, phase, 0);`,
    ],
    [
      '            await closed.promise;',
      '            await closed.promise;\n            this.latencyTrace.mark(traceEvents.drained, phase);',
    ],
    [
      /^        (this\.#[\w.]+\.transition\(false\));$/gm,
      '        const reopened = $1;\n        this.latencyTrace.mark(traceEvents.open, reopened - 1n);',
    ],
    [
      /^        this\.#[\w.]+\.signal\(peer\.id, SIGNAL\.pause\);$/gm,
      '        this.latencyTrace.mark(traceEvents.pauseRequest, command.sequence, peer.id);\n$&',
    ],
    [
      '        this.#inProvider = true;',
      '        this.#inProvider = true;\n        const started = this.latencyTrace.now();',
    ],
    [
      '            this.#inProvider = false;',
      '            this.latencyTrace.span(traceEvents.provider, started, message.ticket, peer.id);\n            this.#inProvider = false;',
    ],
  ]);
  await patch('parallel/worker.js', [
    [
      'class WorkerRuntime {',
      "import { LatencyTrace, latencyEvents as traceEvents } from './latency-trace.js';\nclass WorkerRuntime {",
    ],
    [
      '        this.#control = new SharedControl(data.shared);',
      '        this.#control = new SharedControl(data.shared);\n        this.latencyTrace = new LatencyTrace(data.latencyTraceMemory, data.id + 1);',
    ],
    [
      '        this.#control.store(this.#id, SLOT.acknowledgement, phase);',
      '        this.latencyTrace.mark(traceEvents.acknowledgement, phase);\n        this.#control.store(this.#id, SLOT.acknowledgement, phase);',
    ],
    [
      '    sliceLimit(budget) {',
      `    beginTrace() { return this.latencyTrace.now(); }
    endNativeTrace(started, budget) {
        this.latencyTrace.span(traceEvents.native, started, this.#command, budget);
    }
    sliceLimit(budget) {`,
    ],
    [
      '            const job = WebAssembly.compile(bytes);',
      '            const started = this.latencyTrace.now();\n            const job = WebAssembly.compile(bytes);',
    ],
    [
      '            return await Promise.race([job, paused]);',
      `            const selected = await Promise.race([job, paused]);
            this.latencyTrace.span(traceEvents.compilation, started, this.#command, selected === undefined ? 0 : bytes.length);
            return selected;`,
    ],
    [
      '            this.#requested = nextIdentity(this.#requested);',
      '            const started = this.latencyTrace.now();\n            this.#requested = nextIdentity(this.#requested);',
    ],
    [
      '            this.#exclusive = this.#requested;',
      '            this.#exclusive = this.#requested;\n            this.latencyTrace.span(traceEvents.exclusive, started, this.#command, this.#requested);',
    ],
    [
      '        this.#ioTicket = nextIdentity(this.#ioTicket);',
      '        const started = this.latencyTrace.now();\n        this.#ioTicket = nextIdentity(this.#ioTicket);',
    ],
    [
      '            if (status === 1)\n                return this.#control.loadWord(this.#id, SLOT.ioResult) >>> 0;',
      `            if (status === 1) {
                this.latencyTrace.span(traceEvents.port, started, this.#command, this.#ioTicket);
                return this.#control.loadWord(this.#id, SLOT.ioResult) >>> 0;
            }`,
    ],
    [
      '            this.#running = false;\n            this.#command = 0n;\n            this.#admin = 0n;\n            this.suspend();',
      `            const paused = this.#pauseRequested() ? this.#command : 0n;
            this.#running = false;
            this.#command = 0n;
            this.#admin = 0n;
            this.suspend();
            if (paused !== 0n) this.latencyTrace.mark(traceEvents.pauseSafe, paused);`,
    ],
  ]);
  const nativeExecution = existsSync(resolve(build, 'host/parallel/core.js'))
    ? 'wasm/core.js'
    : 'parallel/core.generated.js';
  await patch(nativeExecution, [
    [
      '            return this.reason(run(this.#statePointer, budget, stop ?? 0n, stop === undefined ? 0 : 1, entryIndex), true);',
      `            const started = this.parallelBinding.beginTrace();
            const outcome = run(this.#statePointer, budget, stop ?? 0n, stop === undefined ? 0 : 1, entryIndex);
            this.parallelBinding.endNativeTrace(started, budget);
            return this.reason(outcome, true);`,
    ],
  ]);
}

const reverse = order === '--reverse';

async function browserMeasure() {
  const server = createServer(async (request, response) => {
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    response.setHeader('Cache-Control', 'no-store');
    const path = new URL(request.url ?? '/', 'http://localhost').pathname;
    if (path === '/') {
      response.setHeader('Content-Type', 'text/html');
      response.end('<!doctype html><title>Ring64 parallel latency benchmark</title>');
      return;
    }
    const [, prefix, ...rest] = path.split('/');
    const root = { clean: build, traced: scratch, benchmark }[prefix];
    const file = root && resolve(root, ...rest);
    if (!file || !file.startsWith(root + sep)) {
      response.writeHead(404).end();
      return;
    }
    try {
      const bytes = await readFile(file);
      response.setHeader(
        'Content-Type',
        file.endsWith('.wasm') ? 'application/wasm' : 'text/javascript',
      );
      response.end(bytes);
    } catch {
      response.writeHead(404).end();
    }
  });
  await new Promise((done, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', done);
  });
  let browser;
  try {
    browser = await chromium.launch({
      headless: true,
      ...(process.env.RING64_BROWSER ? { executablePath: process.env.RING64_BROWSER } : {}),
    });
    const page = await browser.newPage();
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    const cases = await page.evaluate(
      async ({ samples, reverse }) => {
        if (!globalThis.crossOriginIsolated) throw Error('worker isolation required');
        const benchmark = '/benchmark/parallel-latency.mjs';
        const { measureLatency } = await import(benchmark);
        const compiler = await WebAssembly.compileStreaming(fetch('/clean/ring64-parallel.wasm'));
        const base = (path) => new URL(path, globalThis.location.href).href;
        return measureLatency(
          compiler,
          base('/clean/'),
          base('/traced/'),
          samples,
          reverse,
          globalThis.Worker,
        );
      },
      { samples, reverse },
    );
    return { engine: browser.version(), cases };
  } finally {
    try {
      await browser?.close();
    } finally {
      await new Promise((done) => server.close(done));
    }
  }
}

try {
  await instrument();
  const cleanHostSha256 = await graphDigest(resolve(build, 'host'));
  const tracedHostSha256 = await graphDigest(resolve(scratch, 'host'));
  const run =
    runtime === 'chromium'
      ? await browserMeasure()
      : {
          engine: process.version,
          cases: await measureLatency(
            await WebAssembly.compile(wasm),
            pathToFileURL(build + sep).href,
            pathToFileURL(scratch + sep).href,
            samples,
            reverse,
            Worker,
          ),
        };
  if (cleanHostSha256 !== (await graphDigest(resolve(build, 'host'))))
    throw Error('clean host graph changed');
  if (!wasm.equals(await readFile(resolve(build, 'ring64-parallel.wasm'))))
    throw Error('Core bytes changed');
  await writeFile(
    output,
    JSON.stringify(
      {
        runtime,
        ...run,
        samples,
        reverse,
        coreSha256: sha256(wasm),
        cleanHostSha256,
        tracedHostSha256,
        cpu: cpus()[0]?.model,
        logicalProcessors: cpus().length,
      },
      null,
      2,
    ) + '\n',
  );
} finally {
  await rm(scratch, { recursive: true, force: true });
}
