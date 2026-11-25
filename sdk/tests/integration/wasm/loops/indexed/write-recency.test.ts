import assert from 'node:assert/strict';
import { observeArtifactImports } from '../../../../../../tests/support/artifact-imports.js';
import {
  PC,
  initial,
  usingPaged,
} from '../../../../../../tests/execution/memory/paging/fixture.js';
import { indexedTest } from './write-fixture.js';

const loop = 'ffc9741548ffc74883c6040fb60785c074ee8b04838906ebe7';
const probeOffset = 0x80;
const pageSize = 4096;
const capacity = 64;

// Read the private IndexedDwordWrite / IndexedReadTable ABI without touching the cache.
function watchSlots(memory: WebAssembly.Memory, record: number) {
  const view = new DataView(memory.buffer);
  const recency = view.getUint32(record + 56, true);
  if (!recency) return;
  const slots = Array.from({ length: 4 }, (_, operand) => {
    const age = view.getUint32(record + 36 + operand * 4, true);
    const slot = (age - recency - 8) / 8;
    assert.ok(Number.isInteger(slot) && slot >= 0 && slot < capacity);
    return slot;
  });
  return {
    slots,
    ids() {
      const live = new DataView(memory.buffer, recency + 8 + capacity * 8, capacity * 8);
      return slots.map((slot) => live.getBigUint64(slot * 8, true));
    },
  };
}

async function replacementOrder(
  core: WebAssembly.Module,
  useIndexed: boolean,
  aliasTable: boolean,
) {
  const indices = PC + 0x1000n;
  const table = aliasTable ? indices : PC + 0x2000n;
  const output = PC + 0x3000n;
  const paletteOffset = 0x100;
  const count = 32;
  const color = 0x4b8e1023;
  const ram = new Uint8Array(pageSize * (capacity + 3));
  ram.fill(1, 0, count - 1); // Finish with a transparent pixel.
  new DataView(ram.buffer).setUint32(Number(table - indices) + paletteOffset + 4, color, true);
  const words = Array.from(
    { length: capacity + 3 },
    (_, page) => [0x4008n + BigInt(page * 8), 0xa067n + BigInt(page * pageSize)] as const,
  );

  let watched: ReturnType<typeof watchSlots>;
  let warm = true;
  let indexedCalls = 0;
  const restore = observeArtifactImports((env) => {
    const resolve = env.paged_loop_indexed_dword_write;
    if (typeof resolve !== 'function') return;
    const memory = env.memory;
    assert.ok(memory instanceof WebAssembly.Memory);
    env.paged_loop_indexed_dword_write = (...args: [bigint, bigint, bigint, number]) => {
      if (!warm && !useIndexed) return 0; // Ask the generated body to take its checked path.
      const record = resolve(...args) as number;
      const slots = record > 1 ? watchSlots(memory, record) : undefined;
      if (slots) {
        if (warm) watched = slots;
        else indexedCalls++;
      }
      return record;
    };
  });
  const evictions: number[] = [];
  try {
    await usingPaged(
      core,
      {
        code: loop + 'cc'.repeat(probeOffset - loop.length / 2) + '418b00f4',
        words,
        spans: [{ physical: 0xa000n, bytes: ram }],
      },
      async (cpu) => {
        const start = initial(PC, {
          1: BigInt(count + 1),
          3: table + BigInt(paletteOffset),
          6: output - 4n,
          7: indices - 1n,
        });
        const end = PC + BigInt(loop.length / 2);
        cpu.setState(start);
        assert.equal((await cpu.run({ budget: 1000n, stop: end })).reason, 'stop');
        const retained = watched;
        assert.ok(retained, 'warmup published a retained index, table, store and fetch');
        assert.equal(retained.slots[0] === retained.slots[1], aliasTable);
        warm = false;
        cpu.setState(start);
        const result = await cpu.run({ budget: 1000n, stop: end });
        assert.equal(result.reason, 'stop');
        assert.equal(result.retired, 319n);
        if (useIndexed) assert.equal(indexedCalls, 1, 'the cached proof handled later pixels');
        const target = new DataView(cpu.readPhysicalMemory(0xc000n, count * 4).buffer);
        assert.equal(target.getUint32((count - 2) * 4, true), color);
        assert.equal(target.getUint32((count - 1) * 4, true), 0);

        const ids = retained.ids();
        const operands = retained.slots
          .map((slot, operand) => (retained.slots.lastIndexOf(slot) === operand ? operand : -1))
          .filter((operand) => operand >= 0 && operand !== 3);
        const probe = PC + BigInt(probeOffset);
        const unmapped = indices + BigInt(words.length * pageSize);
        cpu.setState(initial(probe, { 8: unmapped }));
        const fault = await cpu.run({ budget: 2n, stop: probe + 3n });
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.pageFault?.address, unmapped);
        assert.equal(fault.retired, 0n);
        assert.deepEqual(retained.ids(), ids, 'a refused walk keeps all retained translations');
        for (let page = 3; page < words.length && evictions.length < operands.length; page++) {
          cpu.setState(initial(probe, { 8: indices + BigInt(page * pageSize) }));
          assert.equal((await cpu.run({ budget: 2n, stop: probe + 3n })).reason, 'stop');
          const current: bigint[] = retained.ids();
          for (const operand of operands)
            if (current[operand] !== ids[operand] && !evictions.includes(operand))
              evictions.push(operand);
          assert.equal(current[3], ids[3], 'the probe kept the fetch translation newest');
        }
      },
    );
    return evictions;
  } finally {
    restore();
  }
}

async function checkReplacementOrder(core: WebAssembly.Module) {
  for (const aliasTable of [false, true]) {
    const checked = await replacementOrder(core, false, aliasTable);
    const generated = await replacementOrder(core, true, aliasTable);
    assert.deepEqual(generated, checked);
    assert.deepEqual(checked, aliasTable ? [2, 1] : [1, 2, 0]);
  }
}
indexedTest(
  'indexed writes preserve the checked translation eviction order',
  checkReplacementOrder,
);
