import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { snapshot } from '../../../../../tests/execution/far-control/fixture.js';
import { BANK, PC, initial, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('guarded scalar read prefixes preserve stop, single-step, and split-page fault ordering', async () => {
  const code = '0fb70783c00331c983c001'; // MOVZX EAX,word [RDI]; ADD EAX,3; XOR ECX,ECX; ADD EAX,1
  const stop = PC + BigInt(code.length / 2);
  const source = PC + 0x1001n;
  const data = new Uint8Array(4096);
  data.set([0x65, 0x87], 1);
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let accepted = 0;
  let refused = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const read = env.paged_loop_read_span;
    if (typeof read === 'function') {
      env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
        const result = read(address, width, segment);
        if (result) accepted++;
        else refused++;
        return result;
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      { code, words: [[0x4008n, 0xa003n]], spans: [{ physical: 0xa000n, bytes: data }] },
      async (cpu) => {
        const enter = (address = source, flags = 2n) =>
          cpu.setState(initial(PC, { 0: 0x1234n, 7: address }, flags));
        for (let wave = 0; wave < 3; wave++) {
          enter();
          const completed = await cpu.run({ budget: 10n, stop });
          assert.equal(completed.reason, 'stop');
          assert.equal(completed.retired, 4n);
          assert.equal(completed.state.registers[0], 0x8769n);
        }
        assert.ok(accepted > 0, 'the ordinary unaligned supervisor read uses a retained span');
        const beforeStop = accepted;
        enter();
        const interior = await cpu.run({ budget: 10n, stop: PC + 3n });
        assert.equal(interior.retired, 1n);
        assert.equal(interior.state.registers[0], 0x8765n);
        assert.equal(accepted, beforeStop);

        enter(source, 0x102n);
        const step = await cpu.run({ budget: 10n, stop });
        assert.equal(step.reason, 'debug-exception');
        assert.equal(step.retired, 1n);
        assert.equal(step.state.registers[0], 0x8765n);
        assert.equal(accepted, beforeStop);

        const beforeSplit = refused;
        enter(PC + 0x1fffn);
        const split = await cpu.run({ budget: 10n, stop });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.retired, 0n);
        assert.equal(split.state.rip, PC);
        assert.equal(split.state.registers[0], 0x1234n);
        assert.equal(refused, beforeSplit + 1);
      },
    );
    await usingPaged(
      compiler,
      {
        code,
        context: snapshot('long64', 3),
        bank: { ...BANK, cr0: BANK.cr0 | 0x40000n },
        words: [[0x4008n, 0xa007n]],
        spans: [{ physical: 0xa000n, bytes: data }],
      },
      async (cpu) => {
        cpu.setState(initial(PC, { 7: source + 1n }, 0x40002n));
        assert.equal((await cpu.run({ budget: 10n, stop })).reason, 'stop');
        const beforeAlignment = accepted + refused;
        cpu.setState(initial(PC, { 0: 0x1234n, 7: source }, 0x40002n));
        const alignment = await cpu.run({ budget: 10n, stop });
        assert.equal(alignment.reason, 'alignment-check');
        assert.equal(alignment.retired, 0n);
        assert.equal(alignment.state.rip, PC);
        assert.equal(alignment.state.registers[0], 0x1234n);
        assert.equal(accepted + refused, beforeAlignment);
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});
