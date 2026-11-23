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
    const resolve = env.paged_loop_copy_window;
    if (typeof resolve === 'function') {
      env.paged_loop_copy_window = (src: bigint, dst: bigint, width: number) => {
        const result = resolve(src, dst, width);
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
          const begins = checkedBegins;
          const run = await cpu.run({ budget: 100n, stop });
          assert.equal(run.reason, 'stop');
          assert.equal(run.retired, 21n);
          assert.deepEqual(cpu.readPhysicalMemory(0xa013n, 96), expected);
          if (wave > 0) assert.equal(checkedBegins, begins, 'warm loop stays in the native pair');
        }
        assert.ok(accepted + refused > 0, 'the cold loop requests checked admission');
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
    restoreImports();
  }
});

for (const [shape, code, length, registers] of [
  [
    'direct',
    '0f1004300f11000f104430100f1140104883c0204839c872e7',
    7,
    (source: bigint, destination: bigint) => ({
      0: destination,
      6: BigInt.asUintN(64, source - destination),
    }),
  ],
  [
    'adjusted',
    '0f100407488d34024883c6200f1146e00f104407100f1146f04883c0204839ce72de',
    9,
    (source: bigint, destination: bigint) => ({ 0: 0n, 2: destination, 7: source }),
  ],
] as const) {
  test(`counted ${shape} vector windows publish exact overlap, exit flags and partial budgets`, async () => {
    const source = PC + 0x1003n;
    const destination = source + 16n;
    const stop = PC + BigInt(code.length / 2);
    const seed = Uint8Array.from({ length: 16 }, (_, index) => index * 23 + 1);
    const repeated = (size: number) =>
      Uint8Array.from({ length: size }, (_, index) => seed[index % seed.length]!);
    const ram = new Uint8Array(0x1000);
    ram.set(seed, 3);

    await usingPaged(
      compiler,
      {
        code,
        words: [[0x4008n, 0xa003n]],
        spans: [{ physical: 0xa000n, bytes: ram }],
      },
      async (cpu) => {
        const enter = () =>
          cpu.setState(initial(PC, { ...registers(source, destination), 1: destination + 65n }));
        for (let wave = 0; wave < 4; wave++) {
          enter();
          const full = await cpu.run({ budget: 100n, stop });
          assert.equal(full.reason, 'stop');
          assert.equal(full.retired, BigInt(length * 3));
          assert.equal(full.state.flags & 0x8d5n, 0x10n);
          assert.deepEqual(cpu.readPhysicalMemory(0xa013n, 96), repeated(96));
        }

        cpu.writePhysicalMemory(0xa013n, new Uint8Array(96));
        enter();
        const inside = await cpu.run({ budget: 100n, stop: PC + 4n });
        assert.equal(inside.reason, 'stop');
        assert.equal(inside.retired, 1n);
        assert.equal(inside.state.rip, PC + 4n);
        assert.deepEqual(cpu.readPhysicalMemory(0xa013n, 96), new Uint8Array(96));

        enter();
        const exact = await cpu.run({ budget: BigInt(length * 2), stop });
        assert.equal(exact.reason, 'budget');
        assert.equal(exact.retired, BigInt(length * 2));
        assert.equal(exact.state.rip, PC);
        assert.equal(exact.state.flags & 0x8d5n, 0x95n);
        assert.deepEqual(cpu.readPhysicalMemory(0xa013n, 64), repeated(64));
        assert.deepEqual(cpu.readPhysicalMemory(0xa053n, 32), new Uint8Array(32));

        cpu.writePhysicalMemory(0xa013n, new Uint8Array(96));
        enter();
        const partial = await cpu.run({ budget: BigInt(length * 2 + 1), stop });
        assert.equal(partial.reason, 'budget');
        assert.equal(partial.retired, BigInt(length * 2 + 1));
        assert.equal(partial.state.rip, PC + 4n);
        assert.equal(partial.state.flags & 0x8d5n, 0x95n);
        assert.deepEqual(cpu.readPhysicalMemory(0xa013n, 64), repeated(64));
        assert.deepEqual(cpu.readPhysicalMemory(0xa053n, 32), new Uint8Array(32));
      },
    );
  });
}
