import assert from 'node:assert/strict';
import test from 'node:test';
import { observeArtifactImports } from '../../../../../tests/support/artifact-imports.js';
import { BANK, PC, initial, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { COPY_FILL_INDEX, compiler } from './fixture.js';

test('retained split vector copies preserve physical aliases and denied later writes', async () => {
  const code = '0f10070f11060f1047100f114610';
  const stop = PC + BigInt(code.length / 2);
  const source = PC + 0x1ff0n;
  const target = PC + 0x3ff8n;
  const ram = new Uint8Array(0x4000);
  ram.set(
    Uint8Array.from({ length: 48 }, (_, index) => 3 + 17 * index),
    0xff0,
  );
  let accepted = 0;
  let refused = 0;
  const restoreImports = observeArtifactImports((env) => {
    const copy = env.paged_loop_copy_window;
    if (typeof copy === 'function') {
      env.paged_loop_copy_window = (src: bigint, dst: bigint, width: number) => {
        assert.equal(src, source);
        assert.equal(width & ~COPY_FILL_INDEX, 32);
        const result = copy(src, dst, width);
        if (result) accepted++;
        else refused++;
        return result;
      };
    }
  });
  try {
    await usingPaged(
      compiler,
      {
        code,
        bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
        words: [
          [0x4008n, 0xa003n],
          [0x4010n, 0xb003n],
          [0x4018n, 0xa003n],
          [0x4020n, 0xb003n],
          [0x4028n, 0xc003n],
          [0x4030n, 0xd001n],
        ],
        spans: [{ physical: 0xa000n, bytes: ram }],
      },
      async (cpu) => {
        const enter = (dst = target) => cpu.setState(initial(PC, { 6: dst, 7: source }));
        for (let wave = 0; wave < 4; wave++) {
          const expected = cpu.readPhysicalMemory(0xaff0n, 48);
          expected.set(expected.slice(0, 16), 8);
          expected.set(expected.slice(16, 32), 24);
          enter();
          const completion = await cpu.run({ budget: 20n, stop });
          assert.equal(completion.reason, 'stop');
          assert.equal(completion.retired, 4n);
          assert.deepEqual(cpu.readPhysicalMemory(0xaff0n, 48), expected);
        }
        assert.ok(accepted > 0, 'both operand ranges used the split lease');
        const before = accepted + refused;
        enter();
        const budget = await cpu.run({ budget: 3n, stop });
        assert.equal(budget.retired, 3n);
        assert.equal(budget.state.rip, PC + 10n);
        assert.equal(accepted + refused, before);

        const first = cpu.readPhysicalMemory(0xaff0n, 16);
        enter(PC + 0x5ff0n);
        const splitFault = await cpu.run({ budget: 20n, stop });
        assert.equal(splitFault.reason, 'page-fault');
        assert.equal(splitFault.retired, 3n);
        assert.equal(splitFault.state.rip, PC + 10n);
        assert.deepEqual(cpu.readPhysicalMemory(0xcff0n, 16), first);
        assert.deepEqual(cpu.readPhysicalMemory(0xd000n, 16), new Uint8Array(16));
      },
    );
  } finally {
    restoreImports();
  }
});
