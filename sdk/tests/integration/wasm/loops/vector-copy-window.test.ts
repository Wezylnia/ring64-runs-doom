import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { BANK, PC, initial, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('paired vector windows preserve overlapping load/store order and exact checked replay', async () => {
  // Two alternating unaligned vector copies, an index advance, and one backward branch.
  const code = '0f1004070f1104060f104407100f114406104883c0204839c872e5';
  const source = PC + 0x1003n;
  const destination = source + 16n;
  const stop = PC + BigInt(code.length / 2);
  const seed = Uint8Array.from({ length: 16 }, (_, index) => index * 23 + 1);
  const ram = new Uint8Array(0x3000);
  ram.set(seed, 3);
  ram[0x100] = 0xf4;

  const proto = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = proto.artifactImports;
  let accepted = 0;
  let refused = 0;
  proto.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const resolve = env.paged_loop_copy_window;
    if (typeof resolve === 'function') {
      env.paged_loop_copy_window = (src: bigint, dst: bigint, width: number) => {
        const result = resolve(src, dst, width);
        if (result) accepted++;
        else refused++;
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
        bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
        words: [
          [0x4008n, 0xa003n],
          [0x4010n, 0xb003n],
          [0x4018n, 0xc001n],
        ],
        spans: [{ physical: 0xa000n, bytes: ram }],
      },
      async (cpu) => {
        const enter = (dst = destination, limit = 96n, src = source) =>
          cpu.setState(initial(PC, { 0: 0n, 1: limit, 6: dst, 7: src }));
        const expected = Uint8Array.from({ length: 96 }, (_, index) => seed[index % 16]!);
        for (let wave = 0; wave < 4; wave++) {
          enter();
          const run = await cpu.run({ budget: 100n, stop });
          assert.equal(run.reason, 'stop');
          assert.equal(run.retired, 21n);
          assert.deepEqual(cpu.readPhysicalMemory(0xa013n, 96), expected);
        }
        assert.ok(accepted > 0);
        const beforeStop = accepted;
        enter();
        const interior = await cpu.run({ budget: 100n, stop: PC + 8n });
        assert.equal(interior.retired, 2n);
        assert.equal(accepted, beforeStop);
        enter();
        const budget = await cpu.run({ budget: 8n, stop });
        assert.equal(budget.reason, 'budget');
        assert.equal(budget.retired, 8n);
        assert.equal(budget.state.rip, PC + 4n);

        const beforeSplit = refused;
        enter(PC + 0x2ff0n, 32n);
        const split = await cpu.run({ budget: 100n, stop });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.retired, 3n);
        assert.equal(split.state.rip, PC + 13n);
        assert.deepEqual(cpu.readPhysicalMemory(0xbff0n, 16), seed);
        assert.deepEqual(cpu.readPhysicalMemory(0xc000n, 16), new Uint8Array(16));
        assert.equal(refused, beforeSplit + 1);

        enter(PC + 18n, 32n, PC + 0x1100n);
        const alias = await cpu.run({ budget: 100n, stop });
        assert.equal(alias.reason, 'halted');
        assert.equal(alias.retired, 5n);
        assert.equal(alias.state.rip, PC + 19n);
      },
    );
  } finally {
    proto.artifactImports = imports;
  }
});
