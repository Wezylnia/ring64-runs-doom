import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { BANK, PC, initial, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('guarded vector copies preserve exact stop, page fault, debug and code write boundaries', async () => {
  const code = '0f1004070f1104064883c0104839c872ef';
  const stop = PC + BigInt(code.length / 2);
  const source = PC + 0x1003n;
  const target = PC + 0x2005n;
  const split = PC + 0x2ff8n;
  const ram = new Uint8Array(0x3000);
  const pattern = Uint8Array.from({ length: 64 }, (_, index) => 2 + index * 19);
  ram.set(pattern, 3);
  ram[0x100] = 0xf4; // HLT for the code-alias continuation.

  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let successes = 0;
  let refusals = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const store = env.paged_loop_write_block;
    if (typeof store === 'function') {
      env.paged_loop_write_block = (address: bigint, pointer: number) => {
        const result = store(address, pointer);
        if (result === 1) successes++;
        else refusals++;
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
        const state = (destination: bigint, length = 64n, flags = 2n, input = source) =>
          initial(PC, { 0: 0n, 1: length, 6: destination, 7: input }, flags);
        for (let wave = 0; wave < 4; wave++) {
          cpu.setState(state(target));
          const copied = await cpu.run({ budget: 100n, stop });
          assert.equal(copied.reason, 'stop');
          assert.equal(copied.retired, 20n);
          assert.deepEqual(cpu.readPhysicalMemory(0xb005n, 64), pattern);
        }
        assert.ok(successes > 0);
        const beforeStop = successes;
        cpu.setState(state(target));
        assert.equal((await cpu.run({ budget: 100n, stop: PC + 4n })).retired, 1n);
        assert.equal(successes, beforeStop);

        const beforeFault = refusals;
        cpu.setState(state(split, 16n));
        const failure = await cpu.run({ budget: 100n, stop });
        assert.equal(failure.reason, 'page-fault');
        assert.equal(failure.retired, 1n);
        assert.equal(failure.state.rip, PC + 4n);
        assert.deepEqual(cpu.readPhysicalMemory(0xbff8n, 16), new Uint8Array(16));
        assert.equal(refusals, beforeFault + 1);

        cpu.setState(state(target, 16n, 0x102n));
        const debug = await cpu.run({ budget: 100n, stop });
        assert.equal(debug.reason, 'debug-exception');
        assert.equal(debug.retired, 1n);

        cpu.setState(state(PC + 8n, 16n, 2n, PC + 0x1100n));
        const patched = await cpu.run({ budget: 100n, stop });
        assert.equal(patched.reason, 'halted');
        assert.equal(patched.retired, 3n);
        assert.equal(patched.state.rip, PC + 9n);
        assert.equal(cpu.readPhysicalMemory(0x9008n, 1)[0], 0xf4);
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});
