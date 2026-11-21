import assert from 'node:assert/strict';
import test from 'node:test';
import { createCpu } from '../../../../src/index.js';
import { protectedSnapshot } from '../../../../../tests/support/context.js';
import {
  LIMITS,
  changed,
  exit,
  fault,
  hex,
  initial,
} from '../../../../../tests/support/fixture.js';
import { compiler, head, row, column, readOnly, mapping } from './fixture.js';

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

test('dependent dword reads wrap the scaled index and resume at the faulting instruction', async () => {
  const code = hex(
    '8b4483fc'.repeat(4) + // MOV EAX,[EBX+EAX*4-4] four times
      '83e90175eb', // SUB ECX,1; JNE head
  );
  const data = hex('01000040020000000300000001000000');
  const codeMap = {
    ...mapping(head, 'code', code.length),
    permissions: { ...readOnly, execute: true },
  };
  const dataMap = mapping(row, 'indices', data.length);
  const cpu = await createCpu({
    compiler,
    entry: head,
    context: protectedSnapshot(),
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'indices', bytes: data },
      ],
      mappings: [codeMap, { ...dataMap, length: 12 }],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: code.length + data.length },
  });
  try {
    const upper = 0x1122334400000000n;
    const input = initial(head, { 0: upper, 1: upper | 1n, 3: upper | (row + 4n) }, 0x893n);
    const time = { tsc: 0xffff_ffff_ffff_fffen, tscAux: 33 };
    const stop = head + BigInt(code.length);
    cpu.setState(input);
    cpu.setTimeState(time);

    // The first load returns 0x40000001; scaling that fresh index wraps to four.
    const partial = await cpu.run({ budget: 2n, stop });
    exit(partial, 'budget', 2n, 2n, changed(input, head + 8n, { 0: upper | 2n }));
    assert.deepEqual(cpu.readTimeState(), { ...time, tsc: 0n });

    cpu.setState(input);
    cpu.setTimeState(time);
    const faultPc = head + 12n;
    const denied = await cpu.run({ budget: 12n, stop });
    exit(denied, 'data-fault', 12n, 3n, changed(input, faultPc, { 0: upper | 3n }));
    fault(denied, {
      instructionPc: faultPc,
      address: row + 12n,
      width: 4,
      access: 'read',
      cause: 'unmapped',
      failingAddress: row + 12n,
      segment: 'ds',
    });
    assert.deepEqual(cpu.readTimeState(), { ...time, tsc: 1n });

    cpu.setMappings([codeMap, dataMap]);
    const repaired = await cpu.run({ budget: 12n, stop });
    exit(repaired, 'stop', 12n, 3n, changed(input, stop, { 0: upper | 1n, 1: upper }, 0x46n));
    assert.deepEqual(cpu.readTimeState(), { ...time, tsc: 4n });
  } finally {
    cpu.dispose();
  }
});

test('Long64 dependent dword reads preserve full indices and resume a denied load', async () => {
  const code = hex('8b4483fc'.repeat(4) + '83e90175eb');
  // MOV EAX,[RBX+RAX*4-4] four times, followed by SUB ECX,1; JNE head.
  const high = 0xffff_8000_0000_4000n;
  const index = 0x1_0000_0000n;
  const data = hex('63000000020000000300000001000000');
  const farData = hex('01000000');
  const codeMap = {
    ...mapping(head, 'code', code.length),
    permissions: { ...readOnly, execute: true },
  };
  const dataMap = mapping(high, 'indices', data.length);
  const farMap = mapping(high + 4n * index, 'far-index', farData.length);
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'indices', bytes: data },
        { id: 'far-index', bytes: farData },
      ],
      mappings: [codeMap, { ...dataMap, length: 12 }, farMap],
    },
    memoryLimits: {
      maxBackings: 3,
      maxMappings: 3,
      maxBackingBytes: code.length + data.length + farData.length,
    },
  });
  try {
    const budget = 12n;
    const stop = head + BigInt(code.length);
    const input = initial(head, { 0: index, 1: 1n, 3: high + 4n }, 0x893n);
    cpu.setState(input);
    const denied = await cpu.run({ budget, stop });
    const faultPc = head + 12n;
    exit(denied, 'data-fault', budget, 3n, changed(input, faultPc, { 0: 3n }));
    fault(denied, {
      instructionPc: faultPc,
      address: high + 12n,
      width: 4,
      access: 'read',
      cause: 'unmapped',
      failingAddress: high + 12n,
      segment: 'ds',
    });

    cpu.setMappings([codeMap, dataMap, farMap]);
    exit(
      await cpu.run({ budget, stop }),
      'stop',
      budget,
      3n,
      changed(input, stop, { 0: 1n, 1: 0n }, 0x46n),
    );
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

test('rotated read branches publish the selected CMP at exits and partial laps', async () => {
  const code = hex(
    'eb06' + // JMP head
      '89d339dd7316' + // MOV EBX,EDX; CMP EBP,EBX; JAE upperExit
      '89de29eed1ee8d142e' + // head: MOV ESI,EBX; SUB ESI,EBP; SHR ESI,1; LEA EDX,[ESI+EBP]
      '393c9173ec' + // CMP [ECX+EDX*4],EDI; JAE upper arm
      '01f54539dd72eb' + // ADD EBP,ESI; INC EBP; CMP EBP,EBX; JB head
      '9090', // separate lower and upper exit PCs
  );
  const entry = head + 8n;
  const lowerExit = head + 29n;
  const upperExit = head + 30n;
  const data = hex('0100000002000000030000000400000005000000060000000700000008000000');
  const context = protectedSnapshot();
  const cpu = await createCpu({
    compiler,
    entry,
    context,
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'table', bytes: data },
      ],
      mappings: [
        { ...mapping(head, 'code', code.length), permissions: { ...readOnly, execute: true } },
        mapping(row, 'table', data.length),
      ],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: code.length + data.length },
  });
  const upper = 0x1234_5678_0000_0000n;
  const input = (length: bigint, key: bigint) =>
    initial(
      entry,
      { 1: row, 2: upper, 3: upper | length, 5: upper, 6: upper, 7: upper | key },
      0x602n,
    );
  try {
    for (const [key, stop, retired, bound] of [
      [2n, lowerExit, 10n, 1n],
      [0n, upperExit, 9n, 0n],
    ] as const) {
      const state = input(1n, key);
      cpu.setState(state);
      exit(
        await cpu.run({ budget: 20n, stop }),
        'stop',
        20n,
        retired,
        changed(state, stop, { 3: upper | bound, 5: upper | bound }, 0x646n),
      );
    }

    const lower = input(8n, 9n);
    cpu.setState(lower);
    exit(
      await cpu.run({ budget: 10n }),
      'budget',
      10n,
      10n,
      changed(lower, entry, { 2: upper | 4n, 5: upper | 5n, 6: upper | 4n }, 0x693n),
    );

    // The shorter upper arm leaves budget for the next MOV, which preserves CMP flags.
    const higher = input(8n, 0n);
    cpu.setState(higher);
    exit(
      await cpu.run({ budget: 10n }),
      'budget',
      10n,
      10n,
      changed(higher, entry + 2n, { 2: upper | 4n, 3: upper | 4n, 6: upper | 4n }, 0x697n),
    );

    // One funded backedge executes, then the original read fails in the next lap.
    context.segments.ds.limit = Number(row + 19n);
    cpu.setContext(context);
    cpu.setState(lower);
    cpu.setTimeState({ tsc: 0xffff_ffff_ffff_fff8n, tscAux: 19 });
    const denied = await cpu.run({ budget: 30n });
    const read = head + 17n;
    exit(
      denied,
      'segment-fault',
      30n,
      14n,
      changed(lower, read, { 2: upper | 6n, 5: upper | 5n, 6: upper | 1n }, 0x603n),
    );
    assert.equal(denied.reason, 'segment-fault');
    assert.deepEqual(denied.segmentFault, {
      access: 'read',
      cause: 'limit',
      failingOffset: row + 24n,
      offset: row + 24n,
      segment: 'ds',
      width: 4,
      instructionPc: read,
    });
    assert.deepEqual(cpu.readTimeState(), { tsc: 6n, tscAux: 19 });
  } finally {
    cpu.dispose();
  }
});
