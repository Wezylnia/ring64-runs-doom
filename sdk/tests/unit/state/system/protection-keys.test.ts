import assert from 'node:assert/strict';
import test from 'node:test';
import {
  captureProtectionKeysState,
  validateProtectionKeysLayout,
} from '../../../../src/state/system/protection-keys.js';

test('protection-key capture owns two primitive uint32 words without enumerating input', () => {
  assert.deepEqual(captureProtectionKeysState({ pkru: 0xffffffff, pkrs: 0x80000000 }), {
    pkru: 0xffffffff,
    pkrs: 0x80000000,
  });
  const zero = captureProtectionKeysState({ pkru: -0, pkrs: -0 });
  assert.equal(Object.is(zero.pkru, -0), false);
  assert.equal(Object.is(zero.pkrs, -0), false);
  const invalid: unknown[] = [
    undefined,
    null,
    [],
    Object.assign([], { pkru: 0, pkrs: 0 }),
    {},
    { pkru: 0 },
    { pkrs: 0 },
  ];
  for (const word of [-1, 2 ** 32, 0.5, NaN, Infinity, 0n, '0', Object(0), null, undefined])
    invalid.push({ pkru: word, pkrs: 0 }, { pkru: 0, pkrs: word });
  for (const input of invalid)
    assert.throws(() => captureProtectionKeysState(input as never), TypeError);
  const reads: PropertyKey[] = [];
  const input = new Proxy(
    { pkru: 1, pkrs: 0xffffffff },
    {
      get(target, key, receiver) {
        reads.push(key);
        return Reflect.get(target, key, receiver);
      },
      ownKeys() {
        return assert.fail('capture must not enumerate unrelated properties');
      },
    },
  );
  assert.deepEqual(captureProtectionKeysState(input), { pkru: 1, pkrs: 0xffffffff });
  assert.deepEqual(reads, ['pkru', 'pkrs']);
});

test('protection-key layout accepts descriptor ordering and rejects overlap or truncated spans', () => {
  const layout = { offset: 784, size: 8, pkru: 0, pkrs: 4 };
  validateProtectionKeysLayout(layout, 792, 784);
  validateProtectionKeysLayout({ ...layout, pkru: 4, pkrs: 0 }, 792, 784);
  for (const changed of [
    { offset: 783 },
    { offset: 785 },
    { size: 7 },
    { size: 9 },
    { pkru: 1 },
    { pkrs: 3 },
    { pkrs: 5 },
    { pkru: -1 },
    { pkru: 0.5 },
    { offset: NaN },
    { pkrs: Infinity },
    { offset: 2 ** 32 },
  ])
    assert.throws(
      () => validateProtectionKeysLayout({ ...layout, ...changed }, 792, 784),
      /protection keys layout/,
    );
  for (const invalid of [null, undefined, [], Object.assign([], layout), {}])
    assert.throws(
      () => validateProtectionKeysLayout(invalid as never, 792, 784),
      /protection keys layout/,
    );
  assert.throws(() => validateProtectionKeysLayout(layout, 791, 784), /protection keys layout/);
});
