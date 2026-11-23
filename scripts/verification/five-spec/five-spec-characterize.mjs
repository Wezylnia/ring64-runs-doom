import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { dirname, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import os from 'node:os';
import { encode, freeze, hash, manifest, root } from '../artifacts.mjs';
import { measureFive, newSpecNames } from './five-spec-kernels.mjs';
import { evaluateInChromium } from '../browser.mjs';

const args = process.argv.slice(2);
const value = (name, fallback) => (args.includes(name) ? args[args.indexOf(name) + 1] : fallback);
const browserMode = args.includes('--browser');
const tag = value('--tag', new Date().toISOString().replaceAll(/[:.]/g, '-'));
const current = args.includes('--snapshot')
  ? await manifest(resolve(value('--snapshot')))
  : await freeze(value('--current', root), `five-spec-characterization-build-${tag}`);
const output = resolve(
  value(
    '--output',
    `build/verification/five-spec-characterization-${browserMode ? 'chrome' : 'node'}-${tag}.json`,
  ),
);
const knobs = {
  iterations: Number(value('--iterations', 65536)),
  samples: Number(value('--samples', 7)),
  workloads: value('--workloads', newSpecNames.join(',')).split(','),
};
for (const [name, max] of [
  ['iterations', 1000001],
  ['samples', 101],
])
  if (!Number.isSafeInteger(knobs[name]) || knobs[name] < 1 || knobs[name] > max)
    throw Error(name + ' outside allowed integer range');
if (knobs.workloads.some((name) => !newSpecNames.includes(name)))
  throw Error('unknown new-spec workload');
let results;
if (!browserMode) {
  const { createCpu } = await import(pathToFileURL(resolve(current.path, 'build/host/index.js')));
  const compiler = await WebAssembly.compile(
    await readFile(resolve(current.path, 'build/ring64.wasm')),
  );
  results = await measureFive({ label: 'current', createCpu, compiler }, knobs);
} else {
  const server = createServer(async (request, response) => {
    try {
      const pathname = new URL(request.url ?? '/', 'http://localhost').pathname;
      if (pathname === '/') {
        response.end('<!doctype html><title>five-spec characterization</title>');
        return;
      }
      const base = pathname.startsWith('/build/') ? current.path : root;
      const file = resolve(base, '.' + pathname);
      if (!(
        file.startsWith(resolve(base, 'build') + sep) ||
        [
          'scripts/verification/five-spec/five-spec-kernels.mjs',
          'scripts/verification/five-spec/five-spec-transitions.mjs',
          'scripts/verification/five-spec/five-spec-x87-extra.mjs',
          'scripts/verification/five-spec/five-spec-numeric-extra.mjs',
          'scripts/verification/five-spec/five-spec-legacy-extra.mjs',
        ].some((path) => file === resolve(root, path))
      ))
        throw Error('outside allowed route');
      response.writeHead(200, {
        'content-type': file.endsWith('.wasm') ? 'application/wasm' : 'text/javascript',
        'cache-control': 'no-store',
      });
      response.end(await readFile(file));
    } catch {
      response.writeHead(404);
      response.end();
    }
  });
  results = await evaluateInChromium(
    server,
    async (knobs) => {
      const { measureFive } = await import('/scripts/verification/five-spec/five-spec-kernels.mjs');
      const { createCpu } = await import('/build/host/index.js');
      const compiler = await WebAssembly.compile(
        await (await fetch('/build/ring64.wasm')).arrayBuffer(),
      );
      return {
        ...(await measureFive({ label: 'current', createCpu, compiler }, knobs)),
        browserUserAgent: navigator.userAgent,
      };
    },
    knobs,
  );
}
await mkdir(dirname(output), { recursive: true });
await writeFile(
  output,
  encode({
    at: new Date().toISOString(),
    runtime: browserMode ? 'chrome' : 'node',
    environment: {
      node: process.version,
      cpu: os.cpus()[0]?.model,
      platform: os.platform(),
      arch: os.arch(),
    },
    manifest: current,
    knobs,
    kernelSha256: hash(
      await readFile(resolve(root, 'scripts/verification/five-spec/five-spec-kernels.mjs')),
    ),
    transitionFixtureSha256: hash(
      await readFile(resolve(root, 'scripts/verification/five-spec/five-spec-transitions.mjs')),
    ),
    extraFixtureSha256: Object.fromEntries(
      await Promise.all(
        ['x87', 'numeric', 'legacy'].map(async (name) => [
          name,
          hash(
            await readFile(
              resolve(root, `scripts/verification/five-spec/five-spec-${name}-extra.mjs`),
            ),
          ),
        ]),
      ),
    ),
    ...results,
  }),
);
console.log({ output, definitions: results.definitions.length, samples: results.samples.length });
