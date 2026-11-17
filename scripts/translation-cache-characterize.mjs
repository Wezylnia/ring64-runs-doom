import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import os from 'node:os';
import { freeze, manifest, hash, encode, root } from './paging-support.mjs';
import {
  measureTranslationCaches,
  translationCacheNames,
  translationCachePlan,
} from './translation-cache-kernels.mjs';

// The caller owns artifact builds, process inventory and the serialized quiet window.
const values = {},
  flags = new Set(['--browser', '--validate-only', '--plan']);
const args = process.argv.slice(2);
for (let i = 0; i < args.length; i++) {
  if (flags.has(args[i])) continue;
  if (
    ![
      '--current',
      '--snapshot',
      '--iterations',
      '--samples',
      '--seed',
      '--workloads',
      '--output',
    ].includes(args[i]) ||
    args[i + 1] === undefined ||
    args[i + 1].startsWith('--')
  )
    throw Error(`unknown/incomplete argument ${args[i]}`);
  values[args[i].slice(2)] = args[++i];
}
const browserMode = args.includes('--browser'),
  validateOnly = args.includes('--validate-only');
const knobs = {
  iterations: validateOnly
    ? Math.min(Number(values.iterations ?? 31), 31)
    : Number(values.iterations ?? 10001),
  samples: validateOnly ? 1 : Number(values.samples ?? 11),
  seed: Number(values.seed ?? 1701),
  workloads: (values.workloads ?? translationCacheNames.join(',')).split(','),
  validateOnly,
};
const definitions = translationCachePlan(knobs);
if (args.includes('--plan')) {
  console.log(
    encode({
      kind: 'static-candidate-characterization-plan',
      claim: 'No CPU, build, guest or timing executed.',
      knobs,
      definitions,
    }),
  );
  process.exit(0);
}
if (!!values.current === !!values.snapshot)
  throw Error('require exactly one --current BUILT_REPO or --snapshot FROZEN_BUILD');
const candidate = values.snapshot
  ? await manifest(resolve(values.snapshot))
  : await freeze(
      resolve(values.current),
      `translation-cache-characterization-${Date.now()}-${process.pid}-candidate`,
    );
const sourcePaths = [
  'scripts/translation-cache-characterize.mjs',
  'scripts/translation-cache-kernels.mjs',
  'scripts/paging-support.mjs',
];
const sources = async () =>
  Object.fromEntries(
    await Promise.all(
      sourcePaths.map(async (path) => [path, hash(await readFile(resolve(root, path)))]),
    ),
  );
const sourceSha256 = await sources();

async function nodeRun() {
  const { createCpu } = await import(pathToFileURL(resolve(candidate.path, 'build/host/index.js')));
  const compiler = await WebAssembly.compile(
    await readFile(resolve(candidate.path, 'build/ring64.wasm')),
  );
  return {
    engine: process.version,
    ...(await measureTranslationCaches({ createCpu, compiler }, knobs)),
  };
}

async function browserRun() {
  const { chromium } = await import('playwright');
  const server = createServer(async (request, response) => {
    try {
      const pathname = new URL(request.url, 'http://localhost').pathname;
      if (pathname === '/') {
        response.writeHead(200, { 'content-type': 'text/html' });
        response.end(
          '<!doctype html><meta charset="utf-8"><title>Retained translation characterization</title>',
        );
        return;
      }
      let path;
      if (pathname === '/kernel.mjs') path = resolve(root, 'scripts/translation-cache-kernels.mjs');
      else {
        if (!pathname.startsWith('/candidate/')) throw Error('unknown route');
        const base = resolve(candidate.path, 'build');
        path = resolve(base, pathname.slice('/candidate/'.length));
        if (!path.startsWith(base + sep)) throw Error('outside frozen build');
      }
      const bytes = await readFile(path);
      response.writeHead(200, {
        'content-type': path.endsWith('.wasm') ? 'application/wasm' : 'text/javascript',
        'cache-control': 'no-store',
      });
      response.end(bytes);
    } catch {
      response.writeHead(404);
      response.end();
    }
  });
  await new Promise((done, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', done);
  });
  let browser;
  try {
    let executablePath = process.env.RING64_BROWSER;
    if (!executablePath && process.platform === 'darwin') {
      const chrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
      try {
        await access(chrome);
        executablePath = chrome;
      } catch {
        /* use bundled Chromium */
      }
    }
    browser = await chromium.launch({
      headless: true,
      ...(executablePath ? { executablePath } : {}),
    });
    const page = await browser.newPage();
    await page.goto(`http://127.0.0.1:${server.address().port}`);
    const result = await page.evaluate(async (options) => {
      const { measureTranslationCaches } = await import('/kernel.mjs');
      const { createCpu } = await import('/candidate/host/index.js');
      const compiler = await WebAssembly.compile(
        await (await fetch('/candidate/ring64.wasm')).arrayBuffer(),
      );
      return JSON.stringify(
        {
          ...(await measureTranslationCaches({ createCpu, compiler }, options)),
          browserUserAgent: navigator.userAgent,
        },
        (_, item) => (typeof item === 'bigint' ? item.toString() : item),
      );
    }, knobs);
    return { engine: browser.version(), ...JSON.parse(result) };
  } finally {
    try {
      await browser?.close();
    } finally {
      await new Promise((done) => server.close(done));
    }
  }
}

const result = await (browserMode ? browserRun() : nodeRun());
const after = await manifest(candidate.path);
if (candidate.coreSha256 !== after.coreSha256 || candidate.hostSha256 !== after.hostSha256)
  throw Error('frozen candidate artifact changed during characterization');
if (JSON.stringify(sourceSha256) !== JSON.stringify(await sources()))
  throw Error('characterization source changed during observation');
const output = resolve(
  values.output ??
    `build/verification/translation-cache-characterization-${browserMode ? 'chrome' : 'node'}${validateOnly ? '-validation' : ''}.json`,
);
const report = {
  mode: validateOnly
    ? 'candidate-correctness-validation-timing-discarded'
    : 'candidate-only-characterization',
  candidate,
  sourceSha256,
  knobs,
  environment: { platform: process.platform, arch: process.arch, cpu: os.cpus()[0]?.model },
  ...result,
};
await mkdir(dirname(output), { recursive: true });
await writeFile(output, encode(report));
console.log(
  encode({
    mode: report.mode,
    cells: definitions.length,
    checkedSamples: result.samples.length,
    engine: result.engine,
    output,
  }),
);
