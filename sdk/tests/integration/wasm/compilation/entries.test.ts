import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { registerBank } from '../../../../../tests/support/fixture.js';
import { createCpu } from '../../../../src/index.js';
import type { State } from '../../../../src/index.js';

const compiler = await WebAssembly.compile(
  new Uint8Array(await readFile(new URL('../../../../../build/ring64.wasm', import.meta.url))),
);
const CODE = 0x1000n;
const RAM = 0x8000n;
const limits = {
  maxInstructions: 32,
  maxSourceBytes: 256,
  maxArtifactBytes: 65536,
  maxCachedArtifacts: 8,
  maxCachedBytes: 1048576,
  maxTranslationsPerRun: 8,
};

function state(rip: bigint): State {
  return {
    registers: registerBank((index) => 0x1122334400000000n + BigInt(index)),
    flags: 0x8d7n,
    rip,
    interruptibility: { sti: false, ss: false },
    event: { kind: 'ready' },
    interrupts: { irq: null, nmiPending: false, nmiBlocked: false },
  };
}

test('the first requested entry can follow a lower discovered entry in the same artifact', async () => {
  const cpu = await createCpu({
    compiler,
    code: { base: CODE, bytes: Uint8Array.of(0xb8, 0x11, 0, 0, 0, 0xeb, 0xf9) },
    ram: { base: RAM, bytes: new Uint8Array(8) },
    limits,
  });
  try {
    const input = state(CODE + 5n);
    cpu.setState(input);
    const first = await cpu.run({ budget: 1n });
    assert.equal(first.reason, 'budget');
    assert.equal(first.retired, 1n);
    assert.equal(first.remaining, 0n);
    assert.equal(first.resources.compilations, 1);
    assert.equal(first.resources.nativeEntries, 1);
    assert.deepEqual(first.state, { ...input, rip: CODE });
    assert.deepEqual(cpu.inspectTranslations()[0]!.entries, [CODE, CODE + 5n]);

    const resumed = await cpu.run({ budget: 1n });
    const expected = state(CODE + 5n);
    expected.registers[0] = 0x11n;
    assert.equal(resumed.reason, 'budget');
    assert.equal(resumed.retired, 1n);
    assert.equal(resumed.remaining, 0n);
    assert.equal(resumed.resources.compilations, 0);
    assert.deepEqual(resumed.state, expected);
    assert.deepEqual(cpu.readMemory(RAM, 8), new Uint8Array(8));
  } finally {
    cpu.dispose();
  }
});

test('a cached middle entry imports fresh state and executes only its resumed suffix', async () => {
  const cpu = await createCpu({
    compiler,
    code: {
      base: CODE,
      bytes: Uint8Array.of(0xb8, 1, 0, 0, 0, 0xb9, 2, 0, 0, 0, 0xba, 3, 0, 0, 0),
    },
    ram: { base: RAM, bytes: new Uint8Array(8) },
    limits,
  });
  try {
    cpu.setState(state(CODE));
    const first = await cpu.run({ budget: 1n });
    assert.equal(first.resources.compilations, 1);
    assert.deepEqual(cpu.inspectTranslations()[0]!.entries, [CODE, CODE + 5n, CODE + 10n]);

    const input = state(CODE + 5n);
    cpu.setState(input);
    const middle = await cpu.run({ budget: 1n });
    const expected = state(CODE + 10n);
    expected.registers[1] = 2n;
    assert.equal(middle.reason, 'budget');
    assert.equal(middle.retired, 1n);
    assert.equal(middle.remaining, 0n);
    assert.equal(middle.resources.compilations, 0);
    assert.equal(middle.resources.cacheHits, 1);
    assert.deepEqual(middle.state, expected);

    const suffix = await cpu.run({ budget: 1n });
    expected.registers[2] = 3n;
    expected.rip = CODE + 15n;
    assert.equal(suffix.reason, 'budget');
    assert.equal(suffix.retired, 1n);
    assert.equal(suffix.remaining, 0n);
    assert.equal(suffix.resources.compilations, 0);
    assert.deepEqual(suffix.state, expected);
    assert.deepEqual(cpu.readMemory(RAM, 8), new Uint8Array(8));
  } finally {
    cpu.dispose();
  }
});

test('a scalar FP loop can use a smaller translation when its specialized body exceeds the cache', async () => {
  // ADDSS xmm0,xmm1; SUB ecx,1; JNE back to ADDSS.
  const code = Uint8Array.of(0xf3, 0x0f, 0x58, 0xc1, 0x83, 0xe9, 1, 0x75, 0xf7);
  async function execute(maxCachedBytes: number) {
    const cpu = await createCpu({
      compiler,
      code: { base: CODE, bytes: code },
      ram: { base: RAM, bytes: new Uint8Array(8) },
      limits: { ...limits, maxCachedBytes },
    });
    try {
      const input = state(CODE);
      input.registers[1] = 3n;
      cpu.setState(input);
      cpu.setXmmState([0x3f800000n, 0x3f800000n, ...Array<bigint>(14).fill(0n)]);
      const result = await cpu.run({ budget: 10n, stop: CODE + BigInt(code.length) });
      assert.equal(result.reason, 'stop');
      assert.equal(result.retired, 9n);
      assert.equal(cpu.readXmmState()[0], 0x40800000n);
      assert.ok(result.resources.cachedBytes <= maxCachedBytes);
      return { bytes: result.resources.generatedBytes, state: result.state };
    } finally {
      cpu.dispose();
    }
  }
  const specialized = await execute(limits.maxCachedBytes);
  const fallback = await execute(specialized.bytes - 1);
  assert.deepEqual(fallback.state, specialized.state);
  assert.ok(fallback.bytes < specialized.bytes);
});
