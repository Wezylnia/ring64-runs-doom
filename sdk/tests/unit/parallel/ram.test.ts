import assert from 'node:assert/strict';
import test from 'node:test';
import { bindRam, ramModule, wasmRamOffset } from '../../../src/parallel/memory/ram.js';

for (const wideOffsets of [false, true]) {
  test(`shared memory32 helpers accept ${wideOffsets ? 64 : 32}-bit compiler offsets`, () => {
    const privateMemory = new WebAssembly.Memory({ initial: 1 });
    const sharedMemory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
    const helpers = bindRam(ramModule(wideOffsets, false), privateMemory, sharedMemory);
    const lane = wasmRamOffset(8, wideOffsets);
    const control = wasmRamOffset(0, wideOffsets);
    helpers.store_lane(lane, 0x1122334400000000n);
    assert.equal(
      helpers.compare_exchange_lane(lane, 0x1122334400000000n, 0x5522334400000000n),
      0x1122334400000000n,
    );
    assert.equal(helpers.load_lane(lane), 0x5522334400000000n);
    helpers.store_control(control, 0x7733);
    assert.equal(helpers.compare_exchange_control(control, 0x7733, 1), 0x7733);
    assert.equal(helpers.load_control(control), 1);
    assert.deepEqual([...new Uint8Array(privateMemory.buffer, 0, 16)], Array<number>(16).fill(0));
  });
}
