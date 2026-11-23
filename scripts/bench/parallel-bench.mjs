import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { extname, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import { chromium } from 'playwright';
import { measureParallel, parallelWorkloads } from '../../benchmarks/parallel/workloads.mjs';
import { hostIdentity } from './emitted-host.mjs';
import { chromiumExecutable } from '../support/browser.mjs';

const [runtime, candidate, baseline, output, workers, max, replicates, ...flags] =
  process.argv.slice(2);
if (
  !['node', 'chromium'].includes(runtime) ||
  !candidate ||
  !baseline ||
  !output ||
  flags.some((flag) => flag !== '--reverse' && !flag.startsWith('--workloads='))
) {
  throw Error(
    'usage: parallel-bench.mjs node|chromium CANDIDATE BASELINE OUTPUT [WORKERS] [ITERATIONS] [SAMPLES] [--reverse] [--workloads=alu,mixed,writes,contended]',
  );
}
const count = Number(workers ?? 2),
  iterations = Number(max ?? 1_000_000),
  samples = Number(replicates ?? 9);
if (
  ![2, 4].includes(count) ||
  !Number.isSafeInteger(iterations) ||
  iterations < 1 ||
  iterations > 100_000_000 ||
  !Number.isSafeInteger(samples) ||
  samples < 1 ||
  samples > 101
) {
  throw Error('invalid workers, iterations or samples');
}
const current = resolve(candidate, 'build'),
  previous = resolve(baseline, 'build'),
  wasm = await readFile(resolve(current, 'ring64-parallel.wasm')),
  privateWasm = await readFile(resolve(previous, 'ring64.wasm'));
const selected = flags
  .find((flag) => flag.startsWith('--workloads='))
  ?.slice('--workloads='.length);
const workloads = selected === undefined ? parallelWorkloads : selected.split(',');
if (
  !workloads.length ||
  new Set(workloads).size !== workloads.length ||
  workloads.some((name) => !parallelWorkloads.includes(name))
) {
  throw Error('invalid or repeated shared workload');
}
const options = {
  count,
  iterations,
  samples,
  workloads,
  reverse: flags.includes('--reverse'),
};
const hash = (bytes) => createHash('sha256').update(bytes).digest('hex');
const hostHashes = async () =>
  Promise.all(
    [current, previous].map(async (root) => {
      const { hostSha256 } = await hostIdentity(resolve(root, 'host'));
      return hostSha256;
    }),
  );
const [parallelHostSha256, privateHostSha256] = await hostHashes();
let engine, cases;
if (runtime === 'node') {
  const { createMachine } = await import(pathToFileURL(resolve(current, 'host/machine.js')));
  const { createCpu } = await import(pathToFileURL(resolve(previous, 'host/index.js')));
  cases = await measureParallel({
    ...options,
    createCpu,
    createMachine,
    parallelCore: await WebAssembly.compile(wasm),
    soloCore: await WebAssembly.compile(privateWasm),
  });
  engine = process.version;
} else {
  const server = createServer(async (request, response) => {
    const pathname = new URL(request.url ?? '/', 'http://localhost').pathname;
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    response.setHeader('Cache-Control', 'no-store');
    if (pathname === '/') {
      response.setHeader('Content-Type', 'text/html; charset=utf-8');
      response.end('<!doctype html><title>Parallel Ring64 performance</title>');
      return;
    }
    const [, prefix, ...suffix] = pathname.split('/');
    const root = prefix === 'candidate' ? current : prefix === 'baseline' ? previous : null;
    const exactHarness = pathname === '/parallel.mjs';
    const path = exactHarness
      ? resolve('benchmarks/parallel/workloads.mjs')
      : root
        ? resolve(root, ...suffix)
        : '';
    if (!(exactHarness || (root && path.startsWith(root + sep)))) {
      response.writeHead(404).end();
      return;
    }
    try {
      const bytes = await readFile(path);
      response.setHeader(
        'Content-Type',
        extname(path) === '.wasm' ? 'application/wasm' : 'text/javascript',
      );
      response.end(bytes);
    } catch {
      response.writeHead(404).end();
    }
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  let browser;
  try {
    const executablePath = await chromiumExecutable();
    browser = await chromium.launch({
      headless: true,
      ...(executablePath ? { executablePath } : {}),
    });
    const page = await browser.newPage();
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    cases = await page.evaluate(async (options) => {
      if (!globalThis.crossOriginIsolated) throw Error('worker isolation required');
      const { createMachine } = await import('/candidate/host/machine.js');
      const { createCpu } = await import('/baseline/host/index.js');
      const { measureParallel } = await import('/parallel.mjs');
      const module = async (path) => WebAssembly.compileStreaming(await fetch(path));
      return measureParallel({
        ...options,
        createCpu,
        createMachine,
        parallelCore: await module('/candidate/ring64-parallel.wasm'),
        soloCore: await module('/baseline/ring64.wasm'),
      });
    }, options);
    engine = browser.version();
  } finally {
    await browser?.close();
    await new Promise((resolve) => server.close(resolve));
  }
}
const [parallelAfter, privateAfter] = await hostHashes();
if (
  parallelAfter !== parallelHostSha256 ||
  privateAfter !== privateHostSha256 ||
  !wasm.equals(await readFile(resolve(current, 'ring64-parallel.wasm'))) ||
  !privateWasm.equals(await readFile(resolve(previous, 'ring64.wasm')))
)
  throw Error('built artifacts changed during parallel observation');

const receipt = {
  runtime,
  engine,
  parallelCore: hash(wasm),
  privateCore: hash(privateWasm),
  parallelHostSha256,
  privateHostSha256,
  ...options,
  cases,
};
await writeFile(output, JSON.stringify(receipt, null, 2) + '\n');
console.table(
  Object.entries(cases).map(([name, v]) => ({
    name,
    sharedSpeedup: v.sharedSpeedup,
    privateBreakEven: v.privateBreakEven,
    sharedSerialMs: v.summaries.sharedSerial,
    sharedParallelMs: v.summaries.sharedParallel,
    privateSerialMs: v.summaries.privateSerial,
  })),
);
