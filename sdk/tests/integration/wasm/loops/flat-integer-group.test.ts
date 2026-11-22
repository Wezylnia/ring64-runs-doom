import assert from 'node:assert/strict';
import test from 'node:test';
import { createCpu } from '../../../../src/index.js';
import { Core } from '../../../../src/wasm/core.js';
import { protectedSnapshot } from '../../../../../tests/support/context.js';
import { LIMITS, hex, initial } from '../../../../../tests/support/fixture.js';
import { compiler } from './fixture.js';

test('flat sampled integer groups preserve precise budgets, stops, and interior entries', async () => {
  const head = 0x31000n;
  const stack = 0x32000n;
  // ADD EAX,1; XOR EAX,2; ADD EAX,4; CALL EBX; DEC EDX; JNZ head; HLT; RET;
  // MOV AL,0xF4; HLT. The MOV immediate also decodes as HLT at the interior entry.
  const code = hex('83c00183f00283c004ffd34a75f2f4c3b0f4f4');
  const seeds = [0n, 3n, 6n, 9n, 11n, 12n, 14n, 15n, 16n, 18n].map((offset) => head + offset);
  const mapping = {
    base: head,
    backing: 'code',
    offset: 0,
    length: code.length,
    permissions: { read: true, write: false, execute: true },
  };
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, entries, regionPoints) {
    if (pc === head && !entries?.length) return compile.call(this, pc, limits, seeds, true);
    return compile.call(this, pc, limits, entries, regionPoints);
  };
  const options = {
    compiler,
    entry: head,
    limits: LIMITS,
    context: protectedSnapshot(),
    memory: {
      backings: [
        { id: 'code', bytes: code },
        { id: 'stack', bytes: new Uint8Array(16) },
      ],
      mappings: [
        mapping,
        {
          base: stack,
          backing: 'stack',
          offset: 0,
          length: 16,
          permissions: { read: true, write: true, execute: false },
        },
      ],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: code.length + 16 },
  };
  try {
    const cpu = await createCpu(options);
    let groupedBytes: number;
    try {
      for (const [entry, input, budget, stop, reason, retired, rip, eax, flags] of [
        [0n, 2n, 8n, 9n, 'stop', 3n, 9n, 5n, 6n],
        [0n, 2n, 8n, 3n, 'stop', 1n, 3n, 3n, 6n],
        [0n, 2n, 2n, 9n, 'budget', 2n, 6n, 1n, 2n],
        [0n, 2n, 3n, 9n, 'budget', 3n, 9n, 5n, 6n],
        [3n, 3n, 8n, 9n, 'stop', 2n, 9n, 5n, 6n],
      ] as const) {
        cpu.setState(initial(head + entry, { 0: input, 3: head + 15n }));
        const result = await cpu.run({ budget, stop: head + stop });
        assert.equal(result.reason, reason);
        assert.equal(result.state.rip, head + rip);
        assert.equal(result.retired, retired);
        assert.equal(result.state.registers[0], eax);
        assert.equal(result.state.flags, flags);
      }
      groupedBytes = cpu.inspectTranslations()[0]!.byteLength;
      assert.ok(!cpu.inspectTranslations()[0]!.entries.includes(head + 17n));
      for (const [target, stop, reason, retired, rip, sp] of [
        [15n, head + 11n, 'stop', 2n, 11n, stack + 16n],
        [17n, head + 17n, 'stop', 1n, 17n, stack + 12n],
        [17n, undefined, 'halted', 2n, 18n, stack + 12n],
        [19n, undefined, 'fetch-fault', 1n, 19n, stack + 12n],
      ] as const) {
        cpu.setState(initial(head + 9n, { 0: 2n, 3: head + target, 4: stack + 16n }));
        const result = await cpu.run({ budget: 8n, stop });
        assert.equal(result.reason, reason);
        assert.equal(result.retired, retired);
        assert.equal(result.state.rip, head + rip);
        assert.equal(result.state.registers[0], 2n);
        assert.equal(result.state.registers[4], sp);
      }
    } finally {
      cpu.dispose();
    }

    const capped = await createCpu({
      ...options,
      limits: { ...LIMITS, maxArtifactBytes: groupedBytes - 1 },
    });
    try {
      capped.setState(initial(head, { 0: 2n, 3: head + 15n }));
      const result = await capped.run({ budget: 8n, stop: head + 9n });
      assert.equal(result.reason, 'stop');
      assert.equal(result.retired, 3n);
      assert.equal(result.state.registers[0], 5n);
      assert.equal(result.state.flags, 6n);
      assert.ok(capped.inspectTranslations()[0]!.byteLength < groupedBytes);
    } finally {
      capped.dispose();
    }
  } finally {
    Core.prototype.compile = compile;
  }
});
