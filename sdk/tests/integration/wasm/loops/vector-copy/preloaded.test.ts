import assert from 'node:assert/strict';
import test from 'node:test';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import { snapshot } from '../../../../../../tests/execution/far-control/fixture.js';
import {
  BANK,
  PC,
  initial,
  little,
  usingPaged,
} from '../../../../../../tests/execution/paging/fixture.js';
import { compiler } from '../fixture.js';

test('a stack-bounded paged vector loop reuses its proved window and keeps precise replay', async () => {
  // Two loads, two stores, ADD RAX,32; CMP [RSP],RAX; JNE back; HLT.
  const code = '0f1004070f104c07100f1104060f114c06104883c0204839042475e4f4';
  const stop = PC + 28n;
  const source = PC + 0x1000n;
  const destination = PC + 0x2000n;
  const stack = PC + 0x3000n;
  const bytes = new Uint8Array(4096);
  for (let i = 0; i < 96; i++) bytes[i] = i * 5;
  bytes.set(little([64n]), 32);
  let stackReads = 0;
  let checkedCompares = 0;
  const restoreImports = observeArtifactImports((env) => {
    const read = env.paged_loop_read_span;
    if (typeof read === 'function') {
      env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
        if (address === stack && width === 8) stackReads++;
        return read(address, width, segment);
      };
    }
    const begin = env.debug_begin;
    if (typeof begin === 'function') {
      env.debug_begin = (pc: bigint, ...args: unknown[]) => {
        if (pc === PC + 22n) checkedCompares++;
        return begin(pc, ...args);
      };
    }
  });
  try {
    await usingPaged(
      compiler,
      {
        code,
        words: [
          [0x4008n, 0xa063n],
          [0x4010n, 0xb063n],
          [0x4018n, 0xc063n],
        ],
        spans: [
          { physical: 0xa000n, bytes },
          { physical: 0xb000n, bytes: new Uint8Array(4096) },
          { physical: 0xc000n, bytes: little([96n]) },
        ],
      },
      async (cpu) => {
        const enter = (sp = stack) =>
          cpu.setState(initial(PC, { 0: 0n, 4: sp, 6: destination, 7: source }));
        for (let wave = 0; wave < 4; wave++) {
          enter();
          stackReads = 0;
          checkedCompares = 0;
          const copied = await cpu.run({ stop, budget: 32n });
          assert.equal(copied.reason, 'stop');
          assert.equal(copied.retired, 21n);
          assert.equal(copied.state.registers[0], 96n);
          assert.deepEqual(cpu.readPhysicalMemory(0xb000n, 96), bytes.subarray(0, 96));
        }
        assert.equal(stackReads, 1, 'one stack admission covers all three laps');
        assert.equal(checkedCompares, 0, 'the warmed comparison stays within the loop');

        cpu.writePhysicalMemory(0xb020n, little([96n]));
        enter(destination + 32n);
        const alias = await cpu.run({ stop, budget: 32n });
        assert.equal(alias.reason, 'stop');
        assert.equal(alias.retired, 14n);
        assert.equal(alias.state.registers[0], 64n);

        enter();
        const partial = await cpu.run({ stop, budget: 8n });
        assert.equal(partial.reason, 'budget');
        assert.equal(partial.retired, 8n);
        assert.equal(partial.state.rip, PC + 4n);

        enter();
        const interior = await cpu.run({ stop: PC + 22n, budget: 32n });
        assert.equal(interior.reason, 'stop');
        assert.equal(interior.retired, 5n);
        assert.equal(interior.state.rip, PC + 22n);
        assert.equal(interior.state.registers[0], 32n);

        enter(PC + 0x5000n);
        const fault = await cpu.run({ stop, budget: 32n });
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.retired, 5n);
        assert.equal(fault.state.rip, PC + 22n);
        assert.equal(fault.state.registers[0], 32n);
      },
    );
    const stackPage = new Uint8Array(4096);
    stackPage.set(little([96n]));
    await usingPaged(
      compiler,
      {
        code,
        context: snapshot('long64', 3),
        bank: { ...BANK, cr0: BANK.cr0 | 0x40000n },
        words: [
          [0x4008n, 0xa067n],
          [0x4010n, 0xb067n],
          [0x4018n, 0xc067n],
        ],
        spans: [
          { physical: 0xa000n, bytes },
          { physical: 0xb000n, bytes: new Uint8Array(4096) },
          { physical: 0xc000n, bytes: stackPage },
        ],
      },
      async (cpu) => {
        const enter = (sp = stack) =>
          cpu.setState(initial(PC, { 0: 0n, 4: sp, 6: destination, 7: source }, 0x40002n));
        enter();
        assert.equal((await cpu.run({ stop, budget: 32n })).reason, 'stop');
        enter(stack + 1n);
        const fault = await cpu.run({ stop, budget: 32n });
        assert.equal(fault.reason, 'alignment-check');
        assert.equal(fault.retired, 5n);
        assert.equal(fault.state.rip, PC + 22n);
      },
    );
  } finally {
    restoreImports();
  }
});
