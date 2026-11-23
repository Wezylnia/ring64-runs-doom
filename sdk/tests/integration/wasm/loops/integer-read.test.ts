import assert from 'node:assert/strict';
import test, { type TestContext } from 'node:test';
import { createCpu } from '../../../../src/index.js';
import { Core } from '../../../../src/wasm/core.js';
import { BANK, PC, little, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
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

    const lap = changed(
      input,
      head,
      { 0: upper | 3n, 1: 4n, 2: upper | 4n, 6: 259n, 7: 6n },
      0x97n,
    );
    cpu.setState(input);
    exit(await cpu.run({ budget: 11n, stop }), 'budget', 11n, 11n, lap);

    cpu.setState(input);
    cpu.setTimeState({ tsc: 0n, tscAux: 9 });
    const partial = changed(lap, head + 11n, { 0: upper | 5n, 2: upper | 6n, 6: 264n }, 2n);
    exit(await cpu.run({ budget: 14n, stop }), 'budget', 14n, 14n, partial);
    assert.deepEqual(cpu.readTimeState(), { tsc: 14n, tscAux: 9 });

    const next = head + 6n;
    const afterAdd = changed(input, next, { 0: upper | 255n, 6: 256n }, 0x16n);
    cpu.setState(input);
    exit(await cpu.run({ budget, stop: next }), 'stop', budget, 2n, afterAdd);

    cpu.setMappings([codeMap, { ...dataMap, length: 1 }]);
    cpu.setState(input);
    const denied = await cpu.run({ budget, stop });
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

// Four MOV EAX,[RDI+RAX*4]; DEC ECX; JNZ head.
const pagedDwordLoop = '8b0487'.repeat(4) + 'ffc975f0';

function countGuardedDwordReads(t: TestContext) {
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let slowReads = 0;
  t.mock.method(prototype, 'artifactImports', function (this: Core, observed: boolean) {
    const env = imports.call(this, observed);
    const read = env.paged_loop_walk_dword;
    if (typeof read === 'function') {
      env.paged_loop_walk_dword = (address: bigint) => {
        slowReads++;
        return read(address);
      };
    }
    return env;
  });
  return () => slowReads;
}

test('direct paged dword reads follow guest INVLPG on remap and removal', async (t) => {
  const slowReads = countGuardedDwordReads(t);
  const data = 0x500000n;
  // Save the last two loop results in EBP and EBX; replace the PTE and invalidate [RDI].
  // Clear EDX for removal on the next lap; restart with EAX=0, ECX=24.
  const replace = '89dd89c34889160f013f31d231c0b918000000ebdb';
  const page = (index: bigint) => {
    const bytes = new Uint8Array(4096);
    bytes.set(little([index, index, index, index], 4));
    return bytes;
  };
  await usingPaged(
    compiler,
    {
      code: pagedDwordLoop + replace,
      words: [
        [0x4800n, 0xa067n],
        [0x4808n, 0x4067n],
      ],
      spans: [
        { physical: 0xa000n, bytes: page(2n) },
        { physical: 0xb000n, bytes: page(3n) },
      ],
    },
    async (cpu) => {
      cpu.setState(initial(PC, { 0: 0n, 1: 24n, 2: 0xb067n, 6: 0x501800n, 7: data }));
      const result = await cpu.run({ budget: 400n });
      assert.equal(result.reason, 'page-fault');
      assert.equal(result.retired, 304n);
      assert.equal(result.state.registers[5], 2n, 'loop read from the original page');
      assert.equal(result.state.registers[3], 3n, 'same loop read from the replacement page');
      assert.equal(result.pageFault?.address, data);
      assert.equal(result.state.rip, PC, 'first access after removal faulted');
      assert.ok(slowReads() > 0 && slowReads() < 24, `192 reads used ${slowReads()} slow lookups`);
    },
  );
});

test('a warm dword loop sees read permission revoked by guest WRPKRU', async () => {
  const data = 0x500000n;
  const bytes = new Uint8Array(4096);
  bytes.set(little([1n, 1n], 4));
  // Save the loop result; deny reads with key 1; reset EAX and ECX and jump back.
  const revoke = '89c3b8040000000f01ef31c0b918000000ebdd';
  await usingPaged(
    compiler,
    {
      code: pagedDwordLoop + revoke,
      bank: { ...BANK, cr4: BANK.cr4 | (1n << 22n) },
      words: [[0x4800n, (1n << 59n) | 0xa067n]],
      spans: [{ physical: 0xa000n, bytes }],
    },
    async (cpu) => {
      cpu.setState(initial(PC, { 0: 0n, 1: 24n, 2: 0n, 7: data }));
      const result = await cpu.run({ budget: 400n });
      assert.equal(result.reason, 'page-fault');
      assert.equal(result.retired, 150n);
      assert.equal(result.state.registers[3], 1n, 'the first 96 reads completed');
      assert.equal(result.state.rip, PC, 'first load after WRPKRU faulted');
      assert.equal(result.pageFault?.address, data);
      assert.equal(result.pageFault?.errorCode, 0x21);
    },
  );
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

test('sampled flat SS reads keep separate ESP and EBP mappings through remaps', async () => {
  const code = hex('0fb64500368b1c244975f5f4'); // MOVZX EAX,[EBP]; MOV EBX,SS:[ESP]; DEC ECX; JNE head
  const data = 0x49000n;
  const stack = 0x70000n;
  const stop = head + 11n;
  const codeMap = {
    ...mapping(head, 'code', code.length),
    permissions: { ...readOnly, execute: true },
  };
  const dataMap = mapping(data, 'data', 1);
  const compile = Core.prototype.compile;
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const calls: number[] = [];
  Core.prototype.compile = function (pc, limits, points, route) {
    if (pc !== head || route) return compile.call(this, pc, limits, points, route);
    return compile.call(this, pc, limits, [head, head + 4n, head + 8n, head + 9n], true);
  };
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const read = env.read_span;
    if (typeof read === 'function') {
      env.read_span = (address: bigint, width: number, segment: number) => {
        calls.push(segment);
        return read(address, width, segment);
      };
    }
    return env;
  };
  const context = protectedSnapshot();
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    context,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'data', bytes: hex('aa') },
        { id: 'first', bytes: hex('78563412') },
        { id: 'second', bytes: hex('efcdab90') },
      ],
      mappings: [codeMap, dataMap, mapping(stack, 'first', 4)],
    },
    memoryLimits: { maxBackings: 4, maxMappings: 3, maxBackingBytes: code.length + 9 },
  });
  try {
    const input = initial(head, { 0: 0n, 1: 8n, 3: 0n, 4: stack, 5: data });
    cpu.setState(input);
    const first = await cpu.run({ budget: 40n, stop });
    assert.equal(first.reason, 'stop');
    assert.equal(first.retired, 32n);
    assert.equal(first.state.registers[0], 0xaan);
    assert.equal(first.state.registers[3], 0x12345678n);
    assert.deepEqual(calls, [1, 0x101]);

    cpu.setMappings([codeMap, dataMap, mapping(stack, 'second', 4)]);
    cpu.setState(input);
    const second = await cpu.run({ budget: 40n, stop });
    assert.equal(second.reason, 'stop');
    assert.equal(second.state.registers[3], 0x90abcdefn);
    assert.deepEqual(calls, [1, 0x101, 1, 0x101]);

    for (const [base, retired, offset, eax] of [
      [stack, 1n, 4n, 0xaan],
      [data, 0n, 0n, 0n],
    ] as const) {
      const bounded = protectedSnapshot();
      bounded.segments.ss.limit = Number(base - 1n);
      bounded.segments.ss.granularity = false;
      cpu.setContext(bounded);
      cpu.setState(input);
      const denied = await cpu.run({ budget: 40n, stop });
      assert.equal(denied.reason, 'segment-fault');
      assert.equal(denied.retired, retired);
      assert.equal(denied.state.rip, head + offset);
      assert.equal(denied.state.registers[0], eax);
      assert.equal(denied.state.registers[3], 0n);
    }
  } finally {
    cpu.dispose();
    Core.prototype.compile = compile;
    prototype.artifactImports = imports;
  }
});
