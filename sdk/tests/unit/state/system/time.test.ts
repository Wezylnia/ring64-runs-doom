import assert from 'node:assert/strict';
import test from 'node:test';
import { validateTimeLayout } from '../../../../src/state/system/time.js';

test('time transport rejects overlapping, truncated and noninteger layout records', () => {
  const layout = { offset: 768, size: 16, tsc: 0, tscAux: 8 };
  validateTimeLayout(layout, 784, 768);
  for (const changed of [
    { offset: 767 },
    { offset: 769 },
    { size: 11 },
    { tsc: 9 },
    { tscAux: 7 },
    { tscAux: 13 },
    { tsc: -1 },
    { size: 1.5 },
    { offset: NaN },
    { tscAux: Infinity },
  ])
    assert.throws(() => validateTimeLayout({ ...layout, ...changed }, 784, 768), /time layout/);
  assert.throws(() => validateTimeLayout(null as never, 784, 768), /time layout/);
});
