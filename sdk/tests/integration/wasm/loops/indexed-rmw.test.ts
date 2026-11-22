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

test('paged ADD and ROL reuse a writable and readable leaf and replay invalid accesses', async () => {
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  const add = PC + 2n;
  const stop = PC + 6n;
  const address = PC + 0x1040n;
  let reads = 0;
  let stores = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
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
    return env;
  };
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
    prototype.artifactImports = imports;
  }
});
