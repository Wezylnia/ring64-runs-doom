import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { context, input, using } from '../../../../../tests/execution/modes/fixture.js';
import { compiler } from './fixture.js';

test('inline x87 stack pops preserve raw bank bits and checked retirement boundaries', async () => {
  const pc = 0x1000n;
  const rawTop = 0xffffdeadbeefcafebaben;
  const original = {
    registers: [0x40008000000000000000n, ...Array<bigint>(6).fill(0n), rawTop],
    controlWord: 0x37f,
    statusWord: 0x7f00,
    tagWord: 0x3fff,
    opcode: 0x235,
    instructionPointer: 0x5500n,
    dataPointer: 0x7700n,
  };
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let helperCalls = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const helper = env.x87;
    if (typeof helper === 'function') {
      env.x87 = (...args: [number, bigint, number, bigint, bigint, bigint]) => {
        helperCalls++;
        return helper(...args);
      };
    }
    return env;
  };
  try {
    // FSTP ST(1); FSTP ST(0)
    await using(compiler, 'ddd9ddd8', context('protected'), async (cpu) => {
      const enter = (entry = pc, x87 = original) => {
        cpu.setState(input(entry, {}, 2n));
        cpu.setX87State(x87);
        helperCalls = 0;
      };
      enter();
      const paired = await cpu.run({ budget: 8n, stop: pc + 4n });
      assert.equal(paired.reason, 'stop');
      assert.equal(paired.retired, 2n);
      assert.equal(helperCalls, 0);
      assert.deepEqual(cpu.readX87State(), {
        ...original,
        registers: [rawTop, ...original.registers.slice(1)],
        statusWord: 0x4d00,
        tagWord: 0xffff,
        instructionPointer: pc + 2n,
      });

      enter(pc + 2n);
      const single = await cpu.run({ budget: 8n, stop: pc + 4n });
      assert.equal(single.reason, 'stop');
      assert.equal(single.retired, 1n);
      assert.equal(helperCalls, 0);
      assert.deepEqual(cpu.readX87State(), {
        ...original,
        statusWord: 0x4500,
        tagWord: 0xffff,
        instructionPointer: pc + 2n,
      });

      enter();
      const stopped = await cpu.run({ budget: 8n, stop: pc + 2n });
      assert.equal(stopped.reason, 'stop');
      assert.equal(stopped.retired, 1n);
      assert.equal(helperCalls, 1);
      assert.deepEqual(cpu.readX87State(), {
        ...original,
        registers: [rawTop, ...original.registers.slice(1)],
        statusWord: 0x4500,
        tagWord: 0xfffe,
        instructionPointer: pc,
      });

      const pending = {
        ...original,
        controlWord: original.controlWord & ~1,
        statusWord: original.statusWord | 0x8081,
      };
      enter(pc, pending);
      const trapped = await cpu.run({ budget: 8n, stop: pc + 4n });
      assert.equal(trapped.reason, 'x87-exception');
      assert.equal(trapped.retired, 0n);
      assert.equal(helperCalls, 2);
      assert.deepEqual(cpu.readX87State(), pending);
    });
  } finally {
    prototype.artifactImports = imports;
  }
});
