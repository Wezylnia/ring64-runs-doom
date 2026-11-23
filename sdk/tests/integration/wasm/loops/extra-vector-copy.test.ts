import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { observeArtifactImports } from '../../../../../tests/support/artifact-imports.js';
import { PC, initial, little, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
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

  let primary = 0;
  let secondary = 0;
  let secondaryBegins = 0;
  let refusePrimary = false;
  let artifactBytes = 0;
  const restoreImports = observeArtifactImports((env) => {
    const guard = env.paged_sse_loop_guard;
    if (typeof guard === 'function') {
      env.paged_sse_loop_guard = (pc: bigint, width: number, flags: bigint, shadow: number) => {
        const result = refusePrimary && pc === PC ? 2 : guard(pc, width, flags, shadow);
        if (pc === next && result === 1) secondary++;
        return result;
      };
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
    const begin = env.debug_begin;
    if (typeof begin === 'function') {
      env.debug_begin = (pc: bigint, ...args: unknown[]) => {
        if (pc === next) secondaryBegins++;
        return begin(pc, ...args);
      };
    }
  });

  try {
    await usingPaged(compiler, setup, async (cpu) => {
      const enter = (target = pairedDestination) => cpu.setState(state(target));
      for (let wave = 0; wave < 4; wave++) {
        enter();
        const before = secondaryBegins;
        const done = await cpu.run({ budget: 80n, stop });
        assert.equal(done.reason, 'stop');
        assert.equal(done.retired, 49n);
        assert.equal(done.resources.compilations, wave === 0 ? 1 : 0);
        artifactBytes = done.resources.cachedBytes;
        assert.deepEqual(cpu.readPhysicalMemory(0xc015n, 64), plain);
        assert.deepEqual(cpu.readPhysicalMemory(0xb020n, 64), repeated);
        if (wave > 0)
          assert.equal(secondaryBegins, before, 'the warm paired placement stays native');
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
    assert.ok(artifactBytes > 0);
    for (;;) {
      const primaryBeforeCap = primary;
      const secondaryBeforeCap = secondary;
      // Exclude the previous body so each run selects the next smaller variant.
      const maxArtifactBytes = artifactBytes - 1;
      await usingPaged(
        compiler,
        { ...setup, limits: { ...setup.limits, maxArtifactBytes } },
        async (cpu) => {
          for (let wave = 0; wave < 4; wave++) {
            cpu.setState(state());
            const capped = await cpu.run({ budget: 80n, stop });
            assert.equal(capped.reason, 'stop');
            assert.equal(capped.retired, 49n);
            artifactBytes = capped.resources.cachedBytes;
            assert.ok(artifactBytes > 0 && artifactBytes <= maxArtifactBytes);
          }
          assert.deepEqual(cpu.readPhysicalMemory(0xc015n, 64), plain);
          assert.deepEqual(cpu.readPhysicalMemory(0xb020n, 64), repeated);
        },
      );
      assert.ok(primary > primaryBeforeCap, 'the smaller body retains the primary placement');
      if (secondary === secondaryBeforeCap) break;
    }
  } finally {
    restoreImports();
  }
});

test('a sampled copy loop reuses quiet SSE fetch admission and keeps checked exits', async () => {
  // CMP RAX,[RDI]; three INC R8 instructions; two ordered vector transfers per lap.
  const prefix = '483b0749ffc049ffc049ffc0';
  const copy = '0f1004070f1104060f104407100f114406104883c0204839c872e5';
  const head = PC + 12n;
  const inside = head + 8n;
  const stop = PC + 39n;
  const afterSuccessor = stop + 3n;
  const source = PC + 0x1010n;
  const seed = Uint8Array.from({ length: 16 }, (_, index) => index + 1);
  const bytes = new Uint8Array(4096);
  bytes.set(seed, 16);
  const expected = Uint8Array.from({ length: 64 }, (_, index) => seed[index % seed.length]!);
  const compile = Core.prototype.compile;
  let guards = 0;
  let windows = 0;
  let successorBegins = 0;
  let copyBegins = 0;
  let refuseWindow = false;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) {
      const points = [0, 3, 6, 9, 12, 16, 20, 25, 30, 34, 37, 39, 42].map(
        (offset) => PC + BigInt(offset),
      );
      return compile.call(this, pc, limits, points, true);
    }
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  const restoreImports = observeArtifactImports((env) => {
    const guard = env.paged_sse_loop_guard;
    if (typeof guard === 'function') {
      env.paged_sse_loop_guard = (pc: bigint, ...args: unknown[]) => {
        if (pc === head) guards++;
        return guard(pc, ...args);
      };
    }
    const window = env.paged_loop_copy_window;
    if (typeof window === 'function') {
      env.paged_loop_copy_window = (...args: unknown[]) => {
        if (refuseWindow) return 0;
        const result = window(...args);
        if (result) windows++;
        return result;
      };
    }
    const begin = env.debug_begin;
    if (typeof begin === 'function') {
      env.debug_begin = (pc: bigint, ...args: unknown[]) => {
        if (pc === stop) successorBegins++;
        if (pc === head) copyBegins++;
        return begin(pc, ...args);
      };
    }
  });

  try {
    await usingPaged(
      compiler,
      {
        code: prefix + copy + '49ffc1f4', // INC R9; HLT.
        words: [[0x4008n, 0xa063n]],
        spans: [
          { physical: 0xa000n, bytes },
          { physical: 0xb000n, bytes: bytes.slice() },
        ],
      },
      async (cpu) => {
        const enter = () =>
          cpu.setState(initial(PC, { 0: 0n, 1: 64n, 6: source + 16n, 7: source, 9: 0n }));
        for (let wave = 0; wave < 4; wave++) {
          enter();
          guards = 0;
          windows = 0;
          copyBegins = 0;
          const run = await cpu.run({ stop, budget: 32n });
          assert.equal(run.reason, 'stop');
          assert.equal(run.retired, 18n);
          assert.deepEqual(cpu.readPhysicalMemory(0xa020n, 64), expected);
          if (wave === 0) {
            assert.ok(windows > 0, 'the first copy resolves a checked data window');
          } else {
            assert.equal(windows, 0, 'the warm copy uses the cached data window');
            assert.equal(copyBegins, 0, 'the warm copy stays in the paired placement');
          }
        }
        assert.equal(guards, 0, 'the admitted copy avoids the checked SSE wrapper');

        enter();
        successorBegins = 0;
        const afterCopy = await cpu.run({ stop: afterSuccessor, budget: 32n });
        assert.equal(afterCopy.reason, 'stop');
        assert.equal(afterCopy.retired, 19n);
        assert.equal(afterCopy.state.registers[9], 1n);
        assert.equal(successorBegins, 0, 'the same-page successor receives the live fetch proof');

        cpu.writePhysicalMemory(0x4008n, little([0xb063n]));
        enter();
        copyBegins = 0;
        refuseWindow = true;
        const replayed = await cpu.run({ stop: afterSuccessor, budget: 32n });
        refuseWindow = false;
        assert.equal(replayed.reason, 'stop');
        assert.equal(replayed.retired, 19n);
        assert.equal(replayed.state.registers[9], 1n);
        assert.ok(copyBegins > 0, 'a refused operand re-enters at its checked instruction');
        assert.deepEqual(cpu.readPhysicalMemory(0xb020n, 64), expected);

        enter();
        cpu.setMxcsr(0x1f00);
        const unmasked = await cpu.run({ stop, budget: 32n });
        assert.equal(unmasked.reason, 'stop');
        assert.equal(unmasked.retired, 18n);
        assert.ok(guards > 0, 'a changed exception mask rechecks the guard');

        cpu.setMxcsr(0x1f80);
        enter();
        const stopped = await cpu.run({ stop: inside, budget: 32n });
        assert.equal(stopped.reason, 'stop');
        assert.equal(stopped.retired, 6n);
        assert.equal(stopped.state.rip, inside);

        cpu.setDebugState({ addresses: [inside, 0n, 0n, 0n], dr6: 0n, dr7: 1n });
        enter();
        const debug = await cpu.run({ stop, budget: 32n });
        assert.equal(debug.reason, 'debug-exception');
        assert.equal(debug.retired, 6n);
        assert.equal(debug.state.rip, inside);

        cpu.setDebugState({ addresses: [stop, 0n, 0n, 0n], dr6: 0n, dr7: 1n });
        enter();
        const successorDebug = await cpu.run({ stop: afterSuccessor, budget: 32n });
        assert.equal(successorDebug.reason, 'debug-exception');
        assert.equal(successorDebug.retired, 18n);
        assert.equal(successorDebug.state.rip, stop);
      },
    );
  } finally {
    Core.prototype.compile = compile;
    restoreImports();
  }
});
