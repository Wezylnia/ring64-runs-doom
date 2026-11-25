import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { chromium } from 'playwright';
import { awaitBrowserResult } from './browser-result.mjs';
import { startBrowserServer } from './browser-server.mjs';
import { chromiumExecutable } from '../support/browser.mjs';

const html = `<!doctype html><meta charset="utf-8"><title>ring64 execution checks</title>
<script type="module" src="/build/tests/runners/browser.js"></script>`;
const server = await startBrowserServer(html);
let browser;
try {
  const executablePath = await chromiumExecutable();
  browser = await chromium.launch({
    headless: true,
    ...(executablePath ? { executablePath } : {}),
  });
  const page = await browser.newPage();
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await page.goto(server.origin);
  // Chromium runs the full suite for both guest memory profiles when supported.
  const encoded = await awaitBrowserResult(
    page,
    'ring64Result',
    'browser execution checks timed out',
    240_000,
  );
  if (errors.length) throw new Error(errors.join('\n'));
  const result = JSON.parse(encoded);
  const binary = await readFile('build/ring64.wasm');
  const evidence = {
    browser: browser.version(),
    headless: true,
    coreSha256: createHash('sha256').update(binary).digest('hex'),
    tools: {
      rust: execFileSync('rustc', ['--version'], { encoding: 'utf8' }).trim(),
      cargo: execFileSync('cargo', ['--version'], { encoding: 'utf8' }).trim(),
      node: process.version,
      typescript: JSON.parse(await readFile('node_modules/typescript/package.json', 'utf8'))
        .version,
      playwright: JSON.parse(await readFile('node_modules/playwright/package.json', 'utf8'))
        .version,
    },
    lockHashes: {
      cargo: createHash('sha256')
        .update(await readFile('Cargo.lock'))
        .digest('hex'),
      npm: createHash('sha256')
        .update(await readFile('package-lock.json'))
        .digest('hex'),
    },
    coreBytes: binary.length,
    ...result,
  };
  await mkdir('build/verification', { recursive: true });
  await writeFile('build/verification/browser.json', JSON.stringify(evidence, null, 2));
  console.log(
    `browser ${evidence.browser}: ${evidence.conformance.passed.length} scenarios passed; evidence build/verification/browser.json`,
  );
} finally {
  try {
    await browser?.close();
  } finally {
    await server.close();
  }
}
