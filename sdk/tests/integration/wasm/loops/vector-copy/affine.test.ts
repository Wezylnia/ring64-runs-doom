import assert from 'node:assert/strict';
import test from 'node:test';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import {
  BANK,
  PC,
  initial,
  usingPaged,
} from '../../../../../../tests/execution/memory/paging/fixture.js';
import { COPY_FILL_INDEX, compiler } from '../fixture.js';

test('copy windows preserve interleaved destination arithmetic and alias order', async () => {
  // Two 16-byte copies, with LEA/ADD computing both destination addresses after the first load.
  const code = '0f100407488d14064883c2200f1142e00f104407100f1142f074e5';
  const stop = PC + BigInt(code.length / 2);
  const source = PC + 0x1003n;
  const target = PC + 0x2013n;
  const seed = Uint8Array.from({ length: 16 }, (_, i) => 7 + 11 * i);
  const ram = new Uint8Array(0x3000);
  for (let wave = 0; wave < 6; wave++) ram.set(seed, 3 + 64 * wave);

  let accepted = 0;
  let declined = 0;
  let checkedBegins = 0;
  const restoreImports = observeArtifactImports((env) => {
    const begin = env.debug_begin;
    if (typeof begin === 'function') {
      env.debug_begin = (pc: bigint, ...args: unknown[]) => {
        if (pc === PC) checkedBegins++;
        return begin(pc, ...args);
      };
    }
    const window = env.paged_loop_copy_window;
    if (typeof window === 'function') {
      env.paged_loop_copy_window = (src: bigint, dst: bigint, width: number) => {
        assert.equal(width & ~COPY_FILL_INDEX, 32);
        const result = window(src, dst, width);
        if (result) accepted++;
        else declined++;
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
          [0x4010n, 0xa003n],
          [0x4018n, 0xc001n],
        ],
        spans: [{ physical: 0xa000n, bytes: ram }],
      },
      async (cpu) => {
        const enter = (wave: number, dst = target + BigInt(64 * wave)) =>
          cpu.setState(
            initial(PC, { 0: 0n, 2: 0xdeadn, 6: dst, 7: source + BigInt(64 * wave) }, 0x803n),
          );
        for (let wave = 0; wave < 4; wave++) {
          enter(wave);
          const begins = checkedBegins;
          const completed = await cpu.run({ budget: 30n, stop });
          const adjusted = target + BigInt(32 + 64 * wave);
          assert.equal(completed.reason, 'stop');
          assert.equal(completed.retired, 7n);
          assert.equal(completed.state.registers[2], adjusted);
          assert.equal(completed.state.flags & 0x8d5n, wave === 0 || wave === 3 ? 4n : 0n);
          assert.deepEqual(cpu.readPhysicalMemory(0xa013n + BigInt(64 * wave), 16), seed);
          assert.deepEqual(cpu.readPhysicalMemory(0xa023n + BigInt(64 * wave), 16), seed);
          if (wave > 0) assert.equal(checkedBegins, begins, 'warm copy stays in the native pair');
        }
        assert.ok(accepted + declined > 0, 'the cold affine copy requests checked admission');
        const calls = accepted + declined;

        enter(4);
        const partial = await cpu.run({ budget: 3n, stop });
        assert.equal(partial.reason, 'budget');
        assert.equal(partial.state.rip, PC + 12n);
        assert.equal(partial.state.registers[2], target + 288n);
        assert.deepEqual(cpu.readPhysicalMemory(0xa113n, 16), new Uint8Array(16));
        assert.equal(accepted + declined, calls);

        enter(5, PC + 0x2ff0n);
        const split = await cpu.run({ budget: 30n, stop });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.retired, 5n);
        assert.equal(split.state.rip, PC + 21n);
        assert.equal(split.state.registers[2], PC + 0x3010n);
        assert.deepEqual(cpu.readPhysicalMemory(0xaff0n, 16), seed);
        assert.deepEqual(cpu.readPhysicalMemory(0xc000n, 16), new Uint8Array(16));
      },
    );
  } finally {
    restoreImports();
  }
});
