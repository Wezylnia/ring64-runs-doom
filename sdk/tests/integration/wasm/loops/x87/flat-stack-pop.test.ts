import assert from 'node:assert/strict';
import test from 'node:test';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import { context, input, using } from '../../../../../../tests/execution/modes/fixture.js';
import { compiler } from '../fixture.js';

test('inline x87 operations preserve raw bank bits and checked retirement boundaries', async () => {
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
  let helperCalls = 0;
  const restoreImports = observeArtifactImports((env) => {
    const helper = env.x87;
    if (typeof helper === 'function') {
      env.x87 = (...args: [number, bigint, number, bigint, bigint, bigint]) => {
        helperCalls++;
        return helper(...args);
      };
    }
  });
  try {
    // FXCH ST(1); FUCOMIP ST(1) swaps slots 7 and 0 before comparing.
    await using(compiler, 'd9c9dfe9', context('protected'), async (cpu) => {
      const left = 0x4000c000000000000000n;
      const right = 0x40008000000000000000n;
      const run = async (source: bigint, stop: bigint) => {
        const registers = [source, ...original.registers.slice(1, 7), left];
        const before = { ...original, registers, tagWord: 0xbffc };
        cpu.setState(input(pc, {}, 0xcd7n));
        cpu.setX87State(before);
        helperCalls = 0;
        const execution = await cpu.run({ budget: 8n, stop });
        return { before, execution };
      };
      for (const [source, calls] of [
        [right, 0],
        [0n, 1],
      ] as const) {
        const { before, execution } = await run(source, pc + 4n);
        assert.equal(execution.reason, 'stop');
        assert.equal(execution.retired, 2n);
        assert.equal(execution.state.flags, 0x403n);
        assert.equal(helperCalls, calls);
        assert.deepEqual(cpu.readX87State(), {
          ...before,
          registers: [left, ...before.registers.slice(1, 7), source],
          statusWord: 0x4500,
          tagWord: 0xfffc,
          instructionPointer: pc + 2n,
        });
      }
      const { before, execution } = await run(right, pc + 2n);
      assert.equal(execution.reason, 'stop');
      assert.equal(execution.retired, 1n);
      assert.equal(execution.state.flags, 0xcd7n);
      assert.equal(helperCalls, 1);
      assert.deepEqual(cpu.readX87State(), {
        ...before,
        registers: [left, ...before.registers.slice(1, 7), right],
        statusWord: 0x7d00,
        tagWord: 0x3ffc,
        instructionPointer: pc,
      });
    });

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
      assert.equal(helperCalls, 0);
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

    // FSUB ST(0), ST(1) with TOP=7 reads physical slot 0.
    await using(compiler, 'd8e1', context('protected'), async (cpu) => {
      const left = 0x4000c000000000000000n;
      const right = 0x40008000000000000000n;
      const one = 0x3fff8000000000000000n;
      const negativeZero = 0x80000000000000000000n;
      for (const [source, controlWord, expected, destinationTag, calls] of [
        [right, 0x37f, one, 0, 0],
        [left, 0x77f, negativeZero, 1, 0],
        [right, 0x27f, one, 0, 1],
      ] as const) {
        const registers = [source, ...original.registers.slice(1, 7), left];
        const before = { ...original, registers, controlWord, tagWord: 0xbffc };
        cpu.setState(input(pc, {}, 2n));
        cpu.setX87State(before);
        helperCalls = 0;
        const result = await cpu.run({ budget: 8n, stop: pc + 2n });
        assert.equal(result.reason, 'stop');
        assert.equal(result.retired, 1n);
        assert.equal(helperCalls, calls);
        assert.deepEqual(cpu.readX87State(), {
          ...before,
          registers: [...registers.slice(0, 7), expected],
          tagWord: 0x3ffc | (destinationTag << 14),
          statusWord: 0x7d00,
          instructionPointer: pc,
        });
      }
    });

    // FSTP ST(2) from TOP=7 writes physical slot 1. The imported source
    // tag is 0 for all three values, including zero and unsupported.
    await using(compiler, 'ddda', context('protected'), async (cpu) => {
      for (const [value, tag] of [
        [0x80000000000000000000n, 1],
        [0x40008000000000000000n, 0],
        [0x40000000000000000001n, 2],
      ] as const) {
        const registers = [...original.registers.slice(0, 7), value];
        cpu.setState(input(pc, {}, 2n));
        cpu.setX87State({ ...original, registers });
        helperCalls = 0;
        const run = await cpu.run({ budget: 8n, stop: pc + 2n });
        assert.equal(run.reason, 'stop');
        assert.equal(run.retired, 1n);
        assert.equal(helperCalls, 0);
        registers[1] = value;
        assert.deepEqual(cpu.readX87State(), {
          ...original,
          registers,
          statusWord: 0x4500,
          tagWord: 0xfff3 | (tag << 2),
          instructionPointer: pc,
        });
      }
    });

    // FLDZ wraps TOP=0 to slot 7 and overwrites its stale value.
    await using(compiler, 'd9ee', context('protected'), async (cpu) => {
      const before = { ...original, statusWord: 0x4700, tagWord: 0xffff };
      const run = async (state = before) => {
        cpu.setState(input(pc, {}, 2n));
        cpu.setX87State(state);
        helperCalls = 0;
        return cpu.run({ budget: 8n, stop: pc + 2n });
      };
      const pushed = await run();
      assert.equal(pushed.reason, 'stop');
      assert.equal(pushed.retired, 1n);
      assert.equal(helperCalls, 0);
      assert.deepEqual(cpu.readX87State(), {
        ...before,
        registers: [...before.registers.slice(0, 7), 0n],
        statusWord: 0x7d00,
        tagWord: 0x7fff,
        instructionPointer: pc,
      });

      const overflow = await run({ ...before, tagWord: 0x3fff });
      assert.equal(overflow.reason, 'stop');
      assert.equal(overflow.retired, 1n);
      assert.equal(helperCalls, 1);
      assert.deepEqual(cpu.readX87State(), {
        ...before,
        registers: [...before.registers.slice(0, 7), 0xffffc000000000000000n],
        statusWord: 0x7f41,
        tagWord: 0xbfff,
        instructionPointer: pc,
      });

      const pending = { ...before, controlWord: 0x37e, statusWord: 0xc781 };
      const trapped = await run(pending);
      assert.equal(trapped.reason, 'x87-exception');
      assert.equal(trapped.retired, 0n);
      assert.equal(helperCalls, 1);
      assert.deepEqual(cpu.readX87State(), pending);
    });
  } finally {
    restoreImports();
  }
});
