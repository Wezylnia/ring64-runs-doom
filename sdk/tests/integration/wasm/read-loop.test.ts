import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createCpu, type MemoryMapping } from '../../../src/index.js';
import { protectedSnapshot } from '../../../../tests/support/context.js';
import { LIMITS, changed, exit, fault, hex, initial } from '../../../../tests/support/fixture.js';

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

test('affine byte reads keep ADD flags at a denied span and resume after remapping', async () => {
  const code = hex(
    '0fb6040b01c6' + // MOVZX EAX,[EBX+ECX]; ADD ESI,EAX
      '0fb6540b0101d7' + // MOVZX EDX,[EBX+ECX+1]; ADD EDI,EDX
      '0fb6440b0201c6' +
      '0fb6540b0301d7' +
      '83c10483f90875dd', // ADD ECX,4; CMP ECX,8; JNE head
  );
  const data = hex('ff02030405060708');
  const codeMap = {
    ...mapping(head, 'code', code.length),
    permissions: { ...readOnly, execute: true },
  };
  const dataMap = mapping(row, 'bytes', data.length);
  const cpu = await createCpu({
    compiler,
    entry: head,
    context: protectedSnapshot(),
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'bytes', bytes: data },
      ],
      mappings: [codeMap, dataMap],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: code.length + data.length },
  });
  try {
    const upper = 0x1122334400000000n;
    const input = initial(head, { 0: upper, 1: 0n, 2: upper, 3: row, 6: 1n, 7: 0n });
    const budget = 30n;
    const stop = head + BigInt(code.length);
    const output = changed(
      input,
      stop,
      { 0: upper | 7n, 1: 8n, 2: upper | 8n, 6: 271n, 7: 20n },
      0x46n,
    );
    cpu.setState(input);
    exit(await cpu.run({ budget, stop }), 'stop', budget, 22n, output);

    cpu.setMappings([codeMap, { ...dataMap, length: 1 }]);
    cpu.setState(input);
    const denied = await cpu.run({ budget, stop });
    const next = head + 6n;
    const afterAdd = changed(input, next, { 0: upper | 255n, 6: 256n }, 0x16n);
    exit(denied, 'data-fault', budget, 2n, afterAdd);
    fault(denied, {
      instructionPc: next,
      address: row + 1n,
      width: 1,
      access: 'read',
      cause: 'unmapped',
      failingAddress: row + 1n,
      segment: 'ds',
    });
    cpu.setMappings([codeMap, dataMap]);
    exit(await cpu.run({ budget, stop }), 'stop', budget, 20n, output);

    const context = protectedSnapshot();
    context.segments.ds.limit = Number(row);
    cpu.setContext(context);
    cpu.setState(input);
    const segment = await cpu.run({ budget, stop });
    exit(segment, 'segment-fault', budget, 2n, afterAdd);
    assert.deepEqual(segment.segmentFault, {
      instructionPc: next,
      segment: 'ds',
      offset: row + 1n,
      width: 1,
      access: 'read',
      cause: 'limit',
      failingOffset: row + 1n,
    });
  } finally {
    cpu.dispose();
  }
});

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
        { id: 'fraction', bytes: floats(1 + 2 ** -23, 2, 3, 4) },
        { id: 'tiny', bytes: floats(2 ** -126, 2, 3, 4) },
        { id: 'half', bytes: floats(0.5, 3, 4, 5) },
      ],
      mappings: [...fixed, mapping(column, 'column', 16)],
    },
    memoryLimits: { maxBackings: 7, maxMappings: 4, maxBackingBytes: code.length + 81 },
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

    reset();
    const beforeMultiply = await cpu.run({ budget: 20n, stop: head + 5n });
    assert.equal(beforeMultiply.reason, 'stop');
    const interruptedXmm = cpu.readXmmState();
    assert.equal(interruptedXmm[0], 0x3f800000n);
    interruptedXmm[0] = upper | 0x3f800000n;
    cpu.setXmmState(interruptedXmm);
    const multiplyOnly = await cpu.run({ budget: 1n });
    assert.equal(multiplyOnly.reason, 'budget');
    assert.equal(multiplyOnly.state.rip, head + 10n);
    assert.equal(cpu.readXmmState()[0], upper | 0x40000000n);

    cpu.setMappings([fixed[0]!, mapping(row, 'fraction', 16), mapping(column, 'fraction', 16)]);
    reset();
    cpu.setMxcsr(0x0f80);
    const precisionFault = await cpu.run({ budget: 20n, stop });
    assert.equal(precisionFault.reason, 'simd-exception');
    assert.equal(precisionFault.retired, 1n);
    assert.equal(precisionFault.state.rip, head + 5n);
    assert.equal(cpu.readMxcsr(), 0x0fa0);
    assert.equal(cpu.readXmmState()[0], 0x3f800001n);

    // The exact tiny multiplication succeeds through the helper. Its subnormal
    // result is still checked as an operand by the next addition.
    cpu.setMappings([fixed[0]!, mapping(row, 'tiny', 16), mapping(column, 'half', 16)]);
    reset();
    const beforeDenormal = cpu.readXmmState();
    beforeDenormal[2] = upper | 0x3f800000n;
    cpu.setXmmState(beforeDenormal);
    cpu.setMxcsr(0x1e80);
    const denormalFault = await cpu.run({ budget: 20n, stop });
    assert.equal(denormalFault.reason, 'simd-exception');
    assert.equal(denormalFault.retired, 4n);
    assert.equal(denormalFault.state.rip, head + 22n);
    assert.equal(cpu.readMxcsr(), 0x1e82);
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [0x00400000n, 0x40c00000n, beforeDenormal[2]]);

    reset();
    cpu.setMxcsr(0x1f80);
    const partialLap = await cpu.run({ budget: 10n, stop });
    assert.equal(partialLap.reason, 'budget');
    assert.equal(partialLap.retired, 10n);
    assert.equal(partialLap.state.rip, head + 5n);
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [
      0x40400000n,
      0x40c00000n,
      upper | 0x40c00000n,
    ]);
    const completedLap = await cpu.run({ budget: 20n, stop });
    assert.equal(completedLap.reason, 'stop');
    assert.equal(completedLap.retired, 8n);
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [
      0x41400000n,
      0x41a00000n,
      upper | 0x42180000n,
    ]);
    assert.equal(cpu.readMxcsr(), 0x1fa2);

    reset();
    cpu.setMxcsr(0x1f80);
    const uninterrupted = await cpu.run({ budget: 20n, stop });
    assert.equal(uninterrupted.reason, 'stop');
    assert.equal(uninterrupted.retired, 18n);
    assert.deepEqual(uninterrupted.state, completedLap.state);
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [
      0x41400000n,
      0x41a00000n,
      upper | 0x42180000n,
    ]);
    assert.equal(cpu.readMxcsr(), 0x1fa2);
    cpu.setMxcsr(0x1f80);

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

test('a rejected float at the guarded loop head executes once through the checked head', async () => {
  const code = hex('f30f59048bf30f100c88f30f590c8bf30f10548804f30f58c14883c1024839d175de');
  // MULSS XMM0,[RBX+RCX*4]; MOVSS XMM1,[RAX+RCX*4]; MULSS XMM1,[RBX+RCX*4];
  // MOVSS XMM2,[RAX+RCX*4+4]; ADDSS XMM0,XMM1; ADD RCX,2; CMP RCX,RDX; JNE head.
  const stop = head + BigInt(code.length);
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'row', bytes: floats(0, 2) },
        { id: 'column', bytes: floats(1 + 2 ** -23) },
      ],
      mappings: [
        { ...mapping(head, 'code', code.length), permissions: { ...readOnly, execute: true } },
        mapping(row, 'row', 8),
        mapping(column, 'column', 4),
      ],
    },
    memoryLimits: { maxBackings: 3, maxMappings: 3, maxBackingBytes: code.length + 12 },
  });
  try {
    const input = initial(head, { 0: row, 1: 0n, 2: 2n, 3: column });
    cpu.setState(input);
    const upper = 0x123456789012345678901234n << 32n;
    cpu.setXmmState([upper | 0x3f800001n, ...Array<bigint>(15).fill(0n)]);
    cpu.setMxcsr(0x5f80); // The exact product lies above 0x3f800002. Round upward to 0x3f800003.
    const rounded = await cpu.run({ budget: 8n, stop });
    assert.equal(rounded.reason, 'budget');
    assert.equal(rounded.retired, 8n);
    assert.equal(rounded.state.rip, stop);
    assert.equal(rounded.state.registers[1], 2n);
    assert.deepEqual(cpu.readXmmState().slice(0, 3), [upper | 0x3f800003n, 0n, 0x40000000n]);
    assert.equal(cpu.readMxcsr(), 0x5fa0);

    cpu.setState(input);
    cpu.setXmmState([upper | 0x7f800001n, ...Array<bigint>(15).fill(0n)]);
    cpu.setMxcsr(0x1f00); // Unmasked invalid operation on the signaling NaN at the loop head.
    const invalid = await cpu.run({ budget: 20n, stop });
    assert.equal(invalid.reason, 'simd-exception');
    assert.equal(invalid.retired, 0n);
    assert.equal(invalid.state.rip, head);
    assert.equal(cpu.readXmmState()[0], upper | 0x7f800001n);
    assert.equal(cpu.readMxcsr(), 0x1f01);
  } finally {
    cpu.dispose();
  }
});

test('an outlined float retry loses its proof when the resumed lap exits', async () => {
  const loop = body();
  // DEC R10; JZ stop; MOV RAX,R8; MOV RBX,R9; XOR ECX,ECX; JMP head.
  const code = Uint8Array.of(...loop, ...hex('49ffca740a4c89c04c89cb31c9ebca'));
  const missing = row + 0x8000n;
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'row', bytes: floats(2 ** -149, 2) },
        { id: 'column', bytes: floats(1, 1) },
      ],
      mappings: [
        { ...mapping(head, 'code', code.length), permissions: { ...readOnly, execute: true } },
        mapping(row, 'row', 8),
        mapping(column, 'column', 8),
      ],
    },
    memoryLimits: { maxBackings: 3, maxMappings: 3, maxBackingBytes: code.length + 16 },
  });
  try {
    cpu.setState(initial(head, { 0: row, 1: 0n, 2: 2n, 3: column, 8: missing, 9: column, 10: 2n }));
    const denied = await cpu.run({ budget: 40n, stop: head + BigInt(code.length) });
    assert.equal(denied.reason, 'data-fault');
    assert.equal(denied.retired, 15n);
    assert.equal(denied.state.rip, head);
    assert.equal(denied.state.registers[10], 1n);
    fault(denied, {
      instructionPc: head,
      address: missing,
      width: 4,
      access: 'read',
      cause: 'unmapped',
      failingAddress: missing,
      segment: 'ds',
    });
  } finally {
    cpu.dispose();
  }
});

test('a read loop interleaving stack and data still faults at the first denied stack operand', async () => {
  const code = hex('8b048c8b1e48ffc14839d175f3f4');
  // MOV EAX,[RSP+RCX*4]; MOV EBX,[RSI]; INC RCX; CMP RCX,RDX; JNE head; HLT
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'stack', bytes: hex('1122334444556677') },
        { id: 'data', bytes: hex('12345678') },
      ],
      mappings: [
        {
          ...mapping(head, 'code', code.length),
          permissions: { ...readOnly, execute: true },
        },
        mapping(row, 'stack', 4),
        {
          ...mapping(row + 4n, 'stack', 4),
          offset: 4,
          permissions: { read: false, write: true, execute: false },
        },
        mapping(column, 'data', 4),
      ],
    },
    memoryLimits: { maxBackings: 3, maxMappings: 4, maxBackingBytes: code.length + 12 },
  });
  try {
    cpu.setState(initial(head, { 1: 0n, 2: 2n, 4: row, 6: column }));
    const denied = await cpu.run({ budget: 20n });
    assert.equal(denied.reason, 'data-fault');
    assert.equal(denied.retired, 5n);
    assert.equal(denied.state.registers[0], 0x44332211n);
    assert.equal(denied.state.registers[3], 0x78563412n);
    fault(denied, {
      instructionPc: head,
      address: row + 4n,
      width: 4,
      access: 'read',
      cause: 'permission',
      failingAddress: row + 4n,
      segment: 'ss',
    });
  } finally {
    cpu.dispose();
  }
});
