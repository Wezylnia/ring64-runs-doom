import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { createCpu } from '../../../src/index.js';
import { Core } from '../../../src/wasm/core.js';
import { LIMITS, fault, hex, initial } from '../../../../tests/support/fixture.js';

const compiler = await WebAssembly.compile(
  new Uint8Array(await readFile(new URL('../../../../build/ring64.wasm', import.meta.url))),
);

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
  Core.prototype.instantiate = function (module, observed) {
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

    const cap = await createCpu({
      ...options,
      limits: { ...LIMITS, maxArtifactBytes: guarded.byteLength - 1 },
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
