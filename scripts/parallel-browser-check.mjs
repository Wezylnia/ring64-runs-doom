import { createServer } from 'node:http';
import { createHash } from 'node:crypto';
import { access, mkdir, readFile, writeFile } from 'node:fs/promises';
import { extname, resolve, sep } from 'node:path';
import { chromium, firefox } from 'playwright';

const root = resolve('.');
const base = resolve(root, 'build') + sep;
const only = process.argv[2];
if (only && only !== 'chromium' && only !== 'firefox') {
  throw Error('usage: parallel-browser-check.mjs [chromium|firefox]');
}
const server = createServer(async (request, response) => {
  try {
    const pathname = new URL(request.url ?? '/', 'http://localhost').pathname;
    if (pathname !== '/unisolated') {
      response.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
      response.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
    }
    response.setHeader('Cache-Control', 'no-store');
    if (pathname === '/' || pathname === '/unisolated') {
      response.setHeader('Content-Type', 'text/html; charset=utf-8');
      response.end(
        '<!doctype html><script type="module" src="/build/tests/runners/parallel-browser.js"></script>',
      );
      return;
    }
    const file = resolve(root, '.' + pathname);
    if (!file.startsWith(base)) {
      response.writeHead(404).end();
      return;
    }
    const data = await readFile(file);
    response.setHeader(
      'Content-Type',
      extname(file) === '.wasm' ? 'application/wasm' : 'text/javascript',
    );
    response.end(data);
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise((resolve, reject) => {
  server.once('error', reject);
  server.listen(0, '127.0.0.1', resolve);
});
const host = server.address();
try {
  const bytes = await readFile('build/ring64-parallel.wasm');
  const coreSha256 = createHash('sha256').update(bytes).digest('hex');
  const rows = [];
  await mkdir('build/verification', { recursive: true });
  for (const [name, engine] of [
    ['chromium', chromium],
    ['firefox', firefox],
  ]) {
    if (only && only !== name) continue;
    let executablePath =
      name === 'firefox' ? process.env.RING64_FIREFOX : process.env.RING64_BROWSER;
    if (!executablePath && name === 'chromium' && process.platform === 'darwin') {
      const chrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
      try {
        await access(chrome);
        executablePath = chrome;
      } catch {
        // Use the pinned Playwright runtime when system Chrome is absent.
      }
    }
    const browser = await engine.launch({
      headless: true,
      ...(executablePath ? { executablePath } : {}),
    });
    try {
      const page = await browser.newPage();
      const passed = [];
      for (const pathname of ['/', '/unisolated']) {
        await page.goto(`http://127.0.0.1:${host.port}${pathname}`);
        await page.waitForFunction(() => globalThis.ring64Parallel !== undefined, undefined, {
          timeout: 30_000,
        });
        const conformance = await page.evaluate(async () => {
          try {
            return await Promise.race([
              globalThis.ring64Parallel,
              new Promise((_, reject) =>
                setTimeout(() => reject(Error('parallel browser run timed out')), 120_000),
              ),
            ]);
          } catch (error) {
            const messages = [];
            for (let value = error; value; value = value.cause) messages.push(String(value));
            throw new Error(messages.join('\n'));
          }
        });
        passed.push(...conformance.passed);
      }
      const receipt = { name, engine: browser.version(), coreSha256, passed };
      rows.push(receipt);
      await writeFile(
        `build/verification/parallel-browser-${name}.json`,
        JSON.stringify(receipt, null, 2) + '\n',
      );
      console.log(`${name} ${browser.version()}: ${passed.length} parallel scenarios passed`);
    } finally {
      await browser.close();
    }
  }
  await writeFile('build/verification/parallel-browser.json', JSON.stringify(rows, null, 2) + '\n');
} finally {
  await new Promise((resolve) => server.close(resolve));
}
