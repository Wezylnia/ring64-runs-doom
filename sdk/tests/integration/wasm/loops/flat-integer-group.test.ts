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

test('flat sampled integer groups preserve exact faults across successful reads', async () => {
  const head = 0x31000n;
  const data = 0x40000n;
  // ADD EAX,1; MOV ECX,[ESI]; ADD EAX,2; CMP EAX,[EDI]; JNE call;
  // ADD EAX,4; call: CALL EBX; DEC EDX; JNZ head; HLT; RET.
  const code = hex('83c0018b0e83c0023b07750383c004ffd34a75ecf4c3');
  const values = hex('bc0a000005000000');
  const seeds = [0n, 3n, 5n, 8n, 10n, 12n, 15n, 17n, 18n, 20n, 21n].map((offset) => head + offset);
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, entries, regionPoints) {
    if (pc === head && !entries?.length) return compile.call(this, pc, limits, seeds, true);
    return compile.call(this, pc, limits, entries, regionPoints);
  };
  try {
    const options = {
      compiler,
      entry: head,
      limits: LIMITS,
      context: protectedSnapshot(),
      memory: {
        backings: [
          { id: 'code', bytes: code },
          { id: 'data', bytes: values },
        ],
        mappings: [
          {
            base: head,
            backing: 'code',
            offset: 0,
            length: code.length,
            permissions: { read: true, write: false, execute: true },
          },
          {
            base: data,
            backing: 'data',
            offset: 0,
            length: values.length,
            permissions: { read: true, write: false, execute: false },
          },
        ],
      },
      memoryLimits: {
        maxBackings: 2,
        maxMappings: 2,
        maxBackingBytes: code.length + values.length,
      },
    };
    const cpu = await createCpu(options);
    try {
      for (const [esi, edi, budget, stop, reason, retired, rip, eax, ecx, flags] of [
        [data, data + 4n, 12n, 15n, 'stop', 6n, 15n, 9n, 0xabcn, 6n],
        [data, data, 12n, 15n, 'stop', 5n, 15n, 5n, 0xabcn, 0x93n],
        [data, data + 4n, 12n, 8n, 'stop', 3n, 8n, 5n, 0xabcn, 6n],
        [data, data + 4n, 3n, 15n, 'budget', 3n, 8n, 5n, 0xabcn, 6n],
        [data + 8n, data, 12n, 15n, 'data-fault', 1n, 3n, 3n, 0n, 6n],
        [data, data + 8n, 12n, 15n, 'data-fault', 3n, 8n, 5n, 0xabcn, 6n],
      ] as const) {
        cpu.setState(initial(head, { 0: 2n, 1: 0n, 3: head + 21n, 6: esi, 7: edi }));
        const result = await cpu.run({ budget, stop: head + stop });
        assert.equal(result.reason, reason);
        assert.equal(result.state.rip, head + rip);
        assert.equal(result.retired, retired);
        assert.equal(result.state.registers[0], eax);
        assert.equal(result.state.registers[1], ecx);
        assert.equal(result.state.flags, flags);
      }
    } finally {
      cpu.dispose();
    }

    // Keep the sampled call loop available while the taken JNE leaves CS.
    code[11] = 0x40;
    options.context.segments.cs.limit = Number(head) + code.length - 1;
    const clipped = await createCpu(options);
    try {
      clipped.setState(initial(head, { 0: 2n, 1: 0n, 3: head + 21n, 6: data, 7: data }));
      const result = await clipped.run({ budget: 12n, stop: head + 15n });
      assert.equal(result.reason, 'segment-fault');
      assert.equal(result.state.rip, head + 10n);
      assert.equal(result.retired, 4n);
      assert.equal(result.state.registers[0], 5n);
      assert.equal(result.state.registers[1], 0xabcn);
      assert.equal(result.state.flags, 0x93n);
      assert.equal(result.segmentFault?.offset, head + 0x4cn);
    } finally {
      clipped.dispose();
    }
  } finally {
    Core.prototype.compile = compile;
  }
});

test('flat write chains preserve prior effects and exit on a code alias write', async () => {
  const head = 0x31000n;
  const data = 0x40000n;
  const alias = 0x41000n;
  // INC EAX; MOV [ESI],EAX; INC ECX; PUSH EAX; ADD [EDI],EAX; ROL [EDI],1;
  // INC EAX; CALL EBX; JMP head; RET.
  const code = hex('40890641500107d10740ffd3ebf2c3');
  const values = hex('00000000010000000000000000000000');
  const seeds = [0n, 1n, 3n, 4n, 5n, 7n, 9n, 10n, 12n, 14n].map((offset) => head + offset);
  const mapping = (base: bigint, backing: string, length: number, execute: boolean) => ({
    base,
    backing,
    offset: 0,
    length,
    permissions: { read: true, write: !execute, execute },
  });
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, entries, regionPoints) {
    if (pc === head && !entries?.length) return compile.call(this, pc, limits, seeds, true);
    return compile.call(this, pc, limits, entries, regionPoints);
  };
  try {
    const cpu = await createCpu({
      compiler,
      entry: head,
      limits: LIMITS,
      context: protectedSnapshot(),
      memory: {
        backings: [
          { id: 'code', bytes: code },
          { id: 'data', bytes: values },
        ],
        mappings: [
          mapping(head, 'code', code.length, true),
          mapping(alias, 'code', code.length, false),
          mapping(data, 'data', values.length, false),
        ],
      },
      memoryLimits: {
        maxBackings: 2,
        maxMappings: 3,
        maxBackingBytes: code.length + values.length,
      },
    });
    try {
      for (const [esi, edi, esp, budget, stop, reason, retired, rip, eax, ecx, sp, contents] of [
        [
          data,
          data + 4n,
          data + 16n,
          10n,
          10n,
          'stop',
          7n,
          10n,
          4n,
          1n,
          data + 12n,
          '03000000080000000000000003000000',
        ],
        [
          data,
          data + 4n,
          data + 16n,
          10n,
          4n,
          'stop',
          3n,
          4n,
          3n,
          1n,
          data + 16n,
          '03000000010000000000000000000000',
        ],
        [
          data,
          data + 4n,
          data + 16n,
          3n,
          10n,
          'budget',
          3n,
          4n,
          3n,
          1n,
          data + 16n,
          '03000000010000000000000000000000',
        ],
        [
          head,
          data + 4n,
          data + 16n,
          10n,
          10n,
          'data-fault',
          1n,
          1n,
          3n,
          0n,
          data + 16n,
          '00000000010000000000000000000000',
        ],
        [
          data,
          data + 4n,
          data + 24n,
          10n,
          10n,
          'data-fault',
          3n,
          4n,
          3n,
          1n,
          data + 24n,
          '03000000010000000000000000000000',
        ],
        [
          data,
          head,
          data + 16n,
          10n,
          10n,
          'data-fault',
          4n,
          5n,
          3n,
          1n,
          data + 12n,
          '03000000010000000000000003000000',
        ],
      ] as const) {
        cpu.writeMemory(data, values);
        cpu.setState(initial(head, { 0: 2n, 1: 0n, 3: head + 14n, 4: esp, 6: esi, 7: edi }));
        const result = await cpu.run({ budget, stop: head + stop });
        assert.equal(result.reason, reason);
        assert.equal(result.retired, retired);
        assert.equal(result.state.rip, head + rip);
        assert.equal(result.state.registers[0], eax);
        assert.equal(result.state.registers[1], ecx);
        assert.equal(result.state.registers[4], sp);
        assert.deepEqual(cpu.readMemory(data, 16), hex(contents));
      }

      cpu.setState(initial(head, { 0: 0x909090f3n, 1: 0n, 4: data + 16n, 6: alias + 3n }));
      const patched = await cpu.run({ budget: 10n, stop: head + 10n });
      assert.equal(patched.reason, 'halted');
      assert.equal(patched.retired, 3n);
      assert.equal(patched.state.rip, head + 4n);
      assert.equal(patched.state.registers[1], 0n);
      assert.equal(patched.state.registers[4], data + 16n);
      assert.deepEqual(cpu.readMemory(head + 3n, 4), hex('f4909090'));
    } finally {
      cpu.dispose();
    }
  } finally {
    Core.prototype.compile = compile;
  }
});

test('a straight prefix preserves the high halves projected by a disjoint register loop', async () => {
  const head = 0x31000n;
  // ADD EAX,1; XOR EAX,2; ADD EAX,4; CALL EBX.
  // loop: ADD EAX,1; DEC EDX; JNZ loop; HLT; RET.
  const code = hex('83c00183f00283c004ffd383c0014a75faf4c3');
  const seeds = [0n, 3n, 6n, 9n, 11n, 14n, 15n, 17n, 18n].map((offset) => head + offset);
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, entries, regionPoints) {
    if (pc === head && !entries?.length) return compile.call(this, pc, limits, seeds, true);
    return compile.call(this, pc, limits, entries, regionPoints);
  };
  try {
    const cpu = await createCpu({
      compiler,
      entry: head,
      limits: LIMITS,
      context: protectedSnapshot(),
      memory: {
        backings: [{ id: 'code', bytes: code }],
        mappings: [
          {
            base: head,
            backing: 'code',
            offset: 0,
            length: code.length,
            permissions: { read: true, write: false, execute: true },
          },
        ],
      },
      memoryLimits: { maxBackings: 1, maxMappings: 1, maxBackingBytes: code.length },
    });
    try {
      cpu.setState(initial(head, { 0: 0x1234567800000002n }));
      const result = await cpu.run({ budget: 8n, stop: head + 9n });
      assert.equal(result.reason, 'stop');
      assert.equal(result.retired, 3n);
      assert.equal(result.state.rip, head + 9n);
      assert.equal(result.state.registers[0], 0x1234567800000005n);
    } finally {
      cpu.dispose();
    }
  } finally {
    Core.prototype.compile = compile;
  }
});

test('a flat read chain does not reuse a disjoint loop indexed window', async () => {
  const head = 0x31000n;
  const oldData = 0x50000n;
  const newData = 0x60000n;
  // Four MOV EAX,[ESI+EAX*4]; SUB EDX,1; JNZ head.
  // MOV ESI,EDI; MOV EAX,[ESI+EAX*4]; ADD EAX,1; CALL EBX; HLT; RET.
  const code = hex('8b04868b04868b04868b048683ea0175ef89fe8b048683c001ffd3f4c3');
  const seeds = [0n, 3n, 6n, 9n, 12n, 15n, 17n, 19n, 22n, 25n, 27n, 28n].map(
    (offset) => head + offset,
  );
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, entries, regionPoints) {
    if (pc === head && !entries?.length) return compile.call(this, pc, limits, seeds, true);
    return compile.call(this, pc, limits, entries, regionPoints);
  };
  try {
    const cpu = await createCpu({
      compiler,
      entry: head,
      limits: LIMITS,
      context: protectedSnapshot(),
      memory: {
        backings: [
          { id: 'code', bytes: code },
          { id: 'old-data', bytes: hex('00000000') },
          { id: 'new-data', bytes: hex('14000000') },
        ],
        mappings: [
          {
            base: head,
            backing: 'code',
            offset: 0,
            length: code.length,
            permissions: { read: true, write: false, execute: true },
          },
          {
            base: oldData,
            backing: 'old-data',
            offset: 0,
            length: 4,
            permissions: { read: true, write: false, execute: false },
          },
          {
            base: newData,
            backing: 'new-data',
            offset: 0,
            length: 4,
            permissions: { read: true, write: false, execute: false },
          },
        ],
      },
      memoryLimits: { maxBackings: 3, maxMappings: 3, maxBackingBytes: code.length + 8 },
    });
    try {
      cpu.setState(initial(head, { 0: 0n, 2: 1n, 6: oldData, 7: newData }));
      const result = await cpu.run({ budget: 40n, stop: head + 25n });
      assert.equal(result.reason, 'stop');
      assert.equal(result.retired, 9n);
      assert.equal(result.state.rip, head + 25n);
      assert.equal(result.state.registers[6], newData);
      assert.equal(result.state.registers[0], 21n);
    } finally {
      cpu.dispose();
    }
  } finally {
    Core.prototype.compile = compile;
  }
});
