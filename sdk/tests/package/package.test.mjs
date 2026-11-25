import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { copyFile, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { tmpdir } from 'node:os';
import { extname, join, resolve, sep } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { chromium } from 'playwright';
import { awaitBrowserResult } from '../../../scripts/check/browser-result.mjs';

const exec = promisify(execFile);
const root = fileURLToPath(new URL('../../../', import.meta.url));

test(
  'the packed SDK has strict public typings and executes in Node and Chrome',
  { timeout: 300_000 },
  async (t) => {
    const project = await mkdtemp(join(tmpdir(), 'ring64-sdk-consumer-'));
    t.after(() => rm(project, { recursive: true, force: true }));
    await writeFile(join(project, 'package.json'), '{"private":true,"type":"module"}\n');
    const { stdout } = await exec(
      'npm',
      [
        'pack',
        resolve(root, 'build/npm/ring64'),
        '--offline',
        '--json',
        '--ignore-scripts',
        '--pack-destination',
        project,
      ],
      { cwd: project },
    );
    const [packed] = JSON.parse(stdout);
    const files = new Set(packed.files.map(({ path }) => path));
    for (const expected of [
      'LICENSE',
      'THIRD_PARTY_NOTICES.txt',
      'index.js',
      'index.d.ts',
      'machine.js',
      'machine.d.ts',
      'ring64.wasm',
      'ring64-parallel.wasm',
      'ring64-memory64.wasm',
      'ring64-parallel-memory64.wasm',
      'parallel/worker/entry.js',
      'parallel/cpu/engine.js',
      'parallel/cpu/engine.d.ts',
      'parallel/cpu/core.js',
      'parallel/cpu/core.d.ts',
    ])
      assert.ok(files.has(expected), `missing package member ${expected}`);
    for (const file of files) {
      assert.ok(!file.split('/').some((part) => part === 'src' || part === 'tests'), file);
      assert.ok(!/\.test\.(?:js|d\.ts)$/.test(file), file);
      assert.ok(!/\.generated\.(?:js|d\.ts)$/.test(file), file);
    }
    await exec(
      'npm',
      [
        'install',
        '--offline',
        '--ignore-scripts',
        '--no-package-lock',
        '--no-audit',
        '--no-fund',
        join(project, packed.filename),
      ],
      { cwd: project },
    );
    const installedManifest = JSON.parse(
      await readFile(join(project, 'node_modules/ring64/package.json'), 'utf8'),
    );
    assert.equal(installedManifest.license, 'MIT');
    const quickstart = await readFile(resolve(root, 'website/docs/examples/add.ts'), 'utf8');
    const parallel = await readFile(
      resolve(root, 'website/docs/examples/api/run-together.ts'),
      'utf8',
    );
    const readme = await readFile(resolve(root, 'README.md'), 'utf8');
    assert.ok(readme.includes('```ts\n' + quickstart.trimEnd() + '\n```'), 'README quickstart');
    assert.ok(readme.includes('```ts\n' + parallel.trimEnd() + '\n```'), 'README parallel example');
    await writeFile(join(project, 'add.ts'), quickstart);
    const quickstartResult = await exec(
      process.execPath,
      ['--experimental-strip-types', 'add.ts'],
      { cwd: project, timeout: 30_000 },
    );
    assert.equal(quickstartResult.stdout.trim(), '42n');
    for (const file of ['consumer.ts', 'contracts.ts'])
      await copyFile(new URL(file, import.meta.url), join(project, file));
    await exec(
      process.execPath,
      [
        resolve(root, 'node_modules/typescript/bin/tsc'),
        '--target',
        'ES2022',
        '--module',
        'NodeNext',
        '--lib',
        'ES2022,DOM',
        '--strict',
        '--noUncheckedIndexedAccess',
        '--exactOptionalPropertyTypes',
        '--noEmitOnError',
        '--outDir',
        'out',
        'consumer.ts',
        'contracts.ts',
      ],
      { cwd: project },
    ).catch((error) => {
      throw new Error(error.stdout || error.message, { cause: error });
    });
    await writeFile(
      join(project, 'check.mjs'),
      `
    import assert from 'node:assert/strict';
    import { checkCpu, checkParallel, supportsWide } from './out/consumer.js';
    await assert.rejects(import('ring64/wasm/core.js'), { code: 'ERR_PACKAGE_PATH_NOT_EXPORTED' });
    await checkCpu();
    await checkCpu('memory32');
    if (supportsWide) await checkCpu('memory64');
    await checkParallel();
    await checkParallel('memory32');
    await checkParallel('memory64');
  `,
    );
    await exec(process.execPath, ['check.mjs'], { cwd: project, timeout: 30_000 });

    await checkBrowser(project);
  },
);

async function checkBrowser(project) {
  const moduleRoot = '/node_modules/ring64/';
  const manifest = JSON.parse(await readFile(join(project, moduleRoot, 'package.json'), 'utf8'));
  const imports = Object.fromEntries(
    Object.entries(manifest.exports).map(([name, entry]) => [
      name === '.' ? manifest.name : manifest.name + name.slice(1),
      moduleRoot + entry.import.slice(2),
    ]),
  );
  const server = createServer(async (request, response) => {
    response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
    response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    const pathname = new URL(request.url ?? '/', 'http://localhost').pathname;
    if (pathname === '/') {
      response.setHeader('Content-Type', 'text/html');
      response.end(
        `<!doctype html><script type="importmap">${JSON.stringify({ imports })}</script>`,
      );
      return;
    }
    try {
      const file = resolve(project, '.' + pathname);
      if (
        !file.startsWith(resolve(project, 'node_modules/ring64') + sep) &&
        pathname !== '/out/consumer.js'
      )
        throw new Error('not a package asset');
      const bytes = await readFile(file);
      response.setHeader(
        'Content-Type',
        extname(file) === '.wasm' ? 'application/wasm' : 'text/javascript',
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
    const executablePath = process.env.RING64_BROWSER;
    browser = await chromium.launch({
      headless: true,
      ...(executablePath ? { executablePath } : {}),
    });
    const page = await browser.newPage();
    const paths = [];
    page.on('request', (request) => paths.push(new URL(request.url()).pathname));
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await runInBrowser(page, 'checkCpu');
    assert.equal(paths.filter((path) => path.endsWith('.wasm')).length, 1);
    assert.ok(
      !paths.some((path) =>
        /\/parallel\/|\/machine\.js$|ring64-parallel(-memory64)?\.wasm$/.test(path),
      ),
    );
    await runInBrowser(page, 'checkParallel');
    assert.equal(paths.filter((path) => /ring64-parallel(-memory64)?\.wasm$/.test(path)).length, 1);
  } finally {
    try {
      await browser?.close();
    } finally {
      await new Promise((resolve) => server.close(resolve));
    }
  }
}

async function runInBrowser(page, method) {
  await page.evaluate(
    ({ entry, method }) => {
      globalThis.ring64PackageResult = import(entry).then((consumer) => consumer[method]());
    },
    { entry: '/out/consumer.js', method },
  );
  await awaitBrowserResult(page, 'ring64PackageResult', `packed SDK ${method} timed out`);
}
