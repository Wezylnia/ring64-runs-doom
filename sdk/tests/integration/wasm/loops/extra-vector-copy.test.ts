import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { PC, initial, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('one artifact keeps its original vector loop and an independently replayable paired copy', async () => {
  // The ordinary loop is longer but has one move pair; the second has a contiguous pair group.
  const ordinary = '0f1004070f1104064883c0104839c890909072ec';
  const prepare = '31c04c89c74c89ce';
  const paired = '0f1004070f1104060f104407100f114406104883c0204839c872e5';
  const code = ordinary + prepare + paired;
  const next = PC + BigInt((ordinary.length + prepare.length) / 2);
  const stop = PC + BigInt(code.length / 2);
  const ordinarySource = PC + 0x1003n;
  const ordinaryDestination = PC + 0x3015n;
  const pairedSource = PC + 0x2010n;
  const pairedDestination = pairedSource + 16n;
  const ram = new Uint8Array(0x3000);
  const plain = Uint8Array.from({ length: 64 }, (_, i) => i * 17 + 1);
  const seed = Uint8Array.from({ length: 16 }, (_, i) => i * 23 + 3);
  ram.set(plain, 3);
  ram.set(seed, 0x1010);
  const repeated = Uint8Array.from({ length: 64 }, (_, i) => seed[i % 16]!);
  const setup = {
    code,
    words: [
      [0x4008n, 0xa003n],
      [0x4010n, 0xb003n],
      [0x4018n, 0xc003n],
    ] as const,
    spans: [{ physical: 0xa000n, bytes: ram }],
    limits: { maxTranslationsPerRun: 1 },
  };
  const state = (target = pairedDestination) =>
    initial(PC, {
      0: 0n,
      1: 64n,
      6: ordinaryDestination,
      7: ordinarySource,
      8: pairedSource,
      9: target,
    });

  const proto = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = proto.artifactImports;
  let primary = 0;
  let secondary = 0;
  let refusePrimary = false;
  let artifactBytes = 0;
  proto.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const guard = env.paged_sse_loop_guard;
    if (typeof guard === 'function') {
      env.paged_sse_loop_guard = (pc: bigint, width: number, flags: bigint, shadow: number) =>
        refusePrimary && pc === PC ? 2 : guard(pc, width, flags, shadow);
    }
    const store = env.paged_loop_write_block;
    if (typeof store === 'function') {
      env.paged_loop_write_block = (address: bigint, scratch: number) => {
        const result = store(address, scratch);
        if (result && address >= ordinaryDestination && address < ordinaryDestination + 64n)
          primary++;
        return result;
      };
    }
    const window = env.paged_loop_copy_window;
    if (typeof window === 'function') {
      env.paged_loop_copy_window = (source: bigint, target: bigint, width: number) => {
        const result = window(source, target, width);
        if (result) secondary++;
        return result;
      };
    }
    return env;
  };

  try {
    await usingPaged(compiler, setup, async (cpu) => {
      const enter = (target = pairedDestination) => cpu.setState(state(target));
      for (let wave = 0; wave < 4; wave++) {
        enter();
        const done = await cpu.run({ budget: 80n, stop });
        assert.equal(done.reason, 'stop');
        assert.equal(done.retired, 49n);
        assert.equal(done.resources.compilations, wave === 0 ? 1 : 0);
        artifactBytes = done.resources.cachedBytes;
        assert.deepEqual(cpu.readPhysicalMemory(0xc015n, 64), plain);
        assert.deepEqual(cpu.readPhysicalMemory(0xb020n, 64), repeated);
      }
      assert.ok(primary > 0, 'the ordinary loop retains its guarded placement');
      assert.ok(secondary > 0, 'the same owner admits the additional paired placement');

      const before = [primary, secondary];
      refusePrimary = true;
      enter();
      const refused = await cpu.run({ budget: 80n, stop });
      assert.equal(refused.reason, 'stop');
      assert.equal(refused.retired, 49n);
      assert.equal(primary, before[0]);
      assert.ok(secondary > before[1]!);
      refusePrimary = false;

      const beforeStop = secondary;
      enter();
      const interior = await cpu.run({ budget: 80n, stop: next + 8n });
      assert.equal(interior.reason, 'stop');
      assert.equal(interior.retired, 37n);
      assert.equal(secondary, beforeStop);

      enter(PC + 0x3ff0n);
      const split = await cpu.run({ budget: 80n, stop });
      assert.equal(split.reason, 'page-fault');
      assert.equal(split.retired, 38n);
      assert.equal(split.state.rip, next + 13n);
      assert.deepEqual(cpu.readPhysicalMemory(0xcff0n, 16), seed);
    });
    const beforeCap = secondary;
    assert.ok(artifactBytes > 0);
    await usingPaged(
      compiler,
      {
        ...setup,
        limits: { ...setup.limits, maxArtifactBytes: artifactBytes - 1 },
      },
      async (cpu) => {
        cpu.setState(state());
        const capped = await cpu.run({ budget: 80n, stop });
        assert.equal(capped.reason, 'stop');
        assert.equal(capped.retired, 49n);
        assert.ok(capped.resources.cachedBytes < artifactBytes);
        assert.equal(secondary, beforeCap);
        assert.deepEqual(cpu.readPhysicalMemory(0xb020n, 64), repeated);
      },
    );
  } finally {
    proto.artifactImports = imports;
  }
});
