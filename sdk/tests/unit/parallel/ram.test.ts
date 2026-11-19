import assert from 'node:assert/strict';
import test from 'node:test';
import { bindRam, ramModule } from '../../../src/parallel/memory/ram.js';

test('shared helper functions access the supplied shared memory at both prescribed widths', () => {
  const privateMemory = new WebAssembly.Memory({ initial: 1 });
  const sharedMemory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  const helpers = bindRam(ramModule(), privateMemory, sharedMemory);
  helpers.store_lane(8, 0x1122334400000000n);
  assert.equal(
    helpers.compare_exchange_lane(8, 0x1122334400000000n, 0x5522334400000000n),
    0x1122334400000000n,
  );
  assert.equal(helpers.load_lane(8), 0x5522334400000000n);
  helpers.store_control(0, 0x7733);
  assert.equal(helpers.compare_exchange_control(0, 0x7733, 1), 0x7733);
  assert.equal(helpers.load_control(0), 1);
  assert.deepEqual([...new Uint8Array(privateMemory.buffer, 0, 16)], Array<number>(16).fill(0));
});
