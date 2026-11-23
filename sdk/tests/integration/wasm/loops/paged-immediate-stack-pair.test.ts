import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import {
  BANK,
  PC,
  initial,
  usingPaged,
  word,
} from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('sampled immediate push and pop preserve the stack store and precise intervening boundaries', async () => {
  const page = PC + 0x1000n;
  const sp = page + 0x40n;
  const push = PC + 12n;
  const pop = push + 2n;
  const stop = pop + 1n;
  const afterSuccessor = stop + 3n;
  const compile = Core.prototype.compile;
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let begins = 0;
  let successorBegins = 0;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) {
      return compile.call(
        this,
        pc,
        limits,
        [PC, PC + 3n, PC + 6n, PC + 9n, push, pop, stop, afterSuccessor],
        true,
      );
    }
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const begin = env.debug_begin;
    if (typeof begin === 'function') {
      env.debug_begin = (pc: bigint, ...args: unknown[]) => {
        if (pc === push || pc === pop) begins++;
        if (pc === stop) successorBegins++;
        return begin(pc, ...args);
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        // CMP RDX,[RDI]; INC RDX (three times); PUSH -1; POP RCX; INC RCX; HLT.
        code: '483b1748ffc248ffc248ffc26aff5948ffc1f4',
        bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
        words: [[0x4008n, 0xa003n]],
        spans: [{ physical: 0xa000n, bytes: new Uint8Array(4096) }],
      },
      async (cpu) => {
        const enter = (stack = sp) => cpu.setState(initial(PC, { 1: 3n, 4: stack, 7: page }));
        const saved = 0xffff_ffff_ffff_ffffn;
        for (let wave = 0; wave < 4; wave++) {
          begins = 0;
          enter();
          const run = await cpu.run({ stop, budget: 12n });
          assert.equal(run.reason, 'stop');
          assert.equal(run.retired, 6n);
          assert.equal(run.state.registers[1], saved);
          assert.equal(run.state.registers[4], sp);
          assert.equal(word(cpu, 0xa038n), saved);
        }
        assert.equal(begins, 0, 'the warm pair uses its readable and writable stack authority');

        enter();
        successorBegins = 0;
        const successor = await cpu.run({ stop: afterSuccessor, budget: 12n });
        assert.equal(successor.reason, 'stop');
        assert.equal(successor.retired, 7n);
        assert.equal(successor.state.registers[1], 0n);
        assert.equal(successorBegins, 0, 'the pair retains the admitted successor fetch');

        cpu.setDebugState({ addresses: [stop, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
        enter();
        const breakpoint = await cpu.run({ stop: afterSuccessor, budget: 12n });
        assert.equal(breakpoint.reason, 'debug-exception');
        assert.equal(breakpoint.retired, 6n);
        assert.equal(breakpoint.state.rip, stop);
        assert.equal(breakpoint.state.registers[1], saved);
        cpu.setDebugState({ addresses: [0n, 0n, 0n, 0n], dr6: 0n, dr7: 0n });

        for (const [limits, reason] of [
          [{ stop: pop, budget: 12n }, 'stop'],
          [{ stop, budget: 5n }, 'budget'],
        ] as const) {
          enter();
          const run = await cpu.run(limits);
          assert.equal(run.reason, reason);
          assert.equal(run.retired, 5n);
          assert.equal(run.state.rip, pop);
          assert.equal(run.state.registers[1], 3n);
          assert.equal(run.state.registers[4], sp - 8n);
        }

        cpu.setDebugState({ addresses: [sp - 8n, 0n, 0n, 0n], dr6: 0n, dr7: 0x10401n });
        enter();
        const watched = await cpu.run({ stop, budget: 12n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.retired, 5n);
        assert.equal(watched.state.rip, pop);
        assert.equal(watched.state.registers[4], sp - 8n);
        assert.equal(watched.state.registers[1], 3n);

        cpu.setDebugState({ addresses: [0n, 0n, 0n, 0n], dr6: 0n, dr7: 0n });
        enter(page + 4096n + 4n);
        const split = await cpu.run({ stop, budget: 12n });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.retired, 4n);
        assert.equal(split.state.rip, push);
        assert.equal(split.pageFault?.address, page + 4096n);
        assert.equal(split.state.registers[4], page + 4096n + 4n);
      },
    );
  } finally {
    Core.prototype.compile = compile;
    prototype.artifactImports = imports;
  }
});
