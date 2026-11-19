import assert from 'node:assert/strict';
import test from 'node:test';
import { SharedControl, SLOT } from '../../../src/parallel/control.js';

test('a phase close between publishing and rechecking a lease prevents entry', () => {
  const memory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  const coordinator = new SharedControl(memory);
  coordinator.initialize();
  class Interrupted extends SharedControl {
    reads = 0;
    override get phase(): bigint {
      if (++this.reads === 2) {
        assert.equal(coordinator.load(0, SLOT.active), 2n);
        coordinator.transition(true);
      }
      return super.phase;
    }
  }
  const worker = new Interrupted(memory);
  assert.equal(worker.acquire(0, 0n), false);
  assert.equal(coordinator.load(0, SLOT.active), 0n);
});

test('a reopened phase cannot admit a worker before its exact invalidation arrives', () => {
  const memory = new WebAssembly.Memory({ initial: 1, maximum: 1, shared: true });
  const coordinator = new SharedControl(memory);
  coordinator.initialize();
  const worker = new SharedControl(memory);
  assert.equal(worker.load(0, SLOT.invalidations), 0n);
  coordinator.transition(true);
  coordinator.store(0, SLOT.invalidations, 1n);
  coordinator.transition(false);
  assert.equal(worker.acquire(0, 0n), false);
  assert.equal(worker.load(0, SLOT.active), 0n);
  assert.equal(worker.acquire(0, 1n), true);
  assert.equal(worker.load(0, SLOT.active), 4n);
});
