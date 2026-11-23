import assert from 'node:assert/strict';
import test from 'node:test';
import type { GeneratedRun } from '../../../src/wasm/abi.js';
import { linkedRunBytes } from '../../../src/wasm/linked-run.js';

test('linked owners respect the next stop, remaining quota, and admission', () => {
  const target = 0x2000n;
  const module = new WebAssembly.Module(
    linkedRunBytes(2, [{ pc: target, owner: 17n, entry: 4, slot: 1 }]),
  );
  const cases = [
    { name: 'link', budget: 10n, stop: 0, entered: true },
    { name: 'stop', budget: 10n, stop: 1, entered: false },
    { name: 'budget', budget: 2n, stop: 0, entered: false },
    { name: 'source write', budget: 10n, stop: 0, entered: false },
    { name: 'observed', budget: 10n, stop: 0, entered: false },
    { name: 'stale owner', budget: 10n, stop: 0, entered: false },
  ];
  for (const { name, budget, stop, entered } of cases) {
    const memory = new WebAssembly.Memory({ initial: 1 });
    const state = new DataView(memory.buffer);
    const calls: string[] = [];
    const env = {
      memory,
      raw0(_state: number, remaining: bigint) {
        calls.push('initial');
        state.setBigUint64(136, target, true);
        state.setBigUint64(144, remaining - 2n, true);
        state.setBigUint64(168, 5n, true);
        return 0;
      },
      raw1(...args: Parameters<GeneratedRun>) {
        calls.push('linked');
        assert.deepEqual(args, [0, 8n, target, 0, 4]);
        assert.equal(state.getBigUint64(168, true), 5n);
        state.setBigUint64(144, 7n, true);
        return 1;
      },
      takeInvalidation() {
        return Number(name === 'source write');
      },
      prepare() {
        calls.push('prepare');
        return Number(name === 'observed');
      },
      activate(owner: bigint) {
        calls.push('activate');
        assert.equal(owner, 17n);
        return Number(name !== 'stale owner');
      },
    };
    state.setBigUint64(168, 6n, true);
    const { exports } = new WebAssembly.Instance(module, { env });
    const run = exports.run0 as GeneratedRun;
    assert.equal(run(0, budget, target, stop, 3), Number(entered), name);
    const expected = ['initial'];
    if (entered || name === 'observed' || name === 'stale owner') expected.push('prepare');
    if (entered || name === 'stale owner') expected.push('activate');
    if (entered) expected.push('linked');
    assert.deepEqual(calls, expected, name);
    assert.equal((exports.count as WebAssembly.Global).value, entered ? 2 : 1, name);
    assert.equal((exports.lastUse as WebAssembly.Global).value, entered ? 513n : 1n, name);
    assert.equal(
      (exports.invalidated as WebAssembly.Global).value,
      Number(name === 'source write'),
    );
  }
});
