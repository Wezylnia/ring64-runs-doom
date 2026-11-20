import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createCpu, type MemoryMapping } from '../../../src/index.js';
import { LIMITS, fault, hex, initial } from '../../../../tests/support/fixture.js';

const compiler = await WebAssembly.compile(
  new Uint8Array(await readFile(new URL('../../../../build/ring64.wasm', import.meta.url))),
);

const head = 0x3b1000n;
const row = 0x55f0209n;
const column = 0x72d0ffdn;
const readOnly = { read: true, write: false, execute: false };

function body(address32 = false): Uint8Array {
  const operand = address32 ? '67f30f' : 'f30f';
  return hex(
    operand +
      '100488' + // MOVSS XMM0,[RAX+RCX*4]
      operand +
      '59048b' + // MULSS XMM0,[RBX+RCX*4]
      operand +
      '104c8804' + // MOVSS XMM1,[RAX+RCX*4+4]
      operand +
      '594c8b04' + // MULSS XMM1,[RBX+RCX*4+4]
      'f30f58d0f30f58d14883c1024839d175' + // ADDSS x2, ADD RCX,2, CMP RCX,RDX, JNE head
      (address32 ? 'd5' : 'd9'),
  );
}

function floats(...values: number[]): Uint8Array<ArrayBuffer> {
  const buffer = new ArrayBuffer(values.length * 4);
  const view = new DataView(buffer);
  values.forEach((value, index) => view.setFloat32(index * 4, value, true));
  return new Uint8Array(buffer);
}

function mapping(base: bigint, backing: string, length: number): MemoryMapping {
  return { backing, base, length, offset: 0, permissions: readOnly };
}

test('SIMD read loops resume from interior PCs and validate every backing after remapping', async () => {
  const code = body();
  const stop = head + BigInt(code.length);
  const fixed = [
    {
      ...mapping(head, 'code', code.length),
      permissions: { ...readOnly, execute: true },
    },
    mapping(row, 'row', 16),
  ];
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'row', bytes: floats(1, 2, 3, 4) },
        { id: 'column', bytes: floats(2, 3, 4, 5) },
        { id: 'tail', bytes: Uint8Array.of(0x41) },
      ],
      mappings: [...fixed, mapping(column, 'column', 16)],
    },
    memoryLimits: { maxBackings: 4, maxMappings: 4, maxBackingBytes: code.length + 33 },
  });
  const input = initial(head, { 0: row, 1: 0n, 2: 4n, 3: column });
  const upper = 0x123456789012345678901234n << 32n;
  const reset = () => {
    cpu.setState(input);
    cpu.setXmmState(Array<bigint>(16).fill(upper));
  };
  try {
    reset();
    const first = await cpu.run({ budget: 20n, stop });
    assert.equal(first.reason, 'stop');
    assert.equal(first.retired, 18n);
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [
      0x41400000n,
      0x41a00000n,
      upper | 0x42200000n,
    ]);

    reset();
    const interior = await cpu.run({ budget: 20n, stop: head + 10n });
    assert.equal(interior.reason, 'stop');
    assert.equal(interior.retired, 2n);
    assert.equal(interior.state.rip, head + 10n);
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [0x40000000n, upper, upper]);
    const resumed = await cpu.run({ budget: 20n, stop });
    assert.deepEqual(resumed.state, first.state);
    assert.equal(resumed.retired, 16n);
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [
      0x41400000n,
      0x41a00000n,
      upper | 0x42200000n,
    ]);

    const split = [...fixed, mapping(column, 'column', 15)];
    const tail = mapping(column + 15n, 'tail', 1);
    cpu.setMappings([...split, tail]);
    reset();
    const remapped = await cpu.run({ budget: 20n, stop });
    assert.equal(remapped.reason, 'stop');
    assert.equal(remapped.retired, 18n);
    // The upper byte of the final operand changed 5.0 to 20.0.
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [
      0x41400000n,
      0x42a00000n,
      upper | 0x42c80000n,
    ]);

    cpu.setMappings([
      ...split,
      { ...tail, permissions: { read: false, write: true, execute: false } },
    ]);
    reset();
    const denied = await cpu.run({ budget: 20n, stop });
    assert.equal(denied.reason, 'data-fault');
    assert.equal(denied.retired, 12n);
    assert.equal(denied.state.rip, head + 16n);
    assert.equal(denied.state.registers[1], 2n);
    fault(denied, {
      instructionPc: head + 16n,
      address: column + 12n,
      width: 4,
      access: 'read',
      cause: 'permission',
      failingAddress: column + 15n,
      segment: 'ds',
    });
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [
      0x41400000n,
      0x40800000n,
      upper | 0x41000000n,
    ]);
    assert.equal(cpu.readMxcsr(), 0x1f80);
  } finally {
    cpu.dispose();
  }
});

test('SIMD read loops wrap address32 independently of the full counter and backing', async () => {
  const code = body(true);
  const row = 0xfffffff8n;
  const stop = head + BigInt(code.length);
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'row', bytes: floats(1, 2, 100, 101) },
        { id: 'wrapped-row', bytes: floats(3, 4) },
        { id: 'column', bytes: floats(2, 3, 4, 5) },
      ],
      mappings: [
        {
          ...mapping(head, 'code', code.length),
          permissions: { ...readOnly, execute: true },
        },
        mapping(row, 'row', 16),
        mapping(0n, 'wrapped-row', 8),
        mapping(column, 'column', 16),
      ],
    },
    memoryLimits: { maxBackings: 4, maxMappings: 4, maxBackingBytes: code.length + 40 },
  });
  try {
    cpu.setState(initial(head, { 0: row, 1: 0n, 2: 4n, 3: column }));
    const result = await cpu.run({ budget: 20n, stop });
    assert.equal(result.reason, 'stop');
    assert.equal(result.retired, 18n);
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [0x41400000n, 0x41a00000n, 0x42200000n]);
  } finally {
    cpu.dispose();
  }
});
