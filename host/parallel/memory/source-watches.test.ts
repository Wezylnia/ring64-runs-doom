import assert from 'node:assert/strict';
import test from 'node:test';
import { sharedLayout } from '../../../sdk/src/parallel/control.js';
import {
  markSourceWatches,
  SourceWatches,
} from '../../../sdk/src/parallel/memory/source-watches.js';

test('sweeping source watches clears only bytes whose last worker owner was released', () => {
  const layout = sharedLayout(2, 4096, 16, 2);
  const memory = new WebAssembly.Memory({
    initial: layout.pages,
    maximum: layout.pages,
    shared: true,
  });
  const words = new Int32Array(memory.buffer, layout.watchBase, 2);
  const snapshot = () => [Atomics.load(words, 0), Atomics.load(words, 1)];
  const watches = new SourceWatches(memory, layout);
  const keep = (cpu: number, owner: bigint, byte: number, length: number) => {
    const ranges = [{ offset: layout.ramBase + byte, length }];
    markSourceWatches(memory, layout, ranges);
    watches.remember(cpu, owner, ranges);
  };

  keep(0, 1n, 31, 3);
  keep(1, 1n, 32, 3);
  assert.deepEqual(snapshot(), [-2147483648, 7]);
  assert.equal(watches.release(0, 1n), true);
  assert.equal(watches.release(0, 1n), false);
  assert.equal(watches.hasReleases, true);
  assert.deepEqual(snapshot(), [-2147483648, 7]);

  keep(0, 2n, 33, 1);
  watches.sweep();
  assert.deepEqual(snapshot(), [0, 7]);
  assert.equal(watches.hasReleases, false);

  assert.equal(watches.release(1, 1n), true);
  watches.sweep();
  assert.deepEqual(snapshot(), [0, 2]);
  assert.equal(watches.release(0, 2n), true);
  watches.sweep();
  assert.deepEqual(snapshot(), [0, 0]);
});
