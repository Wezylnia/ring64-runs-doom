import assert from 'node:assert/strict';
import test from 'node:test';
import { createCpu } from '../../../../src/index.js';
import { Core } from '../../../../src/wasm/core.js';
import { snapshot } from '../../../../../tests/execution/far-control/fixture.js';
import {
  BANK,
  PC,
  hex,
  initial,
  usingPaged,
} from '../../../../../tests/execution/paging/fixture.js';
import { LIMITS } from '../../../../../tests/support/fixture.js';
import { compiler } from './fixture.js';

const seed = Uint8Array.from({ length: 8 }, (_, index) => 19 * index + 3);
const expected = Uint8Array.from({ length: 18 }, (_, index) => seed[index % 8]!);
const source = PC + 0x1003n;
const target = PC + 0x200bn;
const value = new DataView(seed.buffer).getBigUint64(0, true);

const proto = Core.prototype as unknown as {
  artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
};

test('paged scalar copies preserve overlapping memory and a later write fault', async () => {
  const code = '488b07488906488b4708488946080fb7471066894610';
  const stop = PC + BigInt(code.length / 2);
  const ram = new Uint8Array(0x3000);
  for (let wave = 0; wave < 5; wave++) ram.set(seed, 3 + wave * 64);
  const imports = proto.artifactImports;
  let accepted = 0;
  proto.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const copy = env.paged_loop_copy_window;
    if (typeof copy === 'function') {
      env.paged_loop_copy_window = (src: bigint, dst: bigint, width: number) => {
        const result = copy(src, dst, width);
        if (width < 0 && result) accepted++;
        return result;
      };
    }
    return env;
  };
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
        const enter = (wave: number, dst = target + BigInt(wave * 64)) =>
          cpu.setState(
            initial(PC, { 0: 0xffffffffffffffffn, 6: dst, 7: source + BigInt(wave * 64) }),
          );
        for (let wave = 0; wave < 3; wave++) {
          enter(wave);
          const result = await cpu.run({ budget: 20n, stop });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 6n);
          assert.equal(result.state.registers[0], value & 0xffffn);
          assert.deepEqual(cpu.readPhysicalMemory(0xa00bn + BigInt(wave * 64), 18), expected);
        }
        assert.ok(accepted > 0);
        enter(3);
        const budget = await cpu.run({ budget: 3n, stop });
        assert.equal(budget.retired, 3n);
        assert.equal(budget.reason, 'budget');
        assert.equal(budget.state.rip, PC + 10n);
        assert.deepEqual(cpu.readPhysicalMemory(0xa0d3n, 8), new Uint8Array(8));
        enter(4, PC + 0x2ff0n);
        const fault = await cpu.run({ budget: 20n, stop });
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.retired, 5n);
        assert.equal(fault.state.rip, PC + 18n);
        assert.equal(fault.state.registers[0], 0n);
        assert.deepEqual(cpu.readPhysicalMemory(0xaff0n, 8), seed);
        assert.deepEqual(cpu.readPhysicalMemory(0xc000n, 2), new Uint8Array(2));
      },
    );
    const beforeAlignment = accepted;
    await usingPaged(
      compiler,
      {
        code,
        context: snapshot('long64', 3),
        bank: { ...BANK, cr0: BANK.cr0 | 0x40000n },
        words: [
          [0x4008n, 0xa007n],
          [0x4010n, 0xb007n],
        ],
        spans: [{ physical: 0xa000n, bytes: ram }],
      },
      async (cpu) => {
        cpu.setState(initial(PC, { 6: target, 7: source }, 0x40002n));
        const alignment = await cpu.run({ budget: 20n, stop });
        assert.equal(alignment.reason, 'alignment-check');
        assert.equal(alignment.retired, 0n);
        assert.equal(alignment.state.rip, PC);
      },
    );
    assert.equal(accepted, beforeAlignment);
  } finally {
    proto.artifactImports = imports;
  }
});

test('flat scalar copy groups preserve MOVSD/MOVQ upper lanes and partial retirement', async () => {
  const body = 'f20f1007f20f1106f30f7e4708660fd646080fb7471066894610';
  const code = hex(body + '85c074e2');
  const stop = PC + BigInt(code.length);
  const data = new Uint8Array(4096);
  data.set(seed, 3);
  data.set(seed, 67);
  const permissions = { read: true, write: true, execute: false };
  const cpu = await createCpu({
    compiler,
    context: snapshot('protected'),
    entry: PC,
    limits: LIMITS,
    memoryLimits: { maxBackings: 2, maxMappings: 3, maxBackingBytes: 8192 },
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'data', bytes: data },
      ],
      mappings: [
        {
          base: PC,
          length: code.length,
          backing: 'code',
          offset: 0,
          permissions: { ...permissions, write: false, execute: true },
        },
        { base: PC + 0x1000n, length: 4096, backing: 'data', offset: 0, permissions },
        { base: PC + 0x2000n, length: 4096, backing: 'data', offset: 0, permissions },
      ],
    },
  });
  const imports = proto.artifactImports;
  let accepted = 0;
  proto.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const write = env.write_span;
    if (typeof write === 'function') {
      env.write_span = (addr: bigint, width: number, segment: number) => {
        const result = write(addr, width, segment);
        if (width === 18 && result) accepted++;
        return result;
      };
    }
    return env;
  };
  try {
    cpu.setXmmState(Array(16).fill((1n << 128n) - 1n));
    cpu.setState(initial(PC, { 0: 0xffffffffn, 6: target, 7: source }));
    const run = await cpu.run({ budget: 20n, stop });
    assert.equal(run.reason, 'stop');
    assert.equal(run.retired, 8n);
    assert.equal(run.state.registers[0], value & 0xffffn);
    assert.equal(cpu.readXmmState()[0], value);
    assert.deepEqual(cpu.readMemory(target, 18), expected);
    assert.equal(accepted, 1);
    cpu.setState(initial(PC, { 0: 0xffffffffn, 6: target + 64n, 7: source + 64n }));
    const partial = await cpu.run({ budget: 3n, stop });
    assert.equal(partial.reason, 'budget');
    assert.equal(partial.retired, 3n);
    assert.deepEqual(cpu.readMemory(target + 72n, 8), new Uint8Array(8));
  } finally {
    proto.artifactImports = imports;
    cpu.dispose();
  }
});
