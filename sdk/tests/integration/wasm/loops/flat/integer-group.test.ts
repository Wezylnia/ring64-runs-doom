import assert from 'node:assert/strict';
import test from 'node:test';
import { createCpu } from '../../../../../src/index.js';
import { Core } from '../../../../../src/wasm/core.js';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import { protectedSnapshot } from '../../../../../../tests/support/context.js';
import { LIMITS, hex, initial } from '../../../../../../tests/support/fixture.js';
import { compiler } from '../fixture.js';

test('register INC and DEC retain carry and compute dword status at wrap boundaries', async () => {
  const head = 0x31800n;
  const high = 0x1234567800000000n;
  const code = hex('4048f4'); // INC EAX; DEC EAX; HLT.
  const cpu = await createCpu({
    compiler,
    entry: head,
    limits: LIMITS,
    context: protectedSnapshot(),
    memoryLimits: { maxBackings: 1, maxMappings: 1, maxBackingBytes: code.length },
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
  });
  try {
    for (const [offset, input, flags, value, expected] of [
      [0n, 0x7fffffffn, 3n, 0x80000000n, 0x897n],
      [0n, 0xffffffffn, 2n, 0n, 0x56n],
      [0n, 0xfn, 3n, 0x10n, 0x13n],
      [1n, 0x80000000n, 3n, 0x7fffffffn, 0x817n],
      [1n, 0n, 2n, 0xffffffffn, 0x96n],
      [1n, 1n, 3n, 0n, 0x47n],
    ] as const) {
      cpu.setState(initial(head + offset, { 0: high | input }, flags));
      const result = await cpu.run({ budget: 1n });
      assert.equal(result.reason, 'budget');
      assert.equal(result.retired, 1n);
      assert.equal(result.state.rip, head + offset + 1n);
      assert.equal(result.state.registers[0], high | value);
      assert.equal(result.state.flags, expected);
    }
  } finally {
    cpu.dispose();
  }
});

test('restored dword stack bounds distinguish narrow retained spans from exact fits', async () => {
  const head = 0x31800n;
  const stack = 0x32000n;
  // MOVZX EAX,BYTE [ESP]; MOV ECX,[ESP]; CALL EBX; DEC EDX; JNZ head; RET; RET.
  const code = hex('0fb604248b0c24ffd34a75f4c3c3');
  const seeds = [0n, 4n, 7n, 9n, 10n, 12n, 13n].map((pc) => head + pc);
  const mapping = (base: bigint, backing: string, length: number, execute: boolean) => ({
    base,
    backing,
    offset: 0,
    length,
    permissions: { read: true, write: !execute, execute },
  });
  const core = await Core.create(
    compiler,
    {
      entry: head,
      backings: [
        { id: 'code', bytes: code },
        { id: 'short', bytes: hex('112233') },
        { id: 'full', bytes: hex('44556677') },
      ],
      mappings: [
        mapping(head, 'code', code.length, true),
        mapping(stack, 'short', 3, false),
        mapping(stack + 0x100n, 'full', 4, false),
      ],
      memoryLimits: { maxBackings: 3, maxMappings: 3, maxBackingBytes: code.length + 7 },
    },
    protectedSnapshot(),
  );
  const compiled = core.compile(head, LIMITS, seeds, true);
  assert.ok(compiled);
  const { owner, entries } = compiled.metadata;
  assert.ok(core.publish(owner));
  const run = core.instantiate(new WebAssembly.Module(compiled.bytes));
  try {
    core.setState(initial(head, { 0: 0n, 1: 0n, 3: head + 13n, 4: stack }));
    assert.ok(core.activate(owner));
    assert.equal(core.execute(run, entries.indexOf(head), 8n, undefined, 0n), 'data-fault');
    assert.equal(core.current().rip, head + 4n);
    assert.equal(core.current().remaining, 7n);
    assert.equal(core.snapshot().registers[0], 0x11n);
    assert.equal(core.snapshot().registers[1], 0n);

    core.setState(initial(head, { 0: 0n, 1: 0n, 3: head + 13n, 4: stack + 0x100n }));
    assert.ok(core.activate(owner));
    assert.equal(core.execute(run, entries.indexOf(head), 2n, undefined, 0n), 'budget');
    assert.equal(core.current().rip, head + 7n);
    assert.equal(core.current().remaining, 0n);
    assert.equal(core.snapshot().registers[0], 0x44n);
    assert.equal(core.snapshot().registers[1], 0x77665544n);
  } finally {
    core.release(owner);
    core.releaseIo();
  }
});

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
    const upperBanks = Array.from({ length: 8 }, (_, index) => 0xa1b2c3d400000000n + BigInt(index));
    const upper = Object.fromEntries(upperBanks.map((value, index) => [index + 8, value]));
    let groupedBytes: number;
    try {
      for (const [entry, input, budget, stop, reason, retired, rip, eax, flags] of [
        [0n, 2n, 8n, 9n, 'stop', 3n, 9n, 5n, 6n],
        [0n, 2n, 8n, 3n, 'stop', 1n, 3n, 3n, 6n],
        [0n, 2n, 2n, 9n, 'budget', 2n, 6n, 1n, 2n],
        [0n, 2n, 3n, 9n, 'budget', 3n, 9n, 5n, 6n],
        [3n, 3n, 8n, 9n, 'stop', 2n, 9n, 5n, 6n],
      ] as const) {
        cpu.setState(initial(head + entry, { ...upper, 0: input, 3: head + 15n }));
        const result = await cpu.run({ budget, stop: head + stop });
        assert.equal(result.reason, reason);
        assert.equal(result.state.rip, head + rip);
        assert.equal(result.retired, retired);
        assert.equal(result.state.registers[0], eax);
        assert.deepEqual(result.state.registers.slice(8), upperBanks);
        assert.equal(result.state.flags, flags);
      }
      groupedBytes = cpu.inspectTranslations()[0]!.byteLength;
      assert.ok(!cpu.inspectTranslations()[0]!.entries.includes(head + 17n));
      for (const [budget, sp, reason, retired, rip, expectedSp] of [
        [4n, stack + 16n, 'budget', 4n, 15n, stack + 12n],
        [8n, stack + 16n, 'stop', 4n, 15n, stack + 12n],
        [8n, stack, 'data-fault', 3n, 9n, stack],
      ] as const) {
        cpu.setState(initial(head, { 0: 2n, 3: head + 15n, 4: sp }));
        const result = await cpu.run({ budget, stop: head + 15n });
        assert.equal(result.reason, reason);
        assert.equal(result.retired, retired);
        assert.equal(result.state.rip, head + rip);
        assert.equal(result.state.registers[0], 5n);
        assert.equal(result.state.registers[4], expectedSp);
      }
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
      cpu.setState(initial(head, { 0: 2n, 2: 2n, 3: head + 15n, 4: stack + 16n }));
      const loop = await cpu.run({ budget: 20n, stop: head + 14n });
      assert.equal(loop.reason, 'stop');
      assert.equal(loop.retired, 14n);
      assert.equal(loop.state.rip, head + 14n);
      assert.equal(loop.state.registers[0], 8n);
      assert.equal(loop.state.registers[2], 0n);
      assert.equal(loop.state.registers[4], stack + 16n);
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
  // INC EAX; CALL EBX; JMP head; INC EAX; INC ECX; RET.
  const code = hex('40890641500107d10740ffd3ebf24041c3');
  const values = hex('00000000010000000000000000000000');
  const seeds = [0n, 1n, 3n, 4n, 5n, 7n, 9n, 10n, 12n, 14n, 15n, 16n].map(
    (offset) => head + offset,
  );
  const mapping = (base: bigint, backing: string, length: number, execute: boolean) => ({
    base,
    backing,
    offset: 0,
    length,
    permissions: { read: true, write: !execute, execute },
  });
  const mappings = [
    mapping(head, 'code', code.length, true),
    mapping(alias, 'code', code.length, false),
    mapping(data, 'data', values.length, false),
  ];
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, entries, regionPoints) {
    if (pc === head && !entries?.length) return compile.call(this, pc, limits, seeds, true);
    return compile.call(this, pc, limits, entries, regionPoints);
  };
  let probes = 0;
  const restoreImports = observeArtifactImports((env) => {
    const write = env.write_span;
    if (typeof write === 'function') {
      env.write_span = (address: bigint, width: number, segment: number) => {
        probes++;
        return write(address, width, segment);
      };
    }
  });
  try {
    const cpu = await createCpu({
      compiler,
      entry: head,
      limits: { ...LIMITS, maxInstructions: seeds.length },
      context: protectedSnapshot(),
      memory: {
        backings: [
          { id: 'code', bytes: code },
          { id: 'data', bytes: values },
        ],
        mappings,
      },
      memoryLimits: {
        maxBackings: 2,
        maxMappings: 3,
        maxBackingBytes: code.length + values.length,
      },
    });
    try {
      const start = initial(head, { 0: 2n, 1: 0n, 4: data + 16n, 6: data, 7: data + 4n });
      cpu.setState(start);
      const first = await cpu.run({ budget: 10n, stop: head + 10n });
      assert.equal(first.retired, 7n);
      assert.equal(probes, 2);
      cpu.setState(start);
      const warm = await cpu.run({ budget: 10n, stop: head + 10n });
      assert.deepEqual(warm.state, first.state);
      assert.equal(warm.retired, first.retired);
      assert.equal(probes, 2);

      cpu.setMappings([
        mappings[0]!,
        mappings[1]!,
        { ...mappings[2]!, permissions: { read: true, write: false, execute: false } },
      ]);
      cpu.setState(start);
      const revoked = await cpu.run({ budget: 10n, stop: head + 10n });
      assert.equal(revoked.reason, 'data-fault');
      assert.equal(revoked.retired, 1n);
      assert.equal(revoked.state.rip, head + 1n);
      cpu.setMappings(mappings);

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
        const result = await cpu.run({
          budget,
          stop: head + (reason === 'data-fault' ? 12n : stop),
        });
        assert.equal(result.reason, reason);
        assert.equal(result.retired, retired);
        assert.equal(result.state.rip, head + rip);
        assert.equal(result.state.registers[0], eax);
        assert.equal(result.state.registers[1], ecx);
        assert.equal(result.state.registers[4], sp);
        assert.deepEqual(cpu.readMemory(data, 16), hex(contents));
      }

      cpu.writeMemory(data, values);
      cpu.setState(
        initial(head, { 0: 2n, 1: 0n, 3: head + 14n, 4: data + 16n, 6: data, 7: data + 4n }),
      );
      const complete = await cpu.run({ budget: 20n, stop: head + 12n });
      assert.equal(complete.reason, 'stop');
      assert.equal(complete.retired, 11n);
      assert.equal(complete.state.rip, head + 12n);
      assert.equal(complete.state.registers[0], 5n);
      assert.equal(complete.state.registers[1], 2n);
      assert.equal(complete.state.registers[4], data + 12n);

      cpu.setState(initial(head + 14n, { 0: 2n, 1: 0n, 4: data + 16n }));
      const failedReturn = await cpu.run({ budget: 10n, stop: head + 12n });
      assert.equal(failedReturn.reason, 'data-fault');
      assert.equal(failedReturn.retired, 2n);
      assert.equal(failedReturn.state.rip, head + 16n);
      assert.equal(failedReturn.state.registers[0], 3n);
      assert.equal(failedReturn.state.registers[1], 1n);
      assert.equal(failedReturn.state.registers[4], data + 16n);

      cpu.setState(initial(head, { 0: 0x909090f3n, 1: 0n, 4: data + 16n, 6: alias + 3n }));
      const patched = await cpu.run({ budget: 10n, stop: head + 12n });
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
    restoreImports();
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

test('funded stack groups preserve wrapped offsets and partial stack effects', async () => {
  const head = 0x31000n;
  const stack = 0xffffffe8n;
  // PUSH EAX; PUSH ECX; CALL EBX; ADD ESP,8; DEC EDX; JNZ head; HLT;
  // PUSH ESI; MOV EAX,[ESP+8]; MOV ECX,[ESP+12]; POP ESI; RET.
  const code = hex('5051ffd383c4084a75f6f4568b4424088b4c240c5ec3');
  const seeds = [0n, 1n, 2n, 4n, 7n, 8n, 10n, 11n, 12n, 16n, 20n, 21n].map(
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
      memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: code.length + 24 },
      memory: {
        backings: [
          { id: 'code', bytes: code },
          { id: 'stack', bytes: new Uint8Array(24) },
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
            base: stack,
            backing: 'stack',
            offset: 0,
            length: 24,
            permissions: { read: true, write: true, execute: false },
          },
        ],
      },
    });
    try {
      for (const [sp, stop, reason, retired, rip, expectedSp] of [
        [0x100000000n, 10n, 'stop', 22n, 10n, 0x100000000n],
        [0x100000000n, 10n, 'stop', 22n, 10n, 0x100000000n],
        [0x100000000n, 12n, 'stop', 4n, 12n, 0x1fffffff0n],
        [stack + 12n, 10n, 'data-fault', 3n, 11n, stack],
      ] as const) {
        cpu.setState(initial(head, { 0: 10n, 1: 20n, 2: 2n, 3: head + 11n, 4: sp, 6: 50n }));
        const result = await cpu.run({ budget: 64n, stop: head + stop });
        assert.equal(result.reason, reason);
        assert.equal(result.retired, retired);
        assert.equal(result.state.rip, head + rip);
        assert.deepEqual(result.state.registers.slice(0, 2), [10n, 20n]);
        assert.equal(result.state.registers[4], expectedSp);
        assert.equal(result.state.registers[6], 50n);
      }
      const prefix = await cpu.readMemory(stack, 12);
      assert.deepEqual(prefix, hex('04100300140000000a000000'));
    } finally {
      cpu.dispose();
    }
  } finally {
    Core.prototype.compile = compile;
  }
});

test('bounded flat data windows replay at the first checked operand on refusal', async () => {
  const head = 0x31000n;
  const data = 0x40000n;
  const stack = 0x41000n;
  // INC EAX; AND ECX,3; MOV EDX,[ESI+ECX*4]; INC EAX; MOV [ESI+16],EAX;
  // AND EDI,3; ADD [ESI+EDI*4+16],EDX; CALL EBX; DEC EBP; JNZ head; HLT; RET.
  const code = hex('4083e1038b148e4089461083e7030154be10ffd34d75e9f4c3');
  const values = new Uint8Array(32);
  new DataView(values.buffer).setUint32(12, 7, true);
  const seeds = [0n, 1n, 4n, 7n, 8n, 11n, 14n, 18n, 20n, 21n, 23n, 24n].map(
    (offset) => head + offset,
  );
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, entries, regionPoints) {
    if (pc === head && !entries?.length) return compile.call(this, pc, limits, seeds, true);
    return compile.call(this, pc, limits, entries, regionPoints);
  };
  const mapping = (base: bigint, backing: string, length: number, execute: boolean) => ({
    base,
    backing,
    length,
    offset: 0,
    permissions: { read: true, write: !execute, execute },
  });
  const codeMap = mapping(head, 'code', code.length, true);
  const stackMap = mapping(stack, 'stack', 16, false);
  try {
    const cpu = await createCpu({
      compiler,
      entry: head,
      limits: LIMITS,
      context: protectedSnapshot(),
      memoryLimits: { maxBackings: 3, maxMappings: 3, maxBackingBytes: code.length + 48 },
      memory: {
        backings: [
          { id: 'code', bytes: code },
          { id: 'data', bytes: values },
          { id: 'stack', bytes: new Uint8Array(16) },
        ],
        mappings: [codeMap, mapping(data, 'data', 32, false), stackMap],
      },
    });
    try {
      const input = initial(head, {
        0: 10n,
        1: 0x123n,
        2: 0n,
        3: head + 24n,
        4: stack + 16n,
        5: 2n,
        6: data,
        7: 0x100n,
      });
      for (let pass = 0; pass < 2; pass++) {
        cpu.setState(input);
        const result = await cpu.run({ budget: 32n, stop: head + 23n });
        assert.equal(result.reason, 'stop');
        assert.equal(result.retired, 22n);
        assert.equal(result.state.registers[0], 14n);
        assert.equal(result.state.registers[2], 7n);
        assert.deepEqual(await cpu.readMemory(data + 16n, 4), hex('15000000'));
      }

      cpu.setState(input);
      const inner = await cpu.run({ budget: 32n, stop: head + 7n });
      assert.equal(inner.reason, 'stop');
      assert.equal(inner.retired, 3n);
      assert.equal(inner.state.registers[0], 11n);
      assert.equal(inner.state.registers[2], 7n);

      cpu.setMappings([codeMap, mapping(data, 'data', 16, false), stackMap]);
      cpu.setState(input);
      const clipped = await cpu.run({ budget: 32n, stop: head + 23n });
      assert.equal(clipped.reason, 'data-fault');
      assert.equal(clipped.fault?.address, data + 16n);
      assert.equal(clipped.state.rip, head + 8n);
      assert.equal(clipped.retired, 4n);
      assert.equal(clipped.state.registers[0], 12n);
      assert.equal(clipped.state.registers[2], 7n);
      assert.equal(clipped.state.flags, 6n);
    } finally {
      cpu.dispose();
    }
  } finally {
    Core.prototype.compile = compile;
  }
});

test('flat result status preserves imported flags, conditions, and narrow arithmetic', async () => {
  const head = 0x31000n;
  const stack = 0x32000n;
  // ADD AX,1; all 16 CMOVcc EAX,ECX; CALL EBX; DEC EDX; JNZ head; HLT; RET.
  const code = Uint8Array.from([
    ...hex('6683c001'),
    ...Array.from({ length: 16 }, (_, cc) => [0x0f, 0x40 + cc, 0xc1]).flat(),
    ...hex('ffd34a75c7f4c3'),
  ]);
  const conditions = Array.from({ length: 16 }, (_, cc) => head + BigInt(4 + 3 * cc));
  const seeds = [head, ...conditions, ...[52n, 54n, 55n, 57n, 58n].map((offset) => head + offset)];
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
      memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: code.length + 16 },
      memory: {
        backings: [
          { id: 'code', bytes: code },
          { id: 'stack', bytes: new Uint8Array(16) },
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
            base: stack,
            backing: 'stack',
            offset: 0,
            length: 16,
            permissions: { read: true, write: true, execute: false },
          },
        ],
      },
    });
    try {
      for (const [ax, eax, flags] of [
        [0x7fffn, 0xabcdefn, 0x896n],
        [0xffffn, 0n, 0x57n],
        [0x00ffn, 0x100n, 0x16n],
      ] as const) {
        cpu.setState(initial(head, { 0: ax, 1: 0xabcdefn, 3: head + 58n, 4: stack + 16n }));
        const result = await cpu.run({ budget: 8n, stop: head + 7n });
        assert.equal(result.reason, 'stop');
        assert.equal(result.retired, 2n);
        assert.equal(result.state.registers[0], eax);
        assert.equal(result.state.flags, flags);
      }

      for (let bits = 0; bits < 64; bits++) {
        const [carry, parity, auxiliary, zero, sign, overflow] = [0, 1, 2, 3, 4, 5].map(
          (bit) => (bits & (1 << bit)) !== 0,
        );
        const flags = BigInt(
          2 +
            Number(carry) +
            4 * Number(parity) +
            16 * Number(auxiliary) +
            64 * Number(zero) +
            128 * Number(sign) +
            2048 * Number(overflow),
        );
        const less = sign !== overflow;
        const truth = [
          overflow,
          !overflow,
          carry,
          !carry,
          zero,
          !zero,
          carry || zero,
          !carry && !zero,
          sign,
          !sign,
          parity,
          !parity,
          less,
          !less,
          zero || less,
          !zero && !less,
        ];
        for (const [cc, pc] of conditions.entries()) {
          cpu.setState(initial(pc, { 0: 1n, 1: 2n }, flags));
          const result = await cpu.run({ budget: 8n, stop: pc + 3n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 1n);
          assert.equal(result.state.registers[0], truth[cc] ? 2n : 1n);
          assert.equal(result.state.flags, flags);
        }
      }
    } finally {
      cpu.dispose();
    }
  } finally {
    Core.prototype.compile = compile;
  }
});
