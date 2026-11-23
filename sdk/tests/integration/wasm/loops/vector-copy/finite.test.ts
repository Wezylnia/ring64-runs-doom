import assert from 'node:assert/strict';
import test from 'node:test';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import { BANK, PC, initial, usingPaged } from '../../../../../../tests/execution/paging/fixture.js';
import { COPY_FILL_INDEX, compiler } from '../fixture.js';

test('a single vector pair reserves its successor and replays lost admissions', async () => {
  const code = '0f10070f11069090'; // MOVUPS XMM0,[RDI]; MOVUPS [RSI],XMM0; NOP; NOP
  const stop = PC + 8n;
  const source = PC + 0x1003n;
  const destination = PC + 0x1100n;
  const seed = Uint8Array.from({ length: 16 }, (_, index) => 7 * index + 1);
  const ram = new Uint8Array(4096);
  ram.set(seed, 3);
  let accepted = 0;
  let calls = 0;
  const restoreImports = observeArtifactImports((env) => {
    const copy = env.paged_loop_copy_window;
    if (typeof copy === 'function') {
      env.paged_loop_copy_window = (src: bigint, dst: bigint, width: number) => {
        assert.equal(width & ~COPY_FILL_INDEX, 16);
        const result = copy(src, dst, width);
        calls++;
        if (result) accepted++;
        return result;
      };
    }
  });
  try {
    await usingPaged(
      compiler,
      {
        code,
        words: [[0x4008n, 0xa007n]],
        spans: [{ physical: 0xa000n, bytes: ram }],
      },
      async (cpu) => {
        const enter = (target = destination) => cpu.setState(initial(PC, { 6: target, 7: source }));
        for (let wave = 0; wave < 4; wave++) {
          enter();
          const copied = await cpu.run({ budget: 10n, stop });
          assert.equal(copied.reason, 'stop');
          assert.equal(copied.retired, 4n);
          assert.deepEqual(cpu.readPhysicalMemory(0xa100n, 16), seed);
        }
        assert.ok(accepted > 0);
        const charged = calls;
        enter();
        const successor = await cpu.run({ budget: 10n, stop: PC + 6n });
        assert.equal(successor.reason, 'stop');
        assert.equal(successor.retired, 2n);
        assert.equal(calls, charged);
        enter();
        const budget = await cpu.run({ budget: 2n, stop });
        assert.equal(budget.reason, 'budget');
        assert.equal(budget.state.rip, PC + 6n);
        assert.equal(calls, charged);

        enter(PC + 0x1ff8n);
        const split = await cpu.run({ budget: 10n, stop });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.retired, 1n);
        assert.equal(split.state.rip, PC + 3n);
        assert.deepEqual(cpu.readPhysicalMemory(0xaff8n, 8), new Uint8Array(8));
        assert.equal(calls, charged + 1);

        cpu.setDebugState({ addresses: [PC + 3n, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
        enter();
        const debug = await cpu.run({ budget: 10n, stop });
        assert.equal(debug.reason, 'debug-exception');
        assert.equal(debug.retired, 1n);
        assert.equal(debug.state.rip, PC + 3n);
        assert.equal(calls, charged + 1);
      },
    );
  } finally {
    restoreImports();
  }
});

test('straight paged vector copies keep alias ordering and replay precise boundaries', async () => {
  const code = '0f10070f11060f1047100f114610';
  const stop = PC + BigInt(code.length / 2);
  const source = PC + 0x1003n;
  // A distinct linear page aliases the source physical page.
  const target = PC + 0x2013n;
  const seed = Uint8Array.from({ length: 16 }, (_, index) => index * 11 + 4);
  const ram = new Uint8Array(0x3000);
  for (let wave = 0; wave < 8; wave++) ram.set(seed, 3 + wave * 64);

  let accepted = 0;
  let refused = 0;
  let checkedBegins = 0;
  const restoreImports = observeArtifactImports((env) => {
    const begin = env.debug_begin;
    if (typeof begin === 'function') {
      env.debug_begin = (pc: bigint, ...args: unknown[]) => {
        if (pc === PC) checkedBegins++;
        return begin(pc, ...args);
      };
    }
    const copy = env.paged_loop_copy_window;
    if (typeof copy === 'function') {
      env.paged_loop_copy_window = (src: bigint, dst: bigint, width: number) => {
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
          [0x4010n, 0xa003n],
          [0x4018n, 0xc001n],
        ],
        spans: [{ physical: 0xa000n, bytes: ram }],
      },
      async (cpu) => {
        const enter = (wave: number, destination = target + BigInt(wave * 64), flags = 2n) =>
          cpu.setState(initial(PC, { 6: destination, 7: source + BigInt(wave * 64) }, flags));
        for (let wave = 0; wave < 4; wave++) {
          enter(wave);
          const begins = checkedBegins;
          const run = await cpu.run({ budget: 10n, stop });
          assert.equal(run.reason, 'stop');
          assert.equal(run.retired, 4n);
          assert.deepEqual(cpu.readPhysicalMemory(0xa013n + BigInt(wave * 64), 16), seed);
          assert.deepEqual(cpu.readPhysicalMemory(0xa023n + BigInt(wave * 64), 16), seed);
          if (wave > 0) assert.equal(checkedBegins, begins, 'warm copy stays in the native pair');
        }
        assert.ok(
          accepted + refused > 0,
          'the cold copy requests checked admission for the paired placement',
        );
        const calls = accepted + refused;

        enter(4);
        const interior = await cpu.run({ budget: 10n, stop: PC + 6n });
        assert.equal(interior.reason, 'stop');
        assert.equal(interior.retired, 2n);
        assert.deepEqual(cpu.readPhysicalMemory(0xa123n, 16), new Uint8Array(16));

        enter(5);
        const budget = await cpu.run({ budget: 3n, stop });
        assert.equal(budget.reason, 'budget');
        assert.equal(budget.retired, 3n);
        assert.equal(budget.state.rip, PC + 10n);
        assert.deepEqual(cpu.readPhysicalMemory(0xa163n, 16), new Uint8Array(16));

        enter(6, undefined, 0x102n);
        const step = await cpu.run({ budget: 10n, stop });
        assert.equal(step.reason, 'debug-exception');
        assert.equal(step.retired, 1n);
        assert.equal(step.state.rip, PC + 3n);
        assert.equal(accepted + refused, calls);

        const refusedBefore = refused;
        enter(7, PC + 0x2ff0n);
        const split = await cpu.run({ budget: 10n, stop });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.retired, 3n);
        assert.equal(split.state.rip, PC + 10n);
        assert.deepEqual(cpu.readPhysicalMemory(0xaff0n, 16), seed);
        assert.deepEqual(cpu.readPhysicalMemory(0xc000n, 16), new Uint8Array(16));
        assert.equal(refused, refusedBefore + 1);

        const beforeSse = accepted + refused;
        const bank = cpu.readPagingState();
        cpu.setPagingState({ ...bank, cr0: bank.cr0 | 8n });
        enter(0);
        const sse = await cpu.run({ budget: 10n, stop });
        assert.equal(sse.reason, 'device-not-available');
        assert.equal(sse.retired, 0n);
        assert.equal(sse.state.rip, PC);
        assert.equal(accepted + refused, beforeSse);
      },
    );
  } finally {
    restoreImports();
  }
});
