import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { PC, initial, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('an indexed write loop bypasses grouping on a large data leaf', async () => {
  // Both transparent and opaque paths return to the counted-loop header.
  const code = 'ffc9741548ffc74883c6040fb60785c074ee8b04838906ebe7';
  const data = 0x601000n;
  const count = 32;
  const ram = new Uint8Array(4096);
  const colors = new DataView(ram.buffer);
  const color = 0x13572468;
  const table = 0x80;
  const target = 0x100;
  colors.setUint32(table + 4, color, true);
  const expected = new Uint8Array(count * 4);
  const expectedColors = new DataView(expected.buffer);
  for (let index = 0; index < count; index++) {
    ram[index] = index % 2;
    expectedColors.setUint32(index * 4, ram[index] ? color : 0, true);
  }

  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let lookups = 0;
  let unsuitable = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const group = env.paged_loop_indexed_dword_write;
    if (typeof group === 'function') {
      env.paged_loop_indexed_dword_write = (...args: [bigint, bigint, bigint, number]) => {
        lookups++;
        const result = group(...args);
        if (result === 1) unsuitable++;
        return result;
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        code,
        words: [[0x3018n, 0x600083n]],
        spans: [{ physical: data, bytes: ram }],
      },
      async (cpu) => {
        const start = initial(PC, {
          1: BigInt(count + 1),
          3: data + BigInt(table),
          6: data + BigInt(target - 4),
          7: data - 1n,
        });
        for (let wave = 0; wave < 3; wave++) {
          cpu.writePhysicalMemory(data + BigInt(target), new Uint8Array(expected.byteLength));
          cpu.setState(start);
          const before = lookups;
          const result = await cpu.run({ budget: 1000n, stop: PC + BigInt(code.length / 2) });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 274n);
          assert.deepEqual(
            cpu.readPhysicalMemory(data + BigInt(target), expected.byteLength),
            expected,
          );
          assert.ok(lookups - before < 4, 'a large data leaf must not be regrouped every lap');
        }
        assert.ok(unsuitable > 0, 'the optimized clone observed the large data leaf');
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});
