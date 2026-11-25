import { createHash } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { chromium, firefox } from 'playwright';
import { awaitBrowserResult } from './browser-result.mjs';
import { startBrowserServer } from './browser-server.mjs';
import { chromiumExecutable } from '../support/browser.mjs';

const only = process.argv[2];
if (only && only !== 'chromium' && only !== 'firefox') {
  throw Error('usage: parallel-browser-check.mjs [chromium|firefox]');
}
const server = await startBrowserServer(
  '<!doctype html><script type="module" src="/build/tests/runners/parallel-browser.js"></script>',
  { isolated: true },
);
try {
  const bytes = await readFile('build/ring64-parallel.wasm');
  const coreSha256 = createHash('sha256').update(bytes).digest('hex');
  const memory64Bytes = await readFile('build/ring64-parallel-memory64.wasm');
  const memory64CoreSha256 = createHash('sha256').update(memory64Bytes).digest('hex');
  const rows = [];
  await mkdir('build/verification', { recursive: true });
  for (const [name, engine] of [
    ['chromium', chromium],
    ['firefox', firefox],
  ]) {
    if (only && only !== name) continue;
    const executablePath =
      name === 'firefox' ? process.env.RING64_FIREFOX : await chromiumExecutable();
    const browser = await engine.launch({
      headless: true,
      ...(executablePath ? { executablePath } : {}),
    });
    try {
      const page = await browser.newPage();
      const passed = [];
      let memory64 = false;
      for (const pathname of ['/', '/unisolated']) {
        await page.goto(server.origin + pathname);
        const conformance = await awaitBrowserResult(
          page,
          'ring64Parallel',
          'parallel browser run timed out',
        );
        passed.push(...conformance.passed);
        if (conformance.memory64) memory64 = true;
      }
      if (!memory64 && process.env.RING64_REQUIRE_PARALLEL_MEMORY64 === '1')
        throw Error(`${name} did not run shared memory64 conformance`);
      const receipt = {
        name,
        engine: browser.version(),
        coreSha256,
        ...(memory64 ? { memory64CoreSha256 } : {}),
        passed,
      };
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
  await server.close();
}
