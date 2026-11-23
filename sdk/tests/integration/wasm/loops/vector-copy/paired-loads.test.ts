import assert from 'node:assert/strict';
import test from 'node:test';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import {
  BANK,
  PC,
  initial,
  usingPaged,
} from '../../../../../../tests/execution/memory/paging/fixture.js';
import { compiler } from '../fixture.js';

test('paged vector pairs retain both loads before either aliased store', async () => {
  // MOVUPS XMM0,[RDI]; MOVUPS XMM1,[RDI+16]; MOVUPS [RSI],XMM0; MOVUPS [RSI+16],XMM1.
  const code = '0f10070f104f100f11060f114e1090f4';
  const source = PC + 0x1003n;
  const target = PC + 0x2013n;
  const stop = PC + 14n;
  const sample = Uint8Array.from({ length: 32 }, (_, index) => index * 7 + 1);
  const toXmm = (bytes: Uint8Array) =>
    bytes.reduce((value, byte, index) => value | (BigInt(byte) << BigInt(index * 8)), 0n);
  const values = [toXmm(sample.subarray(0, 16)), toXmm(sample.subarray(16))];
  const data = new Uint8Array(0x3000);
  for (let wave = 0; wave < 8; wave++) data.set(sample, 3 + wave * 64);
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
    const window = env.paged_loop_copy_window;
    if (typeof window === 'function') {
      env.paged_loop_copy_window = (src: bigint, dst: bigint, width: number) => {
        assert.equal(width & 0xff, 32);
        const result = window(src, dst, width);
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
        spans: [{ physical: 0xa000n, bytes: data }],
      },
      async (cpu) => {
        const enter = (wave: number, destination = target + BigInt(wave * 64)) =>
          cpu.setState(initial(PC, { 6: destination, 7: source + BigInt(wave * 64) }));
        for (let wave = 0; wave < 4; wave++) {
          enter(wave);
          const begins = checkedBegins;
          const copied = await cpu.run({ stop, budget: 8n });
          assert.equal(copied.reason, 'stop');
          assert.equal(copied.retired, 4n);
          assert.deepEqual(cpu.readXmmState().slice(0, 2), values);
          assert.deepEqual(cpu.readPhysicalMemory(0xa013n + BigInt(wave * 64), 32), sample);
          if (wave > 0) assert.equal(checkedBegins, begins, 'warm copy stays in the native pair');
        }
        assert.ok(accepted + refused > 0, 'the cold pair requests checked admission');
        const calls = accepted + refused;

        enter(4);
        const partial = await cpu.run({ stop: PC + 7n, budget: 8n });
        assert.equal(partial.reason, 'stop');
        assert.equal(partial.retired, 2n);
        assert.deepEqual(cpu.readXmmState().slice(0, 2), values);
        assert.equal(accepted + refused, calls);

        enter(5, PC + 0x2ff0n);
        const fault = await cpu.run({ stop, budget: 8n });
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.retired, 3n);
        assert.equal(fault.state.rip, PC + 10n);
        assert.deepEqual(cpu.readXmmState().slice(0, 2), values);
        assert.deepEqual(cpu.readPhysicalMemory(0xaff0n, 16), sample.subarray(0, 16));
        assert.deepEqual(cpu.readPhysicalMemory(0xc000n, 16), new Uint8Array(16));
        assert.equal(refused + accepted, calls + 1);
      },
    );
  } finally {
    restoreImports();
  }
});

test('paired vector loads cross 4 KiB inside a single large leaf', async () => {
  const stop = PC + 14n;
  const source = 0x600ff0n;
  const target = 0x601ff0n;
  const sample = Uint8Array.from({ length: 32 }, (_, index) => index * 7 + 1);
  const bytes = new Uint8Array(0x2020);
  bytes.set(sample, 0xff0);
  let admitted = false;
  const restore = observeArtifactImports((env) => {
    const resolve = env.paged_loop_copy_window;
    if (typeof resolve === 'function') {
      env.paged_loop_copy_window = (src: bigint, dst: bigint, width: number) => {
        const result = resolve(src, dst, width);
        if (src === source && dst === target && result) admitted = true;
        return result;
      };
    }
  });
  try {
    await usingPaged(
      compiler,
      {
        // Two MOVUPS loads followed by two stores.
        code: '0f10070f104f100f11060f114e1090f4',
        words: [[0x3018n, 0x2000e7n]],
        spans: [{ physical: 0x200000n, bytes }],
      },
      async (cpu) => {
        const run = (destination: bigint) => {
          cpu.setState(initial(PC, { 6: destination, 7: source }));
          return cpu.run({ stop, budget: 8n });
        };
        await run(target);
        const copied = await run(target);
        assert.equal(copied.reason, 'stop');
        assert.equal(copied.retired, 4n);
        assert.deepEqual(cpu.readPhysicalMemory(0x201ff0n, 32), sample);
        assert.ok(admitted, 'one copy window covers both subpage boundaries');

        const fault = await run(0x602028n);
        assert.equal(fault.reason, 'physical-memory-fault');
        assert.equal(fault.retired, 2n);
        assert.equal(fault.state.rip, PC + 7n);
        assert.equal(fault.physicalMemoryFault?.failingPhysicalAddress, 0x202028n);
      },
    );
  } finally {
    restore();
  }
});
