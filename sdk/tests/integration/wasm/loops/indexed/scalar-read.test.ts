import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../../src/wasm/core.js';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import {
  PC,
  initial,
  little,
  usingPaged,
} from '../../../../../../tests/execution/paging/fixture.js';
import { compiler } from '../fixture.js';

async function withSampledLoads(
  entries: bigint[],
  run: (loads: { address: bigint; width: number }[]) => Promise<void>,
) {
  const compile = Core.prototype.compile;
  const loads: { address: bigint; width: number }[] = [];
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) return compile.call(this, pc, limits, entries, true);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  const restoreImports = observeArtifactImports((env) => {
    const load = env.load;
    if (typeof load === 'function') {
      env.load = (address: bigint, packedWidth: number) => {
        loads.push({ address, width: packedWidth & 0xff });
        return load(address, packedWidth);
      };
    }
  });
  try {
    await run(loads);
  } finally {
    restoreImports();
    Core.prototype.compile = compile;
  }
}

const sp = PC + 0x1020n;
const stackOptions = {
  words: [[0x4008n, 0xa003n]] as const,
};

test('sampled CMP and POP reuse ordinary stack reads and preserve the operand watchpoint', async () => {
  const pop = PC + 5n;
  const stop = PC + 6n;
  const stack = new Uint8Array(4096);
  const compare = 0x5566778899aabbccn;
  const popped = 0x1122334455667788n;
  stack.set(little([popped]), 0x20);
  stack.set(little([compare]), 0x30);
  await withSampledLoads([PC, pop, stop], async (loads) => {
    await usingPaged(
      compiler,
      {
        ...stackOptions,
        // CMP qword [RSP+0x10], RAX; POP RCX; HLT.
        code: '483944241059f4',
        spans: [{ physical: 0xa000n, bytes: stack }],
      },
      async (cpu) => {
        const enter = () => cpu.setState(initial(PC, { 0: compare, 1: 3n, 4: sp }));
        for (let wave = 0; wave < 4; wave++) {
          if (wave === 2) {
            assert.ok(loads.some(({ address, width }) => address === sp + 0x10n && width === 8));
            loads.length = 0;
          }
          enter();
          const full = await cpu.run({ stop, budget: 10n });
          assert.equal(full.reason, 'stop');
          assert.equal(full.retired, 2n);
          assert.equal(full.resources.nativeEntries, 1);
          assert.equal(full.state.rip, stop);
          assert.equal(full.state.flags, 0x46n);
          assert.equal(full.state.registers[1], popped);
          assert.equal(full.state.registers[4], sp + 8n);
        }
        assert.deepEqual(loads, [], 'warm CMP and POP both avoid the generic load import');

        enter();
        const interior = await cpu.run({ stop: pop, budget: 10n });
        assert.equal(interior.reason, 'stop');
        assert.equal(interior.retired, 1n);
        assert.equal(interior.state.rip, pop);
        assert.equal(interior.state.flags, 0x46n);
        assert.equal(interior.state.registers[1], 3n);
        assert.equal(interior.state.registers[4], sp);

        cpu.setDebugState({ addresses: [sp + 0x10n, 0n, 0n, 0n], dr6: 0n, dr7: 0x30401n });
        enter();
        const watched = await cpu.run({ stop, budget: 10n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.debugException?.cause, 'data-breakpoint');
        assert.equal(watched.retired, 1n);
        assert.equal(watched.state.rip, pop);
        assert.equal(watched.state.flags, 0x46n);
        assert.equal(watched.state.registers[1], 3n);
        assert.equal(watched.state.registers[4], sp);
      },
    );
  });
});

test('sampled RET keeps the result contract when the stack qword comes from a live translation', async () => {
  const target = PC + 2n;
  const stack = new Uint8Array(4096);
  stack.set(little([target]), 0x20);
  await withSampledLoads([PC, target], async (loads) => {
    await usingPaged(
      compiler,
      {
        ...stackOptions,
        code: 'c390f4',
        spans: [{ physical: 0xa000n, bytes: stack }],
      },
      async (cpu) => {
        const enter = () => cpu.setState(initial(PC, { 4: sp }));
        for (let wave = 0; wave < 4; wave++) {
          if (wave === 2) {
            assert.ok(loads.some(({ address, width }) => address === sp && width === 40));
            loads.length = 0;
          }
          enter();
          const full = await cpu.run({ stop: target, budget: 10n });
          assert.equal(full.reason, 'stop');
          assert.equal(full.retired, 1n);
          assert.equal(full.resources.nativeEntries, 1);
          assert.equal(full.state.rip, target);
          assert.equal(full.state.registers[4], sp + 8n);
        }
        assert.deepEqual(loads, [], 'warm RET avoids the separate read-result import');

        cpu.setDebugState({ addresses: [sp, 0n, 0n, 0n], dr6: 0n, dr7: 0x30401n });
        enter();
        const watched = await cpu.run({ stop: target, budget: 10n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.debugException?.cause, 'data-breakpoint');
        assert.equal(watched.retired, 1n);
        assert.equal(watched.state.rip, target);
        assert.equal(watched.state.registers[4], sp + 8n);
      },
    );
  });
});

test('a warm indexed qword read uses the checked path when its last byte crosses the page', async () => {
  const stop = PC + 3n;
  const within = PC + 0x1ff8n;
  const crossing = within + 4n;
  const first = new Uint8Array(4096);
  const second = new Uint8Array(4096);
  first.set(Uint8Array.of(1, 2, 3, 4, 5, 6, 7, 8), 0xff8);
  second.set(Uint8Array.of(9, 10, 11, 12));
  await withSampledLoads([PC, stop], async (loads) => {
    await usingPaged(
      compiler,
      {
        // CMP qword [RSI], RAX; HLT.
        code: '483906f4',
        words: [
          [0x4008n, 0xa003n],
          [0x4010n, 0xc003n],
        ],
        spans: [
          { physical: 0xa000n, bytes: first },
          { physical: 0xc000n, bytes: second },
        ],
      },
      async (cpu) => {
        for (let wave = 0; wave < 4; wave++) {
          loads.length = 0;
          cpu.setState(initial(PC, { 0: 0x0807060504030201n, 6: within }));
          const warm = await cpu.run({ stop, budget: 10n });
          assert.equal(warm.reason, 'stop');
          assert.equal(warm.retired, 1n);
          assert.equal(warm.state.flags, 0x46n);
        }
        assert.deepEqual(loads, [], 'the within-page address uses the indexed hit');

        cpu.setState(initial(PC, { 0: 0x0c0b0a0908070605n, 6: crossing }));
        const split = await cpu.run({ stop, budget: 10n });
        assert.equal(split.reason, 'stop');
        assert.equal(split.retired, 1n);
        assert.equal(split.state.flags, 0x46n);
        assert.deepEqual(loads, [{ address: crossing, width: 8 }]);
      },
    );
  });
});
