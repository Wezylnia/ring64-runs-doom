import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { PC, initial, little, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('quiet paged x87 pairs preserve the observable first-instruction boundary', async () => {
  const address = PC + 0x2000n;
  const proto = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = proto.artifactImports;
  let accepted = 0;
  proto.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const helper = env.x87;
    if (typeof helper === 'function') {
      env.x87 = (...args: [number, bigint, number, bigint, bigint, bigint]) => {
        const result = helper(...args);
        if (args[2] >>> 29 === 1 && result === 0) accepted++;
        return result;
      };
    }
    return env;
  };
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
        const before = accepted;
        const warm = await run(PC + 4n);
        assert.equal(warm.reason, 'stop');
        assert.equal(warm.retired, 2n);
        assert.equal(accepted, before + 1);
        assert.deepEqual(cpu.readX87State(), expected);

        const stopped = await run(PC + 2n);
        assert.equal(stopped.reason, 'stop');
        assert.equal(stopped.retired, 1n);
        assert.equal(cpu.readX87State().instructionPointer, PC);

        const trapped = await run(PC + 4n, 0x102n);
        assert.equal(trapped.reason, 'debug-exception');
        assert.equal(trapped.retired, 1n);
        assert.equal(trapped.state.rip, PC + 2n);
        assert.equal(accepted, before + 1);
      },
    );
  } finally {
    proto.artifactImports = imports;
  }
});
