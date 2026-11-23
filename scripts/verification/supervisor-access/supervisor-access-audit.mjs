import { createServer } from 'node:http';
import { mkdir, readFile, readdir, writeFile } from 'node:fs/promises';
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
  : await freeze(resolve(values.current ?? root), `supervisor-access-build-${tag}`);
const suite = 'tests/execution/memory/supervisor-access';
async function oracleSources() {
  const files = await readdir(resolve(root, suite), { recursive: true });
  return files
    .filter((name) => name.endsWith('.ts'))
    .map((name) => name.split(sep).join('/'))
    .sort();
}
const oracleFiles = await oracleSources();
const sources = [
  'scripts/verification/supervisor-access/supervisor-access-audit.mjs',
  'scripts/verification/artifacts.mjs',
  'scripts/verification/browser.mjs',
  'scripts/verification/oracles/stage.mjs',
  ...oracleFiles.flatMap((name) => [
    `${suite}/${name}`,
    `build/${suite}/${name.replace(/\.ts$/, '.js')}`,
  ]),
  ...['context', 'literals'].flatMap((name) => [
    `tests/support/${name}.ts`,
    `build/tests/support/${name}.js`,
  ]),
];
const sourceHashes = async () => {
  const inventory = await oracleSources();
  if (JSON.stringify(inventory) !== JSON.stringify(oracleFiles))
    throw Error('supervisor source inventory changed during audit; discard run');
  return Object.fromEntries(
    await Promise.all(
      sources.map(async (name) => [name, hash(await readFile(resolve(root, name)))]),
    ),
  );
};
const before = await sourceHashes();
const verifyStagedOracle = await stageOracle(current.path, before);
let result;
if (!browserMode) {
  const { createCpu } = await import(pathToFileURL(resolve(current.path, 'build/host/index.js')));
  const { checkSupervisorAccess, groups } = await import(
    pathToFileURL(resolve(current.path, 'build/tests/execution/memory/supervisor-access/oracle.js'))
  );
  if (selected?.some((name) => !groups.includes(name)))
    throw Error('unknown supervisor access group');
  const compiler = await WebAssembly.compile(
    await readFile(resolve(current.path, 'build/ring64.wasm')),
  );
  result = await checkSupervisorAccess({ createCpu, compiler }, selected);
} else {
  const server = createServer(async (request, response) => {
    try {
      const pathname = new URL(request.url ?? '/', 'http://localhost').pathname;
      if (pathname === '/') {
        response.end('<!doctype html><title>Supervisor access audit</title>');
        return;
      }
      const file = resolve(current.path, '.' + pathname);
      if (!(
        file.startsWith(resolve(current.path, 'build/host') + sep) ||
        file === resolve(current.path, 'build/ring64.wasm') ||
        file.startsWith(resolve(current.path, 'build/tests') + sep)
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
      const { checkSupervisorAccess, groups } =
        await import('/build/tests/execution/memory/supervisor-access/oracle.js');
      if (selected?.some((name) => !groups.includes(name)))
        throw Error('unknown supervisor access group');
      const compiler = await WebAssembly.compile(
        await (await fetch('/build/ring64.wasm')).arrayBuffer(),
      );
      return {
        ...(await checkSupervisorAccess({ createCpu, compiler }, selected)),
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
    `build/verification/supervisor-access-oracle-${browserMode ? 'chrome' : 'node'}-${tag}.json`,
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
