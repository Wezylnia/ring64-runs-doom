import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../../src/wasm/core.js';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import {
  BANK,
  PC,
  initial,
  little,
  usingPaged,
  word,
} from '../../../../../../tests/execution/memory/paging/fixture.js';
import { compiler } from '../fixture.js';

test('an indexed near call preserves checked fetch and writes in a guarded handler', async () => {
  // CALL RAX; INC ECX; CMP ECX,4; JB start; HLT.
  // Handler: MOV EDX,[RDI]; ADD EDX,1; MOV [RSI],EDX; MOV EDX,[RSI]; ADD EDX,1; RET.
  const target = PC + 16n;
  const returnPc = target + 12n;
  const stop = PC + 9n;
  const page = PC + 0x1000n;
  const rsp = page + 8n;
  const data = page + 0x40n;
  const compile = Core.prototype.compile;
  let handlerGuards = 0;
  let callBegins = 0;
  let guardedWrites = 0;
  let returnBegins = 0;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) return compile.call(this, pc, limits, [target, stop], false);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  const restoreImports = observeArtifactImports((env) => {
    const guard = env.paged_loop_guard;
    if (typeof guard === 'function') {
      env.paged_loop_guard = (pc: bigint, width: number, flags: bigint, shadow: number) => {
        if (pc === target) handlerGuards++;
        return guard(pc, width, flags, shadow);
      };
    }
    const begin = env.debug_begin;
    if (typeof begin === 'function') {
      env.debug_begin = (pc: bigint, flags: bigint, shadow: number) => {
        if (pc === returnPc) returnBegins++;
        if (pc === PC) callBegins++;
        return begin(pc, flags, shadow);
      };
    }
    const write = env.paged_loop_write_dword;
    if (typeof write === 'function') {
      env.paged_loop_write_dword = (address: bigint, value: number) => {
        if (address === data) guardedWrites++;
        return write(address, value);
      };
    }
  });
  try {
    await usingPaged(
      compiler,
      {
        code: 'ffd0ffc183f90472f7f49090909090908b1783c20189168b1683c201c3',
        bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
        words: [
          [0x4008n, 0xa003n],
          [0x4018n, 0xa065n],
        ],
        spans: [{ physical: 0xa000n, bytes: new Uint8Array(4096) }],
      },
      async (cpu) => {
        const enter = (store = data, source = data) =>
          cpu.setState(initial(PC, { 0: target, 1: 0n, 4: rsp, 6: store, 7: source }));
        for (let wave = 1; wave <= 3; wave++) {
          handlerGuards = 0;
          callBegins = 0;
          guardedWrites = 0;
          returnBegins = 0;
          enter();
          const result = await cpu.run({ stop, budget: 50n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 40n);
          assert.equal(result.state.registers[1], 4n);
          assert.equal(result.state.registers[2], BigInt(wave * 4 + 1));
          assert.equal(result.state.registers[4], rsp);
          assert.equal(word(cpu, 0xa040n), BigInt(wave * 4));
        }
        assert.equal(handlerGuards, 1, 'only the first handler needs checked admission');
        assert.equal(callBegins, 1, 'only the first call needs the checked begin');
        assert.equal(guardedWrites, 0, 'warm handler stores use the retained writable mapping');
        assert.equal(returnBegins, 0, 'warm returns reuse the admitted handler fetch');
        enter();
        const inside = await cpu.run({ stop: target + 2n, budget: 50n });
        assert.equal(inside.reason, 'stop');
        assert.equal(inside.retired, 2n);
        assert.equal(inside.state.rip, target + 2n);

        cpu.setDebugState({ addresses: [target, 0n, 0n, 0n], dr6: 0n, dr7: 1n });
        enter();
        const watched = await cpu.run({ stop, budget: 50n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.retired, 1n);
        assert.equal(watched.state.rip, target);

        cpu.setDebugState({ addresses: [data, 0n, 0n, 0n], dr6: 0n, dr7: 0xd0401n });
        enter();
        const writeWatch = await cpu.run({ stop, budget: 50n });
        assert.equal(writeWatch.reason, 'debug-exception');
        assert.equal(writeWatch.debugException?.cause, 'data-breakpoint');
        assert.equal(writeWatch.retired, 4n);
        assert.equal(writeWatch.state.rip, target + 7n);
        assert.equal(word(cpu, 0xa040n), 13n);

        cpu.setDebugState({ addresses: [0n, 0n, 0n, 0n], dr6: 0n, dr7: 0n });
        const readonly = data + 8192n;
        enter(readonly, readonly);
        const denied = await cpu.run({ stop, budget: 50n });
        assert.equal(denied.reason, 'page-fault');
        assert.equal(denied.retired, 3n);
        assert.equal(denied.state.rip, target + 5n);
        assert.equal(denied.state.registers[2], 14n);
        assert.equal(word(cpu, 0xa040n), 13n);

        const end = page + 4096n;
        enter(end - 2n);
        const split = await cpu.run({ stop, budget: 50n });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.pageFault?.address, end);
        assert.equal(split.retired, 3n);
        assert.equal(split.state.rip, target + 5n);
        assert.deepEqual(cpu.readPhysicalMemory(0xaffen, 2), Uint8Array.of(0, 0));

        cpu.setState(initial(returnPc, { 4: page }, 0x102n));
        returnBegins = 0;
        const stepped = await cpu.run({ stop, budget: 50n });
        assert.equal(stepped.reason, 'debug-exception');
        assert.equal(stepped.debugException?.cause, 'single-step');
        assert.equal(stepped.retired, 1n);
        assert.equal(stepped.state.rip, PC + 2n);
        assert.equal(stepped.state.registers[4], rsp);
        assert.equal(returnBegins, 1);

        cpu.setState(initial(returnPc, { 4: end - 4n }));
        const splitReturn = await cpu.run({ stop, budget: 50n });
        assert.equal(splitReturn.reason, 'page-fault');
        assert.equal(splitReturn.pageFault?.address, end);
        assert.equal(splitReturn.retired, 0n);
        assert.equal(splitReturn.state.rip, returnPc);
        assert.equal(splitReturn.state.registers[4], end - 4n);
      },
    );
  } finally {
    restoreImports();
    Core.prototype.compile = compile;
  }
});

test('near calls reuse the indexed stack write and replay observed faults', async () => {
  // CALL RAX; HLT; padding; INC RCX; RET.
  const target = PC + 8n;
  const resume = PC + 2n;
  const stack = PC + 0x1000n;
  const rsp = stack + 8n;
  const compile = Core.prototype.compile;
  let checked = 0;
  let callFinishes = 0;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) return compile.call(this, pc, limits, [target, resume], false);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  const restoreImports = observeArtifactImports((env) => {
    const store = env.store;
    if (typeof store === 'function') {
      env.store = (address: bigint, width: number, value: bigint) => {
        checked++;
        return store(address, width, value);
      };
    }
    const finish = env.debug_finish;
    if (typeof finish === 'function') {
      env.debug_finish = (pc: bigint, ...args: unknown[]) => {
        if (pc === PC) callFinishes++;
        return finish(pc, ...args);
      };
    }
  });
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
        assert.ok(callFinishes > 0, 'the cold call finishes through the checked boundary');
        callFinishes = 0;
        await call(0xa000n);
        assert.equal(checked, warmed, 'the warm call stores through the retained page');
        assert.equal(callFinishes, 0, 'a warm direct stack write keeps the quiet boundary');

        cpu.writePhysicalMemory(0x4008n, little([0xb003n]));
        await call(0xb000n);
        assert.ok(checked > warmed, 'a remap requires checked admission');
        assert.equal(callFinishes, 1, 'a checked stack write still finishes through the boundary');
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
        cpu.setState(initial(PC, { 0: target, 4: rsp }, 0x102n));
        callFinishes = 0;
        const stepped = await cpu.run({ stop: resume, budget: 10n });
        assert.equal(stepped.reason, 'debug-exception');
        assert.equal(stepped.debugException?.cause, 'single-step');
        assert.equal(stepped.retired, 1n);
        assert.equal(stepped.state.rip, target);
        assert.equal(callFinishes, 1);

        cpu.setDebugState({ addresses: [stack, 0n, 0n, 0n], dr6: 0n, dr7: 0x10401n });
        callFinishes = 0;
        const watch = await run();
        assert.equal(watch.reason, 'debug-exception');
        assert.equal(watch.debugException?.cause, 'data-breakpoint');
        assert.equal(watch.retired, 1n);
        assert.equal(watch.state.rip, target);
        assert.equal(watch.state.registers[4], stack);
        assert.equal(word(cpu, 0xb000n), resume);
        assert.equal(callFinishes, 1);
      },
    );
  } finally {
    restoreImports();
    Core.prototype.compile = compile;
  }
});

test('paged near transfers preserve the full target through dense owner dispatch', async () => {
  // CALL RAX; HLT; padding; INC RCX; RET.
  const resume = PC + 2n;
  const target = PC + 8n;
  const alias = target + (1n << 32n);
  const stack = PC + 0x1000n;
  const rsp = stack + 8n;
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) return compile.call(this, pc, limits, [target, resume]);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  try {
    await usingPaged(
      compiler,
      {
        code: 'ffd0f4909090909048ffc1c3',
        words: [[0x4008n, 0xa003n]],
        spans: [{ physical: 0xa000n, bytes: new Uint8Array(4096) }],
      },
      async (cpu) => {
        const enter = (rax: bigint) => cpu.setState(initial(PC, { 0: rax, 1: 5n, 4: rsp }));
        enter(target);
        const returned = await cpu.run({ stop: resume, budget: 8n });
        assert.equal(returned.reason, 'stop');
        assert.equal(returned.retired, 3n);
        assert.equal(returned.state.registers[1], 6n);
        assert.equal(returned.state.registers[4], rsp);
        assert.equal(returned.resources.nativeEntries, 1);

        for (const budget of [1n, 8n]) {
          enter(alias);
          const stopped = await cpu.run({ stop: alias, budget });
          assert.equal(stopped.reason, budget === 1n ? 'budget' : 'stop');
          assert.equal(stopped.retired, 1n);
          assert.equal(stopped.state.rip, alias);
          assert.equal(stopped.state.registers[4], stack);
          assert.equal(word(cpu, 0xa000n), resume);
        }

        enter(alias);
        const fault = await cpu.run({ budget: 8n });
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.retired, 1n);
        assert.equal(fault.state.rip, alias);
        assert.equal(fault.pageFault?.address, alias);
        assert.equal(fault.state.registers[1], 5n);

        enter(0x8000_0000_0000n);
        const invalid = await cpu.run({ budget: 8n, eventDelivery: { budget: 0n } });
        assert.equal(invalid.reason, 'event-budget');
        assert.equal(invalid.retired, 0n);
        assert.equal(invalid.state.rip, PC);
        assert.equal(invalid.state.registers[4], rsp);
        assert.equal(invalid.state.event.kind, 'pending');
        assert.equal(invalid.state.event.exception.vector, 13);
      },
    );
  } finally {
    Core.prototype.compile = compile;
  }
});
