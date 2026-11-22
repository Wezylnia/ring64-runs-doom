import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import {
  BANK,
  PC,
  initial,
  little,
  usingPaged,
  word,
} from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('an indexed near call preserves checked handler fetch across stack hits', async () => {
  // CALL RAX; INC ECX; CMP ECX,4; JB start; HLT; handler resets EDX and adds 3.
  const target = PC + 16n;
  const stop = PC + 9n;
  const rsp = PC + 0x1008n;
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const compile = Core.prototype.compile;
  let handlerGuards = 0;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) return compile.call(this, pc, limits, [target, stop], false);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const guard = env.paged_loop_guard;
    if (typeof guard === 'function') {
      env.paged_loop_guard = (pc: bigint, width: number, flags: bigint, shadow: number) => {
        if (pc === target) handlerGuards++;
        return guard(pc, width, flags, shadow);
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        code: 'ffd0ffc183f90472f7f490909090909031d283c20183c20183c201c3',
        words: [[0x4008n, 0xa003n]],
        spans: [{ physical: 0xa000n, bytes: new Uint8Array(4096) }],
      },
      async (cpu) => {
        const enter = () => cpu.setState(initial(PC, { 0: target, 1: 0n, 4: rsp }));
        for (let wave = 0; wave < 3; wave++) {
          handlerGuards = 0;
          enter();
          const result = await cpu.run({ stop, budget: 40n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 36n);
          assert.equal(result.state.registers[1], 4n);
          assert.equal(result.state.registers[2], 3n);
          assert.equal(result.state.registers[4], rsp);
        }
        assert.equal(handlerGuards, 1, 'only the first handler needs checked admission');
        enter();
        const inside = await cpu.run({ stop: target + 2n, budget: 40n });
        assert.equal(inside.reason, 'stop');
        assert.equal(inside.retired, 2n);
        assert.equal(inside.state.rip, target + 2n);

        cpu.setDebugState({ addresses: [target, 0n, 0n, 0n], dr6: 0n, dr7: 1n });
        enter();
        const watched = await cpu.run({ stop, budget: 40n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.retired, 1n);
        assert.equal(watched.state.rip, target);
      },
    );
  } finally {
    prototype.artifactImports = imports;
    Core.prototype.compile = compile;
  }
});

test('near calls reuse the indexed stack write and replay observed faults', async () => {
  // CALL RAX; HLT; padding; INC RCX; RET.
  const target = PC + 8n;
  const resume = PC + 2n;
  const stack = PC + 0x1000n;
  const rsp = stack + 8n;
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const compile = Core.prototype.compile;
  let checked = 0;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) return compile.call(this, pc, limits, [target, resume], false);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const store = env.store;
    if (typeof store === 'function') {
      env.store = (address: bigint, width: number, value: bigint) => {
        checked++;
        return store(address, width, value);
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        code: 'ffd0f4909090909048ffc1c3',
        bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
        words: [[0x4008n, 0xa003n]],
        spans: [
          { physical: 0xa000n, bytes: new Uint8Array(4096) },
          { physical: 0xb000n, bytes: new Uint8Array(4096) },
        ],
      },
      async (cpu) => {
        const run = async (sp = rsp) => {
          cpu.setState(initial(PC, { 0: target, 1: 5n, 4: sp }));
          return cpu.run({ stop: resume, budget: 10n });
        };
        const call = async (physical: bigint) => {
          const result = await run();
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 3n);
          assert.equal(result.state.registers[1], 6n);
          assert.equal(result.state.registers[4], rsp);
          assert.equal(word(cpu, physical), resume);
        };
        for (let run = 0; run < 4; run++) await call(0xa000n);
        assert.equal(word(cpu, 0x4008n) & 0x60n, 0x60n);
        const warmed = checked;
        assert.ok(warmed > 0, 'the cold call checks its stack store');
        await call(0xa000n);
        assert.equal(checked, warmed, 'the warm call stores through the retained page');

        cpu.writePhysicalMemory(0x4008n, little([0xb003n]));
        await call(0xb000n);
        assert.ok(checked > warmed, 'a remap requires checked admission');
        cpu.writePhysicalMemory(0xa000n, little([0x1234n]));

        cpu.writePhysicalMemory(0x4008n, little([0xa001n]));
        const denied = await run();
        assert.equal(denied.reason, 'page-fault');
        assert.equal(denied.pageFault?.address, stack);
        assert.equal(denied.retired, 0n);
        assert.equal(denied.state.rip, PC);
        assert.equal(denied.state.registers[4], rsp);
        assert.equal(word(cpu, 0xa000n), 0x1234n);

        cpu.writePhysicalMemory(0x4008n, little([0xb003n]));
        await call(0xb000n);
        const end = stack + 4096n;
        cpu.writePhysicalMemory(0xbff8n, little([0x1122334455667788n]));
        const split = await run(end + 4n);
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.pageFault?.address, end);
        assert.equal(split.retired, 0n);
        assert.equal(split.state.registers[4], end + 4n);
        assert.equal(word(cpu, 0xbff8n), 0x1122334455667788n);

        const noncanonicalStack = 0x800000000008n;
        cpu.setState(initial(PC, { 0: target, 4: noncanonicalStack }));
        const noncanonical = await cpu.run({
          stop: resume,
          budget: 10n,
          eventDelivery: { budget: 0n },
        });
        assert.equal(noncanonical.reason, 'event-budget');
        assert.equal(noncanonical.retired, 0n);
        assert.equal(noncanonical.state.rip, PC);
        assert.equal(noncanonical.state.registers[4], noncanonicalStack);
        assert.equal(noncanonical.state.event.kind, 'pending');
        assert.equal(noncanonical.state.event.exception.vector, 12);

        await call(0xb000n);
        await call(0xb000n);
        cpu.setDebugState({ addresses: [stack, 0n, 0n, 0n], dr6: 0n, dr7: 0x10401n });
        const watch = await run();
        assert.equal(watch.reason, 'debug-exception');
        assert.equal(watch.debugException?.cause, 'data-breakpoint');
        assert.equal(watch.retired, 1n);
        assert.equal(watch.state.rip, target);
        assert.equal(watch.state.registers[4], stack);
        assert.equal(word(cpu, 0xb000n), resume);
      },
    );
  } finally {
    prototype.artifactImports = imports;
    Core.prototype.compile = compile;
  }
});
