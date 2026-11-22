import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { PC, initial, little, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('an ordinary paged MOV reuses a writable leaf and replays for a data breakpoint', async () => {
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const address = PC + 0x1040n;
  const stop = PC + 2n;
  let calls = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const store = env.store;
    if (typeof store === 'function') {
      env.store = (target: bigint, width: number, value: bigint) => {
        if (target === address) calls++;
        return store(target, width, value);
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        // MOV [RDI], EAX; HLT.
        code: '8907f4',
        words: [[0x4008n, 0xa003n]],
        spans: [{ physical: 0xa000n, bytes: new Uint8Array(4096) }],
      },
      async (cpu) => {
        for (let wave = 0; wave < 4; wave++) {
          cpu.setState(initial(PC, { 0: BigInt(16 + wave), 7: address }));
          const before = calls;
          const result = await cpu.run({ stop, budget: 10n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 1n);
          assert.deepEqual(cpu.readPhysicalMemory(0xa040n, 4), Uint8Array.of(16 + wave, 0, 0, 0));
          if (wave === 3) assert.equal(calls, before);
        }
        assert.ok(calls > 0);
        cpu.setDebugState({ addresses: [address, 0n, 0n, 0n], dr6: 0n, dr7: 0xd0401n });
        cpu.setState(initial(PC, { 0: 42n, 7: address }));
        const before = calls;
        const watched = await cpu.run({ stop, budget: 10n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.retired, 1n);
        assert.equal(watched.state.rip, stop);
        assert.deepEqual(cpu.readPhysicalMemory(0xa040n, 4), Uint8Array.of(42, 0, 0, 0));
        assert.equal(calls, before + 1);
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});

test('sampled PUSH reuses a checked writable stack leaf and honors a data breakpoint', async () => {
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const compile = Core.prototype.compile;
  const pop = PC + 2n;
  const stop = PC + 3n;
  const sp = PC + 0x1040n;
  let calls = 0;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) {
      return compile.call(this, pc, limits, [PC, pop, stop], true);
    }
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const store = env.store;
    if (typeof store === 'function') {
      env.store = (address: bigint, width: number, value: bigint) => {
        if (address === sp - 8n) calls++;
        return store(address, width, value);
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        // PUSH 0x10; POP RCX; HLT.
        code: '6a1059f4',
        words: [[0x4008n, 0xa003n]],
        spans: [{ physical: 0xa000n, bytes: new Uint8Array(4096) }],
      },
      async (cpu) => {
        const enter = () => cpu.setState(initial(PC, { 1: 3n, 4: sp }));
        for (let wave = 0; wave < 4; wave++) {
          enter();
          const before = calls;
          const result = await cpu.run({ stop, budget: 10n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 2n);
          assert.equal(result.state.registers[1], 16n);
          assert.equal(result.state.registers[4], sp);
          assert.deepEqual(cpu.readPhysicalMemory(0xa038n, 8), little([16n]));
          if (wave === 3) assert.equal(calls, before);
        }
        assert.ok(calls > 0);
        cpu.setDebugState({ addresses: [sp - 8n, 0n, 0n, 0n], dr6: 0n, dr7: 0x10401n });
        enter();
        const before = calls;
        const watched = await cpu.run({ stop, budget: 10n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.retired, 1n);
        assert.equal(watched.state.rip, pop);
        assert.equal(calls, before + 1);
      },
    );
  } finally {
    prototype.artifactImports = imports;
    Core.prototype.compile = compile;
  }
});
