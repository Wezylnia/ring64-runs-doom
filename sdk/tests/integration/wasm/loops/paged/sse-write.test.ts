import assert from 'node:assert/strict';
import test from 'node:test';
import { createCpu } from '../../../../../src/index.js';
import { protectedSnapshot } from '../../../../../../tests/support/context.js';
import { LIMITS, fault, hex, initial } from '../../../../../../tests/support/fixture.js';
import { compiler, head, row, column, readOnly, mapping } from '../fixture.js';

test('a packed SSE loop preserves an interior stop, a partial lap, and a later read fault', async () => {
  // MOVUPS; CMPNLEPS; MOVMSKPS; TEST; LEA; JNZ head; MOV store; MOV load; JMP head.
  const code = hex('0f100e0fc2c8060f50c185c08d761075ef890f8b13ebe9');
  const data = new Uint8Array(32);
  const values = new DataView(data.buffer);
  for (let offset = 16; offset < data.length; offset += 4) values.setFloat32(offset, 1, true);

  for (const context of [protectedSnapshot(), undefined]) {
    const cpu = await createCpu({
      compiler,
      entry: head,
      ...(context ? { context } : {}),
      limits: LIMITS,
      memory: {
        backings: [
          { id: 'code', bytes: code },
          { id: 'source', bytes: data },
          { id: 'output', bytes: new Uint8Array(4) },
        ],
        mappings: [
          { ...mapping(head, 'code', code.length), permissions: { ...readOnly, execute: true } },
          mapping(row, 'source', data.length),
          { ...mapping(column, 'output', 4), permissions: { ...readOnly, write: true } },
        ],
      },
      memoryLimits: {
        maxBackings: 3,
        maxMappings: 3,
        maxBackingBytes: code.length + data.length + 4,
      },
    });
    try {
      cpu.setState(initial(head, { 1: 0x1234n, 3: column, 6: row, 7: column }));
      const stop = await cpu.run({ budget: 50n, stop: head + 17n });
      assert.equal(stop.reason, 'stop');
      assert.equal(stop.retired, 6n);
      assert.deepEqual(cpu.readMemory(column, 4), new Uint8Array(4));

      const partial = await cpu.run({ budget: 4n });
      assert.equal(partial.reason, 'budget');
      assert.equal(partial.retired, 4n);
      assert.equal(partial.state.rip, head + 3n);
      assert.deepEqual(cpu.readMemory(column, 4), hex('34120000'));

      cpu.setState(initial(head, { 0: 0n, 1: 0x5678n, 2: 0n, 3: column, 6: row, 7: column }));
      const denied = await cpu.run({ budget: 50n });
      assert.equal(denied.reason, 'data-fault');
      assert.equal(denied.retired, 15n);
      assert.equal(denied.state.rip, head);
      assert.equal(denied.state.registers[0], 15n);
      assert.equal(denied.state.registers[2], 0x5678n);
      assert.equal(denied.state.registers[6], row + 32n);
      assert.deepEqual(cpu.readMemory(column, 4), hex('78560000'));
      fault(denied, {
        instructionPc: head,
        address: row + 32n,
        width: 16,
        access: 'read',
        cause: 'unmapped',
        failingAddress: row + 32n,
        segment: 'ds',
      });
    } finally {
      cpu.dispose();
    }
  }
});

test('a packed SSE loop publishes the zero test flags before a denied store', async () => {
  // JMP head; INC EBX; ADD ESI,16; CMP EBX,3; JAE done.
  // head: MOVUPS; CMPNLEPS; MOVMSKPS; TEST; JNZ earlier; MOV store; MOV load; JMP earlier.
  const code = hex('eb0affc383c61083fb0373140f100e0fc2c8060f50c185c075e8890f8b17ebe290');
  const data = new Uint8Array(32);
  const lanes = new DataView(data.buffer);
  for (let offset = 0; offset < 16; offset += 4) lanes.setFloat32(offset, 1, true);
  const cpu = await createCpu({
    compiler,
    entry: head,
    context: protectedSnapshot(),
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'source', bytes: data },
        { id: 'output', bytes: new Uint8Array(4) },
      ],
      mappings: [
        { ...mapping(head, 'code', code.length), permissions: { ...readOnly, execute: true } },
        mapping(row, 'source', data.length),
        mapping(column, 'output', 4),
      ],
    },
    memoryLimits: {
      maxBackings: 3,
      maxMappings: 3,
      maxBackingBytes: code.length + data.length + 4,
    },
  });
  try {
    cpu.setState(initial(head, { 1: 0x1234n, 3: 0n, 6: row, 7: column }));
    const denied = await cpu.run({ budget: 60n });
    assert.equal(denied.reason, 'data-fault');
    assert.equal(denied.retired, 15n);
    assert.equal(denied.state.rip, head + 26n);
    assert.equal(denied.state.flags, 0x46n);
    assert.deepEqual(cpu.readMemory(column, 4), new Uint8Array(4));
    fault(denied, {
      instructionPc: head + 26n,
      address: column,
      width: 4,
      access: 'write',
      cause: 'permission',
      failingAddress: column,
      segment: 'ds',
    });
  } finally {
    cpu.dispose();
  }
});

test('a packed SSE loop publishes the latch comparison on memory and numeric failure', async () => {
  // JMP head; INC EBX; ADD ESI,16; CMP EBX,3; JAE done.
  // head: MOVUPS; CMPNLEPS; MOVMSKPS; TEST; JNZ earlier; MOV store; MOV load; JMP earlier.
  const code = hex('eb0affc383c61083fb0373140f100e0fc2c8060f50c185c075e8890f8b17ebe290');
  for (const simdFailure of [false, true]) {
    const source = new Uint8Array(simdFailure ? 32 : 16);
    const lanes = new DataView(source.buffer);
    for (let offset = 0; offset < 16; offset += 4) lanes.setFloat32(offset, 1, true);
    if (simdFailure) lanes.setUint32(16, 0x7f800001, true);
    const cpu = await createCpu({
      compiler,
      entry: head,
      context: protectedSnapshot(),
      limits: LIMITS,
      memory: {
        backings: [
          { id: 'code', bytes: code },
          { id: 'source', bytes: source },
          { id: 'output', bytes: new Uint8Array(4) },
        ],
        mappings: [
          { ...mapping(head, 'code', code.length), permissions: { ...readOnly, execute: true } },
          mapping(row, 'source', source.length),
          { ...mapping(column, 'output', 4), permissions: { ...readOnly, write: true } },
        ],
      },
      memoryLimits: {
        maxBackings: 3,
        maxMappings: 3,
        maxBackingBytes: code.length + source.length + 4,
      },
    });
    try {
      cpu.setState(initial(head, { 3: 0n, 6: row, 7: column }));
      cpu.setMxcsr(0x1f00);
      const failed = await cpu.run({ budget: 80n });
      assert.equal(failed.reason, simdFailure ? 'simd-exception' : 'data-fault');
      assert.equal(failed.retired, simdFailure ? 11n : 10n);
      assert.equal(failed.state.rip, head + (simdFailure ? 15n : 12n));
      assert.equal(failed.state.flags, 0x93n); // CMP 1,3
      assert.equal(failed.state.registers[3], 1n);
      assert.equal(failed.state.registers[6], row + 16n);
      assert.equal(cpu.readMxcsr(), simdFailure ? 0x1f01 : 0x1f00);
      if (!simdFailure) {
        fault(failed, {
          instructionPc: head + 12n,
          address: row + 16n,
          width: 16,
          access: 'read',
          cause: 'unmapped',
          failingAddress: row + 16n,
          segment: 'ds',
        });
      }
    } finally {
      cpu.dispose();
    }
  }
});
