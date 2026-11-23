import assert from 'node:assert/strict';
import { bindGuestMemory64 } from '../../src/wasm/guest-memory64.js';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { checkMemory64, supportsMemory64 } from '../../../tests/execution/memory/memory64.js';

test(
  'memory64 backing reads and writes cross 4 GiB with intact aliases and faults',
  {
    skip: !supportsMemory64() && 'runtime lacks Wasm memory64',
  },
  async () => {
    const bytes = await readFile(new URL('../../../build/ring64-memory64.wasm', import.meta.url));
    await checkMemory64(await WebAssembly.compile(new Uint8Array(bytes)));
  },
);

test(
  'requesting memory64 on an unsupported engine reports the missing Wasm features',
  {
    skip: supportsMemory64(),
  },
  () => {
    assert.throws(bindGuestMemory64, /memory64.*multi-memory/);
  },
);
