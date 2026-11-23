import assert from 'node:assert/strict';
import test from 'node:test';
import { observeArtifactImports } from '../../../../../tests/support/artifact-imports.js';
import {
  BANK,
  PC,
  initial,
  little,
  usingPaged,
} from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

function observePairs(discriminator: number) {
  let accepted = 0;
  const restoreImports = observeArtifactImports((env) => {
    const helper = env.x87;
    if (typeof helper === 'function') {
      env.x87 = (...args: [number, bigint, number, bigint, bigint, bigint]) => {
        const result = helper(...args);
        if (args[2] >>> 28 === discriminator && result === 0) accepted++;
        return result;
      };
    }
  });
  return {
    count: () => accepted,
    restore: () => {
      restoreImports();
    },
  };
}

test('quiet paged x87 pairs preserve the observable first-instruction boundary', async () => {
  const address = PC + 0x2000n;
  const pairs = observePairs(2);
  try {
    await usingPaged(
      compiler,
      {
        // FLD m64 [RBX]; FLD ST(0)
        code: 'dd03d9c0',
        words: [[0x4010n, 0xa007n]],
        spans: [{ physical: 0xa000n, bytes: little([0x3ff0000000000000n]) }],
      },
      async (cpu) => {
        const run = (stop: bigint, flags = 2n) => {
          cpu.setState(initial(PC, { 3: address }, flags));
          cpu.setX87State({
            registers: Array<bigint>(8).fill(0n),
            controlWord: 0x37f,
            statusWord: 0,
            tagWord: 0xffff,
            opcode: 0,
            instructionPointer: 0n,
            dataPointer: 0n,
          });
          return cpu.run({ budget: 8n, stop });
        };

        const cold = await run(PC + 4n);
        assert.equal(cold.reason, 'stop');
        assert.equal(cold.retired, 2n);
        const expected = cpu.readX87State();
        const before = pairs.count();
        const warm = await run(PC + 4n);
        assert.equal(warm.reason, 'stop');
        assert.equal(warm.retired, 2n);
        assert.equal(pairs.count(), before + 1);
        assert.deepEqual(cpu.readX87State(), expected);

        const stopped = await run(PC + 2n);
        assert.equal(stopped.reason, 'stop');
        assert.equal(stopped.retired, 1n);
        assert.equal(cpu.readX87State().instructionPointer, PC);

        const trapped = await run(PC + 4n, 0x102n);
        assert.equal(trapped.reason, 'debug-exception');
        assert.equal(trapped.retired, 1n);
        assert.equal(trapped.state.rip, PC + 2n);
        assert.equal(pairs.count(), before + 1);
      },
    );
  } finally {
    pairs.restore();
  }
});

test('quiet paged x87 store pairs preserve the arithmetic at a store fault or stop', async () => {
  const source = PC + 0x2000n;
  const writable = PC + 0x3000n;
  const readOnly = PC + 0x4000n;
  const unchanged = little([0x1122334455667788n]);
  const one = little([0x3ff0000000000000n]);
  const converted = little([0x4000000000000000n]);
  const pairs = observePairs(1);
  try {
    await usingPaged(
      compiler,
      {
        // FDIVR m64 [RBX]; FSTP m64 [RCX]
        code: 'dc3bdd19',
        bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
        words: [
          [0x4010n, 0xa007n],
          [0x4018n, 0xb007n],
          [0x4020n, 0xc005n],
        ],
        spans: [
          { physical: 0xa000n, bytes: converted },
          { physical: 0xb000n, bytes: unchanged },
          { physical: 0xc000n, bytes: unchanged },
        ],
      },
      async (cpu) => {
        const run = (
          destination: bigint,
          stop = PC + 4n,
          denominator = 0x3fff8000000000000000n,
        ) => {
          cpu.setState(initial(PC, { 1: destination, 3: source }));
          cpu.setX87State({
            registers: [denominator, ...Array<bigint>(7).fill(0n)],
            controlWord: 0x37f,
            statusWord: 0,
            tagWord: 0xfffc,
            opcode: 0,
            instructionPointer: 0n,
            dataPointer: 0n,
          });
          return cpu.run({ budget: 8n, stop });
        };

        const cold = await run(writable, PC + 4n, 0x40008000000000000000n);
        assert.equal(cold.reason, 'stop');
        assert.equal(cold.retired, 2n);
        assert.equal(pairs.count(), 0);
        assert.deepEqual(cpu.readPhysicalMemory(0xb000n, 8), one);
        const stored = cpu.readX87State();
        assert.equal(stored.instructionPointer, PC + 2n);
        assert.equal(stored.dataPointer, writable);

        const warm = await run(writable);
        assert.equal(warm.reason, 'stop');
        assert.equal(warm.retired, 2n);
        assert.equal(pairs.count(), 1);
        assert.deepEqual(cpu.readPhysicalMemory(0xb000n, 8), converted);
        assert.equal(cpu.readX87State().instructionPointer, stored.instructionPointer);
        assert.equal(cpu.readX87State().dataPointer, stored.dataPointer);

        const stopped = await run(writable, PC + 2n, 0x40008000000000000000n);
        assert.equal(stopped.reason, 'stop');
        assert.equal(stopped.retired, 1n);
        assert.deepEqual(cpu.readPhysicalMemory(0xb000n, 8), converted);
        const arithmetic = cpu.readX87State();
        assert.equal(arithmetic.instructionPointer, PC);
        assert.equal(arithmetic.dataPointer, source);
        assert.equal(arithmetic.registers[0], 0x3fff8000000000000000n);

        const fault = await run(readOnly, PC + 4n, 0x40008000000000000000n);
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.retired, 1n);
        assert.equal(fault.state.rip, PC + 2n);
        assert.deepEqual(cpu.readPhysicalMemory(0xc000n, 8), unchanged);
        assert.deepEqual(cpu.readX87State(), arithmetic);
        assert.equal(pairs.count(), 1);
      },
    );
  } finally {
    pairs.restore();
  }
});
