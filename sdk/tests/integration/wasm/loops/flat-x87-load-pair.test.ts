import assert from 'node:assert/strict';
import test from 'node:test';
import { createCpu } from '../../../../src/index.js';
import { Core } from '../../../../src/wasm/core.js';
import { protectedSnapshot } from '../../../../../tests/support/context.js';
import { LIMITS, hex, initial } from '../../../../../tests/support/fixture.js';
import { compiler, head, mapping, readOnly } from './fixture.js';

test('flat x87 register-load pairs recheck retained reads after remapping', async () => {
  const code = hex('d9c0d803f4'); // FLD ST(0); FADD dword [EBX]; HLT
  const operand = 0x4ffcn;
  const codeMap = {
    ...mapping(head, 'code', code.length),
    permissions: { ...readOnly, execute: true },
  };
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    context: protectedSnapshot(),
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'first', bytes: hex('0000803f') },
        { id: 'second', bytes: hex('00000040') },
        { id: 'short', bytes: hex('000080') },
      ],
      mappings: [codeMap, mapping(operand, 'first', 4)],
    },
    memoryLimits: { maxBackings: 4, maxMappings: 2, maxBackingBytes: code.length + 11 },
  });
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let pairResults: number[] = [];
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const helper = env.x87;
    if (typeof helper === 'function') {
      env.x87 = (...args: [number, bigint, number, bigint, bigint, bigint]) => {
        const result = helper(...args) as number;
        if (args[2] >>> 0 === 0x80000002) pairResults.push(result);
        return result;
      };
    }
    return env;
  };
  const one = 0x3fff8000000000000000n;
  const original = {
    registers: [one, ...Array<bigint>(7).fill(0n)],
    controlWord: 0x37f,
    statusWord: 0x4500,
    tagWord: 0xfffc,
    opcode: 0x142,
    instructionPointer: 0x3000n,
    dataPointer: 0x4000n,
  };
  const run = async (address: bigint) => {
    cpu.setState(initial(head, { 3: address }));
    cpu.setX87State(original);
    pairResults = [];
    return cpu.run({ budget: 8n, stop: head + 4n });
  };
  try {
    for (const [backing, value] of [
      ['first', 0x40008000000000000000n],
      ['first', 0x40008000000000000000n],
      ['second', 0x4000c000000000000000n],
    ] as const) {
      if (backing === 'second') cpu.setMappings([codeMap, mapping(operand, backing, 4)]);
      const result = await run(operand);
      assert.equal(result.reason, 'stop');
      assert.equal(result.retired, 2n);
      assert.deepEqual(pairResults, [0]);
      const x87 = cpu.readX87State();
      assert.equal(x87.registers[7], value);
      assert.equal(x87.instructionPointer, head + 2n);
      assert.equal(x87.dataPointer, operand);
    }
    for (const [address, remap] of [
      [operand + 1n, false],
      [operand, true],
    ] as const) {
      if (remap) cpu.setMappings([codeMap, mapping(operand, 'short', 3)]);
      const result = await run(address);
      assert.equal(result.reason, 'data-fault');
      assert.equal(result.retired, 1n);
      assert.equal(result.state.rip, head + 2n);
      assert.deepEqual(pairResults, [-1]);
      const x87 = cpu.readX87State();
      assert.equal(x87.registers[7], one);
      assert.equal(x87.instructionPointer, head);
      assert.equal(x87.dataPointer, original.dataPointer);
    }
  } finally {
    prototype.artifactImports = imports;
    cpu.dispose();
  }
});
