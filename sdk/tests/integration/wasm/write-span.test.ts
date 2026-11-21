import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createCpu } from '../../../src/index.js';
import { Core } from '../../../src/wasm/core.js';
import { protectedSnapshot } from '../../../../tests/support/context.js';
import {
  LIMITS,
  changed,
  exit,
  fault,
  hex,
  initial,
  memory,
} from '../../../../tests/support/fixture.js';

const compiler = await WebAssembly.compile(
  new Uint8Array(await readFile(new URL('../../../../build/ring64.wasm', import.meta.url))),
);

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
