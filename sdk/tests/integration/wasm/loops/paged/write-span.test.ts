import assert from 'node:assert/strict';
import test from 'node:test';
import { createCpu } from '../../../../../src/index.js';
import { Core } from '../../../../../src/wasm/core.js';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import { protectedSnapshot } from '../../../../../../tests/support/context.js';
import {
  LIMITS,
  changed,
  exit,
  fault,
  hex,
  initial,
  memory,
} from '../../../../../../tests/support/fixture.js';
import { compiler } from '../fixture.js';

function lebSize(value: number): number {
  let size = 1;
  for (; value >= 128; value >>>= 7) size++;
  return size;
}

function branchHintBytes(module: WebAssembly.Module): number {
  const name = 'metadata.code.branch_hint';
  const nameSize = lebSize(name.length) + name.length;
  return WebAssembly.Module.customSections(module, name).reduce((size, data) => {
    const payloadSize = nameSize + data.byteLength;
    return size + 1 + lebSize(payloadSize) + payloadSize;
  }, 0);
}

test('loop stores use private writable spans and fall back for faults and translated code', async () => {
  const head = 0x31000n;
  const data = 0x49000n;
  const code = hex('b04288040f48ffc14839d175f5b301f4');
  // MOV AL,42h; loop: MOV [RDI+RCX],AL; INC RCX; CMP RCX,RDX; JNE loop; MOV BL,1; HLT
  const codeMap = {
    base: head,
    backing: 'code',
    offset: 0,
    length: code.length,
    permissions: { read: true, write: false, execute: true },
  };
  const dataMap = {
    base: data,
    backing: 'data',
    offset: 0,
    length: 2,
    permissions: { read: true, write: true, execute: false },
  };
  const options = {
    compiler,
    entry: head,
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'data', bytes: Uint8Array.of(0xaa, 0xbb) },
      ],
      mappings: [codeMap, dataMap],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: code.length + 2 },
  };
  const cpu = await createCpu(options);
  const instantiate = Core.prototype.instantiate;
  const imports: boolean[] = [];
  let firstModule: WebAssembly.Module | undefined;
  Core.prototype.instantiate = function (module, observed) {
    firstModule ??= module;
    imports.push(WebAssembly.Module.imports(module).some(({ name }) => name === 'write_span'));
    return instantiate.call(this, module, observed);
  };
  try {
    const input = initial(head, { 1: 0n, 2: 3n, 7: data });
    cpu.setState(input);
    const interior = await cpu.run({ budget: 20n, stop: head + 5n });
    assert.equal(interior.reason, 'stop');
    assert.equal(interior.retired, 2n);
    assert.equal(interior.state.registers[1], 0n);
    assert.deepEqual(cpu.readMemory(data, 2), Uint8Array.of(0x42, 0xbb));
    assert.deepEqual(imports, [true]);
    const guarded = cpu.inspectTranslations()[0]!;

    cpu.setState(input);
    const overflow = await cpu.run({ budget: 20n });
    assert.equal(overflow.reason, 'data-fault');
    assert.equal(overflow.retired, 9n);
    assert.equal(overflow.state.registers[1], 2n);
    assert.deepEqual(cpu.readMemory(data, 2), Uint8Array.of(0x42, 0x42));
    fault(overflow, {
      instructionPc: head + 2n,
      address: data + 2n,
      width: 1,
      access: 'write',
      cause: 'unmapped',
      failingAddress: data + 2n,
      segment: 'ds',
    });

    cpu.setMappings([
      codeMap,
      { ...dataMap, permissions: { ...dataMap.permissions, write: false } },
    ]);
    cpu.setState(input);
    const denied = await cpu.run({ budget: 20n });
    assert.equal(denied.reason, 'data-fault');
    assert.equal(denied.retired, 1n);
    fault(denied, {
      instructionPc: head + 2n,
      address: data,
      width: 1,
      access: 'write',
      cause: 'permission',
      failingAddress: data,
      segment: 'ds',
    });

    assert.ok(firstModule);
    // Optional annotations can be dropped before the executable fast path.
    const guardedCodeBytes = guarded.byteLength - branchHintBytes(firstModule);
    const cap = await createCpu({
      ...options,
      limits: { ...LIMITS, maxArtifactBytes: guardedCodeBytes - 1 },
    });
    try {
      cap.setState(input);
      const checked = await cap.run({ budget: 20n, stop: head + 5n });
      assert.deepEqual(checked.state, interior.state);
      assert.deepEqual(cap.readMemory(data, 2), Uint8Array.of(0x42, 0xbb));
      assert.deepEqual(imports, [true, false]);
    } finally {
      cap.dispose();
    }

    cpu.setMappings([codeMap, { ...dataMap, backing: 'code', offset: 14, length: 1 }]);
    cpu.setState(initial(head, { 1: 0n, 2: 1n, 3: 0n, 7: data }));
    const patched = await cpu.run({ budget: 20n, stop: head + 15n });
    assert.equal(patched.reason, 'stop');
    assert.equal(patched.retired, 6n);
    assert.equal(patched.state.registers[3], 0x42n);
    assert.deepEqual(cpu.readMemory(data, 1), Uint8Array.of(0x42));
    assert.ok(cpu.inspectTranslations().every(({ owner }) => owner !== guarded.owner));
  } finally {
    Core.prototype.instantiate = instantiate;
    cpu.dispose();
  }
});

test('a retained flat vector write span follows remaps and executable aliases', async () => {
  const head = 0x31000n;
  const data = 0x49000n;
  // MOVDQU [EBX],XMM0; JMP head. The extra code bytes make an entire alias writable.
  const code = hex('f30f7f03ebfa' + '90'.repeat(10));
  const codeMap = {
    base: head,
    backing: 'code',
    offset: 0,
    length: code.length,
    permissions: { read: true, write: false, execute: true },
  };
  const dataMap = {
    base: data,
    backing: 'data',
    offset: 0,
    length: 16,
    permissions: { read: true, write: true, execute: false },
  };
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    context: protectedSnapshot(),
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'data', bytes: new Uint8Array(16) },
        { id: 'read-only', bytes: new Uint8Array(16).fill(0xaa) },
      ],
      mappings: [codeMap, dataMap],
    },
    memoryLimits: { maxBackings: 3, maxMappings: 2, maxBackingBytes: code.length + 32 },
  });
  let probes = 0;
  const restoreImports = observeArtifactImports((env) => {
    const write = env.write_span;
    if (typeof write === 'function') {
      env.write_span = (address: bigint, width: number, segment: number) => {
        probes++;
        return write(address, width, segment);
      };
    }
  });
  try {
    const start = initial(head, { 3: data });
    cpu.setState(start);
    cpu.setXmmState([0x11223344n, ...Array<bigint>(15).fill(0n)]);
    const first = await cpu.run({ budget: 2n });
    assert.equal(first.retired, 2n);
    assert.equal(probes, 1);
    const owner = cpu.inspectTranslations()[0]!.owner;

    cpu.setState(start);
    cpu.setXmmState([0x55667788n, ...Array<bigint>(15).fill(0n)]);
    const warm = await cpu.run({ budget: 2n });
    assert.equal(warm.resources.compilations, 0);
    assert.equal(warm.retired, 2n);
    assert.equal(probes, 1);
    assert.deepEqual(cpu.readMemory(data, 4), hex('88776655'));

    cpu.setMappings([
      codeMap,
      { ...dataMap, backing: 'read-only', permissions: { ...dataMap.permissions, write: false } },
    ]);
    cpu.removeBacking('data');
    cpu.setState(start);
    const denied = await cpu.run({ budget: 2n });
    assert.equal(denied.reason, 'data-fault');
    assert.equal(denied.retired, 0n);
    assert.equal(probes, 2);
    assert.deepEqual(cpu.readMemory(data, 16), new Uint8Array(16).fill(0xaa));

    cpu.setMappings([codeMap, { ...dataMap, backing: 'code' }]);
    cpu.setState(start);
    const alias = await cpu.run({ budget: 1n });
    assert.equal(alias.retired, 1n);
    assert.equal(probes, 3);
    assert.deepEqual(cpu.readMemory(head, 4), hex('88776655'));
    assert.ok(cpu.inspectTranslations().every((row) => row.owner !== owner));
  } finally {
    restoreImports();
    cpu.dispose();
  }
});

test('a loop store crossing its cached span admits the entire operand before publishing it', async () => {
  const head = 0x31000n;
  const data = 0x49000n;
  // MOV EAX,44332211h; loop: MOV [RDI+RCX*4],EAX; INC RCX; CMP RCX,RDX; JNE loop; HLT
  const code = hex('b81122334489048f48ffc14839d175f5f4');
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'data', bytes: new Uint8Array(8).fill(0xaa) },
      ],
      mappings: [
        {
          base: head,
          backing: 'code',
          offset: 0,
          length: code.length,
          permissions: { read: true, write: false, execute: true },
        },
        {
          base: data,
          backing: 'data',
          offset: 0,
          length: 6,
          permissions: { read: true, write: true, execute: false },
        },
        {
          base: data + 6n,
          backing: 'data',
          offset: 6,
          length: 2,
          permissions: { read: true, write: false, execute: false },
        },
      ],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 3, maxBackingBytes: code.length + 8 },
  });
  try {
    cpu.setState(initial(head, { 1: 0n, 2: 2n, 7: data }));
    const denied = await cpu.run({ budget: 20n });
    assert.equal(denied.reason, 'data-fault');
    assert.equal(denied.retired, 5n);
    assert.deepEqual(cpu.readMemory(data, 8), hex('11223344aaaaaaaa'));
    fault(denied, {
      instructionPc: head + 5n,
      address: data + 4n,
      width: 4,
      access: 'write',
      cause: 'permission',
      failingAddress: data + 6n,
      segment: 'ds',
    });
  } finally {
    cpu.dispose();
  }
});

test('fixed stack slots preserve the spill and compare flags when the following load faults', async () => {
  const head = 0x31000n;
  const esp = 0x49000n;
  const stack = esp + 16n;
  const spill = esp + 24n;
  // Spill first, then CMP/JB over the middle slot's load. SUB replaces flags on success.
  const code = hex('8b5424108954241883f80172068b4c241489ca83ee0175e890');
  const stackBytes = hex('01000000efcdab89aaaaaaaa');
  const codeMap = {
    base: head,
    backing: 'code',
    offset: 0,
    length: code.length,
    permissions: { read: true, write: false, execute: true },
  };
  const stackMap = {
    base: stack,
    backing: 'stack',
    offset: 0,
    length: stackBytes.length,
    permissions: { read: true, write: true, execute: false },
  };
  const cpu = await createCpu({
    compiler,
    entry: head,
    context: protectedSnapshot(),
    limits: LIMITS,
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'stack', bytes: stackBytes },
      ],
      mappings: [codeMap, stackMap],
    },
    memoryLimits: {
      maxBackings: 2,
      maxMappings: 3,
      maxBackingBytes: code.length + stackBytes.length,
    },
  });
  try {
    const input = initial(head, { 0: 0x80000000n, 1: 0n, 2: 0n, 4: esp, 6: 1n });
    const stop = head + 24n;
    const budget = 20n;
    cpu.setState(input);
    const completed = await cpu.run({ budget, stop });
    exit(
      completed,
      'stop',
      budget,
      8n,
      changed(input, stop, { 1: 0x89abcdefn, 2: 0x89abcdefn, 6: 0n }, 0x46n),
    );
    memory(cpu, spill, '01000000');

    cpu.setMappings([
      codeMap,
      { ...stackMap, length: 4 },
      { ...stackMap, base: spill, offset: 8, length: 4 },
    ]);
    cpu.writeMemory(spill, hex('aaaaaaaa'));
    cpu.setState(input);
    const denied = await cpu.run({ budget, stop });
    const faultPc = head + 13n;
    exit(denied, 'data-fault', budget, 4n, changed(input, faultPc, { 2: 1n }, 0x816n));
    fault(denied, {
      instructionPc: faultPc,
      address: esp + 20n,
      width: 4,
      access: 'read',
      cause: 'unmapped',
      failingAddress: esp + 20n,
      segment: 'ss',
    });
    memory(cpu, spill, '01000000');
  } finally {
    cpu.dispose();
  }
});

test('sampled stack stores preserve failed PUSH and POP restart state', async () => {
  const head = 0x31000n;
  const low = 0x70000n;
  const stack = low + 4n;
  const code = hex('8b0424894424046a105b4975f3f4');
  // MOV EAX,[ESP]; MOV [ESP+4],EAX; PUSH 16; POP EBX; DEC ECX; JNE head
  const stop = head + 13n;
  const push = head + 7n;
  const pop = head + 9n;
  const permissions = { read: true, write: true, execute: false };
  const codeMap = {
    base: head,
    backing: 'code',
    offset: 0,
    length: code.length,
    permissions: { read: true, write: false, execute: true },
  };
  const whole = { base: low, backing: 'stack', offset: 0, length: 12, permissions };
  const high = { ...whole, base: stack, offset: 4, length: 8 };
  const writeOnly = {
    ...whole,
    length: 4,
    permissions: { ...permissions, read: false },
  };
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, points, route) {
    if (pc < head || pc >= stop || route) return compile.call(this, pc, limits, points, route);
    return compile.call(
      this,
      pc,
      limits,
      [head, head + 3n, push, pop, head + 10n, head + 11n],
      true,
    );
  };
  const cpu = await createCpu({
    compiler,
    limits: LIMITS,
    entry: head,
    context: protectedSnapshot(),
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'stack', bytes: hex('000000007856341200000000') },
      ],
      mappings: [codeMap, whole],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 3, maxBackingBytes: code.length + 12 },
  });
  try {
    const input = initial(head, { 0: 0n, 1: 1n, 3: 0n, 4: stack });
    cpu.setState(input);
    const ready = await cpu.run({ budget: 24n, stop });
    assert.equal(ready.reason, 'stop');
    assert.equal(ready.retired, 6n);
    assert.equal(ready.state.registers[3], 16n);
    assert.equal(ready.state.registers[4], stack);
    memory(cpu, low, '100000007856341278563412');

    cpu.setMappings([codeMap, high]);
    cpu.setState(input);
    const deniedPush = await cpu.run({ budget: 24n, stop });
    assert.equal(deniedPush.reason, 'data-fault');
    assert.equal(deniedPush.retired, 2n);
    assert.equal(deniedPush.state.rip, push);
    assert.equal(deniedPush.state.registers[4], stack);
    fault(deniedPush, {
      instructionPc: push,
      address: low,
      width: 4,
      access: 'write',
      cause: 'unmapped',
      failingAddress: low,
      segment: 'ss',
    });

    cpu.setMappings([codeMap, writeOnly, high]);
    const deniedPop = await cpu.run({ budget: 24n, stop });
    assert.equal(deniedPop.reason, 'data-fault');
    assert.equal(deniedPop.retired, 1n);
    assert.equal(deniedPop.state.rip, pop);
    assert.equal(deniedPop.state.registers[3], 0n);
    assert.equal(deniedPop.state.registers[4], low);
    fault(deniedPop, {
      instructionPc: pop,
      address: low,
      width: 4,
      access: 'read',
      cause: 'permission',
      failingAddress: low,
      segment: 'ss',
    });

    cpu.setMappings([codeMap, whole]);
    const resumed = await cpu.run({ budget: 24n, stop });
    assert.equal(resumed.reason, 'stop');
    assert.equal(resumed.retired, 3n);
    assert.deepEqual(resumed.state, ready.state);
    memory(cpu, low, '100000007856341278563412');
  } finally {
    cpu.dispose();
    Core.prototype.compile = compile;
  }
});

test('flat memory rotates reuse checked spans and fault before changing flags', async () => {
  const head = 0x31000n;
  const data = 0x49000n;
  // MOV [EBX],EDI; MOV ESI,[EBX]; ROL [EBX],CL; ROR [EBX],CL; CALL EAX; DEC EDX; JNZ head.
  const code = hex('893b8b33d303d30bffd04a75f390');
  const codeMap = {
    base: head,
    backing: 'code',
    offset: 0,
    length: code.length,
    permissions: { read: true, write: false, execute: true },
  };
  const dataMap = {
    base: data,
    backing: 'data',
    offset: 0,
    length: 4,
    permissions: { read: true, write: true, execute: false },
  };
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    context: protectedSnapshot(),
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'data', bytes: new Uint8Array(4) },
        { id: 'readonly', bytes: hex('78563412') },
      ],
      mappings: [codeMap, dataMap],
    },
    memoryLimits: { maxBackings: 3, maxMappings: 2, maxBackingBytes: code.length + 8 },
  });
  let checkedReads = 0;
  const restoreImports = observeArtifactImports((env) => {
    const read = env.segmented_load_for_write;
    assert.ok(typeof read === 'function');
    env.segmented_load_for_write = (address: bigint, width: number, segment: number) => {
      checkedReads++;
      return read(address, width, segment);
    };
  });
  try {
    const flags = 0x42n;
    for (const [count, retired, offset, value, expectedFlags] of [
      [1n, 3n, 6n, '03000000', flags | 0x801n],
      [1n, 4n, 8n, '01000080', flags | 0x801n],
      [32n, 4n, 8n, '01000080', flags],
    ] as const) {
      cpu.setState(initial(head, { 1: count, 3: data, 7: 0x80000001n }, flags));
      const result = await cpu.run({ budget: 20n, stop: head + offset });
      assert.equal(result.reason, 'stop');
      assert.equal(result.retired, retired);
      assert.equal(result.state.flags, expectedFlags);
      assert.deepEqual(cpu.readMemory(data, 4), hex(value));
    }
    assert.equal(checkedReads, 0);
    cpu.setMappings([
      codeMap,
      { ...dataMap, backing: 'readonly', permissions: { ...dataMap.permissions, write: false } },
    ]);
    cpu.removeBacking('data');
    cpu.setState(initial(head + 4n, { 1: 32n, 3: data }, flags));
    const denied = await cpu.run({ budget: 20n, stop: head + 6n });
    assert.equal(denied.reason, 'data-fault');
    assert.equal(denied.retired, 0n);
    assert.equal(denied.state.flags, flags);
    assert.equal(checkedReads, 1);
    assert.deepEqual(cpu.readMemory(data, 4), hex('78563412'));
    fault(denied, {
      instructionPc: head + 4n,
      address: data,
      width: 4,
      segment: 'ds',
      access: 'write',
      cause: 'permission',
      failingAddress: data,
    });
  } finally {
    restoreImports();
    cpu.dispose();
  }
});
