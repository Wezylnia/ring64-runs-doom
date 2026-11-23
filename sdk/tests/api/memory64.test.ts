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

test(
  'bounded memory64 backings stay aligned when first-fit reuses a hole',
  {
    skip: !supportsMemory64(),
  },
  () => {
    const { allocate, release, ram } = bindGuestMemory64().guest64;
    const code = allocate(89n, 16, 16);
    const data = allocate(7n, 16, 16);
    const neighbor = allocate(31n, 16, 16);
    assert.equal(data % 16n, 0n);
    assert.equal(neighbor % 16n, 0n);
    const bytes = new Uint8Array(ram.buffer);
    bytes.fill(0xcc, Number(data), Number(data) + 7);
    bytes[Number(neighbor)] = 0xab;
    release(data);
    const reused = allocate(7n, 16, 16);
    assert.equal(reused, data);
    assert.deepEqual(bytes.slice(Number(data), Number(data) + 7), new Uint8Array(7));
    assert.equal(bytes[Number(neighbor)], 0xab);
    release(neighbor);
    release(reused);
    release(code);
  },
);
