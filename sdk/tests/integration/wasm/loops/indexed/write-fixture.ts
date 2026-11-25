import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { supportsGuestMemory64 } from '../../../../../src/wasm/guest-memory64.js';
import { compiler } from '../fixture.js';

const supportsWide = supportsGuestMemory64();
if (process.env.RING64_REQUIRE_MEMORY64 === '1') {
  assert.ok(supportsWide, 'this check requires mixed-width Wasm memory operations');
}
const wideCompiler = supportsWide
  ? await WebAssembly.compile(
      new Uint8Array(
        await readFile(new URL('../../../../../../build/ring64-memory64.wasm', import.meta.url)),
      ),
    )
  : undefined;

export function indexedTest(name: string, run: (compiler: WebAssembly.Module) => Promise<void>) {
  test(`memory32: ${name}`, () => run(compiler));
  test(`memory64: ${name}`, async (t) => {
    if (!wideCompiler) return t.skip('runtime lacks mixed-width Wasm memory operations');
    await run(wideCompiler);
  });
}
