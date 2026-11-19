import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import os from 'node:os';
import { chromium } from 'playwright';
import {
  benchmarkScript,
  encode,
  freeze,
  hash,
  manifest,
  root,
} from '../paging/paging-support.mjs';
import {
  alignmentPerformanceNames,
  alignmentPerformancePlan,
} from './alignment-checking-performance-kernels.mjs';

// Build, guest-process serialization and the quiet timing window belong to the caller.
const args = process.argv.slice(2),
  values = {};
const flags = new Set(['--browser', '--plan', '--validate-only', '--prepare-only']);
for (let i = 0; i < args.length; i++) {
  if (flags.has(args[i])) continue;
  if (
    ![
      '--current',
      '--snapshot',
      '--baseline',
      '--output',
      '--iterations',
      '--samples',
      '--workloads',
      '--seed',
    ].includes(args[i]) ||
    args[i + 1] === undefined ||
    args[i + 1].startsWith('--')
  )
    throw Error(`unknown/incomplete argument ${args[i]}`);
  values[args[i].slice(2)] = args[++i];
}
const browserMode = args.includes('--browser'),
  validation = args.includes('--validate-only');
const knobs = {
  iterations: validation
    ? Math.min(Number(values.iterations ?? 31), 31)
    : Number(values.iterations ?? 100001),
  samples: validation ? 1 : Number(values.samples ?? 11),
  seed: Number(values.seed ?? 1701),
  workloads: (values.workloads ?? alignmentPerformanceNames.join(',')).split(','),
  validateOnly: validation,
};
const plan = alignmentPerformancePlan(knobs);
const baselinePath = resolve(values.baseline ?? 'build/verification/alignment-baseline-b6da305');
function inheritedCommands(candidate, baseline, destination) {
  const common = (iterations = knobs.iterations) => [
    '--iterations',
    String(iterations),
    '--samples',
    String(Math.max(2, knobs.samples)),
  ];
  const browser = browserMode ? ['--browser'] : [];
  const pair = (iterations = knobs.iterations) => [
    '--baseline',
    baseline,
    '--current',
    candidate,
    ...common(iterations),
    ...browser,
  ];
  const ordinary =
    'scalar-add,checked-scalar-load,checked-byte-rmw,stack-push-pop,rep-forward-copy,x87-load-store-m32,x87-load-store-m64,x87-load-store-m80,x87-add-dependent-pc53,x87-budget-one-resume,mmx-add-dependent,mmx-checked-load-store,mmx-add-dependent-resume,numeric-fxsave,numeric-fx-roundtrip-live-xmm,numeric-legacy-roundtrip';
  const benchmark = benchmarkScript(candidate);
  return [
    [
      'ordinary',
      [benchmark, '--baseline', baseline, ...common(), ...browser, '--workloads', ordinary],
    ],
    [
      'matched64-resume-report',
      [
        benchmark,
        '--baseline',
        baseline,
        ...common(),
        ...browser,
        '--workloads',
        'far-call-ret-64-resume',
      ],
    ],
    [
      'matched64-resume-delivery',
      [
        benchmark,
        '--baseline',
        baseline,
        ...common(),
        ...browser,
        '--matched-delivery',
        '--workloads',
        'far-call-ret-64-resume',
      ],
    ],
    [
      'ia32e-memory',
      [
        resolve(root, 'scripts/verification/paging/paging-latency-audit.mjs'),
        ...pair(),
        '--workloads',
        'load,store,rmw,system',
      ],
    ],
    [
      'context-reuse',
      [
        resolve(root, 'scripts/verification/paging/paging-latency-audit.mjs'),
        ...pair(Math.min(knobs.iterations, 101)),
        '--context-reuse',
        '--workloads',
        'fast-syscall-sysret64,fast-budget-one-roundtrip,fast-debug-enabled-miss',
      ],
    ],
    [
      'gate',
      [
        resolve(root, 'scripts/verification/call-gates/call-gates-pair.mjs'),
        ...pair(Math.min(knobs.iterations, 4096)),
        '--workloads',
        'gate-w4-n1-linear,gate-w4-n31-linear,gate-w4-n1-pg0,gate-w4-n1-nonpae,gate-w4-n1-pae,gate-w4-n1-short-resumes,gate-ia64-ia32e-physical-long,gate-ia64-ia32e-fixed-long',
      ],
    ],
  ].map(([name, command]) => ({
    name,
    executable: process.execPath,
    requestedIterations: knobs.iterations,
    runnerIterations: Number(command[command.indexOf('--iterations') + 1]),
    completedWorkAuthority:
      'Each result definition and sample records actual retired/element/event counts; existing leaf-specific kernel caps may be lower than the runner argument',
    iterationPolicy:
      name === 'context-reuse'
        ? 'Retain inherited101-roundtrip context protocol cap'
        : name === 'gate'
          ? 'Retain inherited4096-cycle gate protocol cap'
          : 'Requested iterations; unchanged kernel-specific caps remain explicit in each result definition',
    arguments: [
      ...command,
      '--output',
      resolve(destination, `${name}-${browserMode ? 'chrome' : 'node'}.json`),
    ],
    reverse: 'Repeat with --reverse in a separate quiet window; retain every raw sample.',
  }));
}
if (args.includes('--plan')) {
  const result = encode({
    mode: 'static-plan-only-no-guest-or-timing',
    knobs,
    definitions: plan,
    inherited: inheritedCommands(
      values.snapshot ?? '<FROZEN_CANDIDATE>',
      '<PINNED_BASELINE_VIEW>',
      '<OUTPUT_DIRECTORY>',
    ),
  });
  if (values.output) {
    const output = resolve(values.output);
    await mkdir(dirname(output), { recursive: true });
    await writeFile(output, result);
    console.log(
      encode({ mode: 'static-plan-only-no-guest-or-timing', output, definitions: plan.length }),
    );
  } else console.log(result);
  process.exit(0);
}
if (!!values.current === !!values.snapshot)
  throw Error('require exactly one of --current BUILT_REPO or --snapshot FROZEN_BUILD');
const tag = `${Date.now()}-${process.pid}`;
const current = values.snapshot
  ? await manifest(resolve(values.snapshot))
  : await freeze(resolve(values.current), `alignment-performance-${tag}-current`);
const sourcePaths = [
  'scripts/verification/alignment-checking/alignment-checking-performance.mjs',
  'scripts/verification/alignment-checking/alignment-checking-performance-kernels.mjs',
  'scripts/verification/paging/paging-support.mjs',
];
const sourceHashes = async () =>
  Object.fromEntries(
    await Promise.all(
      sourcePaths.map(async (name) => [name, hash(await readFile(resolve(root, name)))]),
    ),
  );
const sourceSha256 = await sourceHashes();
if (args.includes('--prepare-only')) {
  const receipt = JSON.parse(await readFile(resolve(baselinePath, 'baseline.json'), 'utf8'));
  const old = await manifest(baselinePath);
  if (
    receipt.commit !== 'b6da3055d3ae5fda7a7a9225d6fbcde59990f55b' ||
    old.coreSha256 !== receipt.core
  )
    throw Error('baseline is not the frozen b6da305 Core');
  if (current.coreSha256 === old.coreSha256)
    throw Error('candidate is still the disabled-AM baseline Core; build the candidate first');
  const sourceReceipt = JSON.parse(await readFile(resolve(baselinePath, 'source.json'), 'utf8'));
  for (const [name, expected] of Object.entries(sourceReceipt)) {
    const path = name.startsWith('benchmarks/')
      ? resolve(current.path, name)
      : name === 'scripts/benchmark.mjs' || name === 'scripts/bench/benchmark.mjs'
        ? benchmarkScript(current.path)
        : undefined;
    if (path && hash(await readFile(path)) !== expected)
      throw Error(`inherited measurement source changed: ${name}`);
  }
  // A separate receipt view prevents generic runners from reading this repo's later HEAD.
  const view = await freeze(baselinePath, `alignment-performance-${tag}-baseline`);
  await writeFile(
    resolve(view.path, 'paging-source.json'),
    encode({
      source: { commit: receipt.commit, workingTree: [] },
      frozenBaseline: baselinePath,
      receipt,
      sourceReceiptSha256: hash(await readFile(resolve(baselinePath, 'source.json'))),
    }),
  );
  const baseline = await manifest(view.path);
  if (baseline.coreSha256 !== old.coreSha256 || baseline.hostSha256 !== old.hostSha256)
    throw Error('baseline view changed Core or its own host bundle');
  const directory = resolve(root, `build/verification/alignment-performance-${tag}`);
  await mkdir(directory, { recursive: true });
  const output = resolve(values.output ?? resolve(directory, 'prepared.json'));
  await mkdir(dirname(output), { recursive: true });
  await writeFile(
    output,
    encode({
      mode: 'prepared-only-no-engine-or-timing',
      current,
      baseline,
      sourceSha256,
      inherited: inheritedCommands(current.path, baseline.path, directory),
      active: {
        executable: process.execPath,
        arguments: [
          resolve(
            root,
            'scripts/verification/alignment-checking/alignment-checking-performance.mjs',
          ),
          '--snapshot',
          current.path,
          ...(browserMode ? ['--browser'] : []),
          '--iterations',
          String(knobs.iterations),
          '--samples',
          String(knobs.samples),
          '--output',
          resolve(directory, `active-${browserMode ? 'chrome' : 'node'}.json`),
        ],
      },
    }),
  );
  console.log(encode({ output, current: current.path, baseline: baseline.path }));
  process.exit(0);
}
const kernelBytes = await readFile(
  resolve(
    root,
    'scripts/verification/alignment-checking/alignment-checking-performance-kernels.mjs',
  ),
);
const frozenKernel = resolve(current.path, 'alignment-checking-performance-kernels.mjs');
await writeFile(frozenKernel, kernelBytes);
let result;
if (!browserMode) {
  const { createCpu } = await import(pathToFileURL(resolve(current.path, 'build/host/index.js')));
  const { measureAlignmentPerformance } = await import(pathToFileURL(frozenKernel));
  const compiler = await WebAssembly.compile(
    await readFile(resolve(current.path, 'build/ring64.wasm')),
  );
  result = await measureAlignmentPerformance({ createCpu, compiler }, knobs);
} else {
  const server = createServer(async (request, response) => {
    try {
      const pathname = new URL(request.url ?? '/', 'http://localhost').pathname;
      if (pathname === '/') {
        response.end('<!doctype html><title>Alignment-checking characterization</title>');
        return;
      }
      const file = resolve(current.path, '.' + pathname);
      if (!(
        file.startsWith(resolve(current.path, 'build/host') + sep) ||
        file === resolve(current.path, 'build/ring64.wasm') ||
        file === frozenKernel
      ))
        throw Error('outside allowed route');
      response.writeHead(200, {
        'content-type': pathname.endsWith('.wasm') ? 'application/wasm' : 'text/javascript',
        'cache-control': 'no-store',
      });
      response.end(await readFile(file));
    } catch {
      response.writeHead(404);
      response.end();
    }
  });
  // Owned ephemeral listener has no expected shared port; the caller serializes this command.
  await new Promise((done, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', done);
  });
  let browser;
  try {
    let executablePath = process.env.RING64_BROWSER;
    if (!executablePath)
      try {
        await access('/Applications/Google Chrome.app/Contents/MacOS/Google Chrome');
        executablePath = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
      } catch {
        /* Playwright default */
      }
    browser = await chromium.launch({
      headless: true,
      ...(executablePath ? { executablePath } : {}),
    });
    const page = await browser.newPage();
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    result = await page.evaluate(async (knobs) => {
      const { createCpu } = await import('/build/host/index.js');
      const { measureAlignmentPerformance } =
        await import('/alignment-checking-performance-kernels.mjs');
      const compiler = await WebAssembly.compile(
        await (await fetch('/build/ring64.wasm')).arrayBuffer(),
      );
      return {
        ...(await measureAlignmentPerformance({ createCpu, compiler }, knobs)),
        browserUserAgent: navigator.userAgent,
      };
    }, knobs);
    result.browserVersion = browser.version();
  } finally {
    await browser?.close();
    await new Promise((done) => server.close(done));
  }
}
const after = await manifest(current.path);
if (current.coreSha256 !== after.coreSha256 || current.hostSha256 !== after.hostSha256)
  throw Error('candidate Core/host changed during observation');
if (
  JSON.stringify(sourceSha256) !== JSON.stringify(await sourceHashes()) ||
  hash(await readFile(frozenKernel)) !== hash(kernelBytes)
)
  throw Error('measurement sources changed during observation');
const output = resolve(
  values.output ??
    `build/verification/alignment-${validation ? 'validation' : 'activation'}-${browserMode ? 'chrome' : 'node'}-${tag}.json`,
);
await mkdir(dirname(output), { recursive: true });
await writeFile(
  output,
  encode({
    at: new Date().toISOString(),
    mode: validation ? 'new-capability-validation-no-timing' : 'new-capability-characterization',
    runtime: browserMode ? 'chrome' : 'node',
    environment: {
      node: process.version,
      cpu: os.cpus()[0]?.model,
      platform: os.platform(),
      arch: os.arch(),
    },
    manifest: current,
    sourceSha256,
    knobs,
    postRunIntegrity: { core: true, ownHost: true, sources: true, frozenKernel: true },
    ...result,
  }),
);
console.log(
  encode({ output, definitions: result.definitions.length, samples: result.samples.length }),
);
