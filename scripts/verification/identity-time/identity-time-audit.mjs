import { createServer } from 'node:http';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { dirname, resolve, sep } from 'node:path';
import { pathToFileURL } from 'node:url';
import os from 'node:os';
import { root, freeze, manifest, hash, encode } from '../artifacts.mjs';
import { evaluateInChromium } from '../browser.mjs';
import { stageOracle } from '../oracles/stage.mjs';

const args = process.argv.slice(2);
const values = {};
for (let i = 0; i < args.length; i++) {
  if (args[i] === '--browser') continue;
  if (
    !['--snapshot', '--current', '--output', '--groups'].includes(args[i]) ||
    args[i + 1] === undefined ||
    args[i + 1].startsWith('--')
  )
    throw Error(`unknown or incomplete argument ${args[i]}`);
  values[args[i].slice(2)] = args[++i];
}
if (values.current && values.snapshot) throw Error('choose --current or --snapshot');
const selected = values.groups?.split(',');
if (selected && (new Set(selected).size !== selected.length || selected.some((name) => !name)))
  throw Error('groups must be nonempty and unique');
const browserMode = args.includes('--browser');
const tag = `${Date.now()}-${process.pid}`;
const current = values.snapshot
  ? await manifest(resolve(values.snapshot))
  : await freeze(resolve(values.current ?? root), `identity-time-build-${tag}`);
const sources = [
  'scripts/verification/identity-time/identity-time-audit.mjs',
  'scripts/verification/artifacts.mjs',
  'scripts/verification/browser.mjs',
  'scripts/verification/oracles/stage.mjs',
  'tests/support/literals.ts',
  'build/tests/support/literals.js',
  'tests/execution/processor/identity-time/oracle.ts',
  'build/tests/execution/processor/identity-time/oracle.js',
];
const sourceHashes = async () =>
  Object.fromEntries(
    await Promise.all(
      sources.map(async (name) => [name, hash(await readFile(resolve(root, name)))]),
    ),
  );
const before = await sourceHashes();
const verifyStagedOracle = await stageOracle(current.path, before);
let result;
if (!browserMode) {
  const { createCpu } = await import(pathToFileURL(resolve(current.path, 'build/host/index.js')));
  const { checkIdentityTime, groups } = await import(
    pathToFileURL(resolve(current.path, 'build/tests/execution/processor/identity-time/oracle.js'))
  );
  if (selected?.some((name) => !groups.includes(name))) throw Error('unknown identity/time group');
  const compiler = await WebAssembly.compile(
    await readFile(resolve(current.path, 'build/ring64.wasm')),
  );
  result = await checkIdentityTime({ createCpu, compiler }, selected);
} else {
  const server = createServer(async (request, response) => {
    try {
      const pathname = new URL(request.url ?? '/', 'http://localhost').pathname;
      if (pathname === '/') {
        response.end('<!doctype html><title>Identity and time audit</title>');
        return;
      }
      const file = resolve(current.path, '.' + pathname);
      if (!(
        file.startsWith(resolve(current.path, 'build/host') + sep) ||
        file === resolve(current.path, 'build/ring64.wasm') ||
        file === resolve(current.path, 'build/tests/support/literals.js') ||
        file === resolve(current.path, 'build/tests/execution/processor/identity-time/oracle.js')
      ))
        throw Error('outside allowed route');
      const bytes = await readFile(file);
      response.writeHead(200, {
        'content-type': pathname.endsWith('.wasm') ? 'application/wasm' : 'text/javascript',
        'cache-control': 'no-store',
      });
      response.end(bytes);
    } catch {
      response.writeHead(404);
      response.end();
    }
  });
  // An ephemeral owned listener has no expected shared port and is always closed below.
  result = await evaluateInChromium(
    server,
    async (selected) => {
      const { createCpu } = await import('/build/host/index.js');
      const { checkIdentityTime, groups } =
        await import('/build/tests/execution/processor/identity-time/oracle.js');
      if (selected?.some((name) => !groups.includes(name)))
        throw Error('unknown identity/time group');
      const compiler = await WebAssembly.compile(
        await (await fetch('/build/ring64.wasm')).arrayBuffer(),
      );
      return {
        ...(await checkIdentityTime({ createCpu, compiler }, selected)),
        browserUserAgent: navigator.userAgent,
      };
    },
    selected,
  );
}
const after = await manifest(current.path);
if (after.coreSha256 !== current.coreSha256 || after.hostSha256 !== current.hostSha256)
  throw Error('frozen Core/host changed during audit; discard run');
if (JSON.stringify(before) !== JSON.stringify(await sourceHashes()))
  throw Error('oracle source changed during audit; discard run');
await verifyStagedOracle();
const output = resolve(
  values.output ??
    `build/verification/identity-time-oracle-${browserMode ? 'chrome' : 'node'}-${tag}.json`,
);
await mkdir(dirname(output), { recursive: true });
await writeFile(
  output,
  encode({
    at: new Date().toISOString(),
    runtime: browserMode ? 'chrome' : 'node',
    mode: 'independent-finite-literal-and-BigInt-oracle',
    environment: {
      node: process.version,
      cpu: os.cpus()[0]?.model,
      platform: os.platform(),
      arch: os.arch(),
    },
    manifest: current,
    sourceSha256: before,
    selected,
    postRunIntegrity: { core: true, host: true, source: true, oracle: true },
    ...result,
  }),
);
console.log({ output, cases: result.cases, groups: result.groups });
