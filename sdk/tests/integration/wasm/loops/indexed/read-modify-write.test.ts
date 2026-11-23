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

test('paged ADD and ROL reuse a writable and readable leaf and replay invalid accesses', async () => {
  const add = PC + 2n;
  const stop = PC + 6n;
  const address = PC + 0x1040n;
  let reads = 0;
  let stores = 0;
  const restoreImports = observeArtifactImports((env) => {
    const load = env.load_for_write;
    if (typeof load === 'function') {
      env.load_for_write = (target: bigint, width: number) => {
        reads++;
        return load(target, width);
      };
    }
    const store = env.store;
    if (typeof store === 'function') {
      env.store = (target: bigint, width: number, value: bigint) => {
        stores++;
        return store(target, width, value);
      };
    }
  });
  try {
    await usingPaged(
      compiler,
      {
        // MOV [RDI], EAX; ADD [RDI], EAX; ROL dword [RDI], CL; HLT; ADD [RSP], EAX; HLT.
        code: '89070107d307f4010424f4',
        bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
        words: [
          [0x4008n, 0xa003n],
          [0x4018n, 0xb003n],
        ],
        spans: [
          { physical: 0xa000n, bytes: new Uint8Array(4096) },
          { physical: 0xb000n, bytes: new Uint8Array(4096), read: false },
        ],
      },
      async (cpu) => {
        const enter = (pc = add, target = address) =>
          cpu.setState(initial(pc, { 0: 1n, 1: 1n, 7: target }));
        let expected = 0n;
        for (let wave = 0; wave < 4; wave++) {
          enter();
          const result = await cpu.run({ stop, budget: 10n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 2n);
          expected = (expected + 1n) * 2n;
          assert.equal(word(cpu, 0xa040n), expected);
        }
        const before = [reads, stores];
        assert.ok(
          before.every((count) => count > 0),
          'the first RMW must establish admission',
        );
        enter();
        assert.equal((await cpu.run({ stop, budget: 10n })).retired, 2n);
        assert.deepEqual([reads, stores], before, 'warm RMW bypasses both Core helpers');
        expected = (expected + 1n) * 2n;
        assert.equal(word(cpu, 0xa040n), expected);
        assert.equal(word(cpu, 0x4008n) & 0x60n, 0x60n);

        const split = PC + 0x1ffen;
        cpu.writePhysicalMemory(0xaff8n, little([0xaabbccddeeff1122n]));
        enter(add, split);
        const fault = await cpu.run({ stop, budget: 10n });
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.pageFault?.address, PC + 0x2000n);
        assert.equal(fault.retired, 0n);
        assert.equal(word(cpu, 0xaff8n), 0xaabbccddeeff1122n);

        const writeOnly = PC + 0x3040n;
        enter(PC, writeOnly);
        assert.equal((await cpu.run({ stop: add, budget: 10n })).retired, 1n);
        const prior = [reads, stores] as const;
        enter(add, writeOnly);
        const denied = await cpu.run({ stop, budget: 10n });
        assert.equal(denied.reason, 'physical-memory-fault');
        assert.equal(denied.retired, 0n);
        assert.deepEqual([reads, stores], [prior[0] + 1, prior[1]]);

        cpu.writePhysicalMemory(0x4008n, little([0xa001n]));
        enter();
        const readOnly = await cpu.run({ stop, budget: 10n });
        assert.equal(readOnly.reason, 'page-fault');
        assert.equal(readOnly.pageFault?.address, address);
        assert.equal(readOnly.retired, 0n);
        assert.equal(word(cpu, 0xa040n), expected);
        cpu.writePhysicalMemory(0x4008n, little([0xa003n]));

        const stackAdd = stop + 1n;
        cpu.setState(initial(stackAdd, { 0: 1n, 4: 0x800000000000n }));
        const stackFault = await cpu.run({ budget: 10n, eventDelivery: { budget: 0n } });
        assert.equal(stackFault.reason, 'event-budget');
        assert.equal(stackFault.retired, 0n);
        assert.equal(stackFault.state.rip, stackAdd);
        assert.equal(stackFault.state.event.kind, 'pending');
        assert.equal(stackFault.state.event.exception.vector, 12);

        enter();
        cpu.setDebugState({ addresses: [address, 0n, 0n, 0n], dr6: 0n, dr7: 0xd0401n });
        const watched = await cpu.run({ stop, budget: 10n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.state.rip, add + 2n);
        assert.equal(watched.retired, 1n);
        assert.equal(word(cpu, 0xa040n), expected + 1n);
      },
    );
  } finally {
    restoreImports();
  }
});

test('guarded handler ADD and ROL keep precise RMW boundaries', async () => {
  // CALL RAX; INC ECX; CMP ECX,4; JB start; HLT.
  // Handler: MOV EDX,[RDI]; ADD EDX,1 (three times); ADD [RSI],EDX; ROL [RSI],1; RET.
  const code = 'ffd0ffc183f90472f7f49090909090908b1783c20183c20183c2010116d106c3';
  const handler = PC + 16n;
  const add = handler + 11n;
  const rotate = add + 2n;
  const stop = PC + 9n;
  const stack = PC + 0x1000n;
  const data = stack + 0x40n;
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) return compile.call(this, pc, limits, [handler, stop], false);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  let begins = 0;
  const restoreImports = observeArtifactImports((env) => {
    const begin = env.debug_begin;
    if (typeof begin === 'function') {
      env.debug_begin = (pc: bigint, flags: bigint, shadow: number) => {
        if (pc === add || pc === rotate) begins++;
        return begin(pc, flags, shadow);
      };
    }
  });
  try {
    await usingPaged(
      compiler,
      {
        code,
        words: [[0x4008n, 0xa003n]],
        spans: [{ physical: 0xa000n, bytes: new Uint8Array(4096) }],
      },
      async (cpu) => {
        const enter = (destination = data) =>
          cpu.setState(initial(PC, { 0: handler, 1: 0n, 4: stack + 8n, 6: destination, 7: data }));
        let expected = 0n;
        for (let wave = 0; wave < 3; wave++) {
          begins = 0;
          enter();
          const result = await cpu.run({ stop, budget: 64n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 44n);
          for (let lap = 0; lap < 4; lap++) expected = expected * 4n + 6n;
          assert.equal(word(cpu, 0xa040n), expected);
        }
        assert.equal(begins, 0, 'warm RMWs reuse the checked handler fetch');

        const end = stack + 4096n;
        enter(end - 2n);
        const split = await cpu.run({ stop, budget: 64n });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.pageFault?.address, end);
        assert.equal(split.retired, 5n);
        assert.equal(split.state.rip, add);
        assert.equal(split.state.registers[2], expected + 3n);
        assert.deepEqual(cpu.readPhysicalMemory(0xaffen, 2), Uint8Array.of(0, 0));
        assert.equal(begins, 1);

        begins = 0;
        cpu.setDebugState({ addresses: [data, 0n, 0n, 0n], dr6: 0n, dr7: 0xd0401n });
        enter();
        const watched = await cpu.run({ stop, budget: 64n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.debugException?.cause, 'data-breakpoint');
        assert.equal(watched.state.rip, rotate);
        assert.equal(watched.retired, 6n);
        assert.equal(word(cpu, 0xa040n), expected * 2n + 3n);
        assert.equal(begins, 1);
      },
    );
  } finally {
    restoreImports();
    Core.prototype.compile = compile;
  }
});
