import assert from 'node:assert/strict';
import test from 'node:test';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import {
  PC,
  initial,
  little,
  usingPaged,
} from '../../../../../../tests/execution/memory/paging/fixture.js';
import { compiler } from '../fixture.js';

test('ordinary paged vector moves retain live read and write mappings and replay checked faults', async () => {
  // MOVDQU XMM0,[RDI]; MOVDQU [RSI],XMM0; HLT.
  const source = PC + 0x1011n;
  const destination = PC + 0x2013n;
  const value = Uint8Array.from({ length: 16 }, (_, index) => index + 1);
  const sourcePage = new Uint8Array(4096);
  sourcePage.set(value, 0x11);
  const calls = { vector_load: 0, vector_store: 0 };
  let finishes = 0;
  const restoreImports = observeArtifactImports((env) => {
    const finish = env.debug_finish;
    if (typeof finish === 'function') {
      env.debug_finish = (pc: bigint, ...args: unknown[]) => {
        if (pc === PC || pc === PC + 4n) finishes++;
        return finish(pc, ...args);
      };
    }
    for (const name of ['vector_load', 'vector_store'] as const) {
      const helper = env[name];
      if (typeof helper === 'function') {
        env[name] = (...args: unknown[]) => {
          calls[name]++;
          return helper(...args);
        };
      }
    }
  });
  try {
    await usingPaged(
      compiler,
      {
        code: 'f30f6f07f30f7f06f4',
        words: [
          [0x4008n, 0xa003n],
          [0x4010n, 0xb003n],
        ],
        spans: [
          { physical: 0xa000n, bytes: sourcePage },
          { physical: 0xb000n, bytes: new Uint8Array(4096) },
          { physical: 0xc000n, bytes: new Uint8Array(4096) },
        ],
      },
      async (cpu) => {
        const run = async (target = destination, from = source) => {
          cpu.setState(initial(PC, { 6: target, 7: from }));
          return cpu.run({ stop: PC + 8n, budget: 8n });
        };
        for (let wave = 0; wave < 4; wave++) {
          const result = await run();
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 2n);
          assert.deepEqual(cpu.readPhysicalMemory(0xb013n, 16), value);
        }
        assert.ok(calls.vector_load > 0 && calls.vector_store > 0);
        const admitted = { ...calls };
        const warmFinishes = finishes;
        assert.equal((await run()).reason, 'stop');
        assert.deepEqual(
          calls,
          admitted,
          'ordinary warm operands avoid both checked vector helpers',
        );

        assert.equal(finishes, warmFinishes, 'warm checked operands can complete quietly');

        cpu.writePhysicalMemory(0x4010n, little([0xc003n]));
        assert.equal((await run()).reason, 'stop');
        assert.deepEqual(cpu.readPhysicalMemory(0xc013n, 16), value);
        assert.ok(
          calls.vector_store > admitted.vector_store,
          'remapping invalidates the stored pointer',
        );

        assert.ok(finishes > warmFinishes, 'a remapped operand takes checked completion');

        const end = PC + 0x3000n;
        const split = await run(end - 8n);
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.pageFault?.address, end);
        assert.equal(split.retired, 1n);
        assert.equal(split.state.rip, PC + 4n);
        assert.deepEqual(cpu.readPhysicalMemory(0xcff8n, 8), new Uint8Array(8));

        const splitLoad = await run(destination, end - 8n);
        assert.equal(splitLoad.reason, 'page-fault');
        assert.equal(splitLoad.pageFault?.address, end);
        assert.equal(splitLoad.retired, 0n);
        assert.equal(splitLoad.state.rip, PC);

        cpu.setDebugState({ addresses: [destination, 0n, 0n, 0n], dr6: 0n, dr7: 0x10401n });
        const watched = await run();
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.debugException?.cause, 'data-breakpoint');
        assert.equal(watched.retired, 2n);
        assert.equal(watched.state.rip, PC + 8n);

        cpu.setDebugState({ addresses: [0n, 0n, 0n, 0n], dr6: 0n, dr7: 0n });
        const bank = cpu.readPagingState();
        cpu.setPagingState({ ...bank, cr0: bank.cr0 | 8n });
        const suspended = await run();
        assert.equal(suspended.reason, 'device-not-available');
        assert.equal(suspended.retired, 0n);
        assert.equal(suspended.state.rip, PC);
      },
    );
    for (const [prefix, offset] of [
      ['66', 0x10],
      ['f3', 0x11],
    ] as const) {
      // MOVDQA/MOVDQU XMM0,[RSP]; MOVDQA/MOVDQU [RBP],XMM0; HLT.
      const page = new Uint8Array(4096);
      page.set(value, offset);
      const src = PC + 0x1000n + BigInt(offset);
      const dst = PC + 0x2000n + BigInt(offset);
      await usingPaged(
        compiler,
        {
          code: `${prefix}0f6f0424${prefix}0f7f4500f4`,
          words: [
            [0x4008n, 0xa003n],
            [0x4010n, 0xb003n],
          ],
          spans: [
            { physical: 0xa000n, bytes: page },
            { physical: 0xb000n, bytes: new Uint8Array(4096) },
          ],
        },
        async (cpu) => {
          const run = async (from = src, target = dst) => {
            cpu.setState(initial(PC, { 4: from, 5: target }));
            return cpu.run({ stop: PC + 10n, budget: 8n, eventDelivery: { budget: 0n } });
          };
          for (let wave = 0; wave < 4; wave++) {
            const result = await run();
            assert.equal(result.reason, 'stop');
            assert.equal(result.retired, 2n);
          }
          assert.deepEqual(cpu.readPhysicalMemory(0xb000n + BigInt(offset), 16), value);
          const admitted = { ...calls };
          assert.equal((await run()).reason, 'stop');
          assert.deepEqual(calls, admitted, 'ordinary warm SS operands avoid both checked helpers');

          if (prefix === '66') {
            for (const [from, target, retired] of [
              [src + 1n, dst, 0n],
              [src, dst + 1n, 1n],
            ] as const) {
              const fault = await run(from, target);
              assert.equal(fault.reason, 'event-budget');
              assert.equal(fault.retired, retired);
              assert.equal(fault.state.rip, PC + 5n * retired);
              const event = fault.state.event;
              assert.equal(event.kind, 'pending');
              if (event.kind === 'pending') {
                assert.equal(event.exception.vector, 13);
                assert.equal(event.exception.errorCode, 0);
              }
              assert.deepEqual(cpu.readPhysicalMemory(0xb000n + BigInt(offset), 16), value);
            }
          }
        },
      );
    }
  } finally {
    restoreImports();
  }
});
