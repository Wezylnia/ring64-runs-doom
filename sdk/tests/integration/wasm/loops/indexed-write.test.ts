import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { PC, initial, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('an indexed write loop bypasses grouping on a large data leaf', async () => {
  // Both transparent and opaque paths return to the counted-loop header.
  const code = 'ffc9741548ffc74883c6040fb60785c074ee8b04838906ebe7';
  const data = 0x601000n;
  const count = 32;
  const ram = new Uint8Array(4096);
  const colors = new DataView(ram.buffer);
  const color = 0x13572468;
  const table = 0x80;
  const target = 0x100;
  colors.setUint32(table + 4, color, true);
  const expected = new Uint8Array(count * 4);
  const expectedColors = new DataView(expected.buffer);
  for (let index = 0; index < count; index++) {
    ram[index] = index % 2;
    expectedColors.setUint32(index * 4, ram[index] ? color : 0, true);
  }

  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let lookups = 0;
  let unsuitable = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const group = env.paged_loop_indexed_dword_write;
    if (typeof group === 'function') {
      env.paged_loop_indexed_dword_write = (...args: [bigint, bigint, bigint, number]) => {
        lookups++;
        const result = group(...args);
        if (result === 1) unsuitable++;
        return result;
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        code,
        words: [[0x3018n, 0x600083n]],
        spans: [{ physical: data, bytes: ram }],
      },
      async (cpu) => {
        const start = initial(PC, {
          1: BigInt(count + 1),
          3: data + BigInt(table),
          6: data + BigInt(target - 4),
          7: data - 1n,
        });
        for (let wave = 0; wave < 3; wave++) {
          cpu.writePhysicalMemory(data + BigInt(target), new Uint8Array(expected.byteLength));
          cpu.setState(start);
          const before = lookups;
          const result = await cpu.run({ budget: 1000n, stop: PC + BigInt(code.length / 2) });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 274n);
          assert.deepEqual(
            cpu.readPhysicalMemory(data + BigInt(target), expected.byteLength),
            expected,
          );
          assert.ok(lookups - before < 4, 'a large data leaf must not be regrouped every lap');
        }
        assert.ok(unsuitable > 0, 'the optimized clone observed the large data leaf');
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});

test('paired dword reads keep live aliases and retire before a split-page fault', async () => {
  // DEC ECX; JZ end; ADD EAX,[RBX]; ADD R12D,[RBX+4]; MOV EDI,[RBX+4];
  // TEST EDI,EDI; JZ head; MOV [RSI],EAX; JMP head.
  const code = 'ffc974110303440363048b7b0485ff74ef8906ebeb';
  const data = PC + 0x1000n;
  const ram = new Uint8Array(4096);
  const words = new DataView(ram.buffer);
  words.setUint32(0, 1, true);
  words.setUint32(4, 7, true);
  words.setUint32(4092, 9, true);
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let reused = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const readSpan = env.paged_loop_read_span;
    if (typeof readSpan === 'function') {
      env.paged_loop_read_span = (address: bigint, bytes: number, segment: number) => {
        const result = readSpan(address, bytes, segment);
        if (address === data && bytes === 8 && segment === 7 && result !== 0) reused++;
        return result;
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        code,
        words: [[0x4008n, 0xa067n]],
        spans: [{ physical: 0xa000n, bytes: ram }],
      },
      async (cpu) => {
        const stop = PC + BigInt(code.length / 2);
        cpu.setState(initial(PC, { 0: 1n, 1: 17n, 3: data, 6: data, 7: 1n, 12: 0n }));
        const full = await cpu.run({ stop, budget: 200n });
        assert.equal(full.reason, 'stop');
        assert.equal(full.retired, 146n);
        assert.equal(full.state.registers[0], 65536n);
        assert.equal(full.state.registers[12], 112n);
        assert.deepEqual(cpu.readPhysicalMemory(0xa000n, 4), Uint8Array.of(0, 0, 1, 0));
        assert.ok(reused > 0, 'the paired span reused a live aliased source');

        cpu.setState(initial(PC, { 0: 1n, 1: 17n, 3: data + 4092n, 6: data, 7: 1n }));
        const split = await cpu.run({ stop, budget: 200n });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.retired, 3n);
        assert.equal(split.state.rip, PC + 6n);
        assert.equal(split.state.registers[0], 10n);
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});

test('the paired read window sees intervening writes and respects a clipped backing', async () => {
  // ADD EAX,[RBX]; ADD R12D,[RBX+4]; MOV [RSI],EAX; MOV EDI,[RBX-4].
  const code = 'ffc9741103034403630489068b7bfc85ff74edebeb';
  const data = PC + 0x1000n;
  const ram = new Uint8Array(4092);
  const words = new DataView(ram.buffer);
  words.setUint32(4, 1, true);
  words.setUint32(8, 7, true);

  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let hits = 0;
  let clipped = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const readSpan = env.paged_loop_read_span;
    if (typeof readSpan === 'function') {
      env.paged_loop_read_span = (address: bigint, bytes: number, segment: number) => {
        const result = readSpan(address, bytes, segment);
        if (bytes === 4 && segment === 8) {
          if (address === data + 4n && result !== 0) hits++;
          if (address === data && result === 0) clipped++;
        }
        return result;
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        code,
        words: [[0x4008n, 0xa067n]],
        spans: [{ physical: 0xa004n, bytes: ram }],
      },
      async (cpu) => {
        const stop = PC + BigInt(code.length / 2);
        cpu.setState(initial(PC, { 0: 1n, 1: 17n, 3: data + 8n, 6: data + 4n, 12: 0n }));
        const full = await cpu.run({ stop, budget: 200n });
        assert.equal(full.reason, 'stop');
        assert.equal(full.retired, 146n);
        assert.equal(full.state.registers[0], 17n);
        assert.equal(full.state.registers[7], 17n);
        assert.equal(full.state.registers[12], 112n);
        assert.ok(hits > 0, 'the same-page window read the changed dword');

        cpu.setState(initial(PC, { 0: 1n, 1: 17n, 3: data + 4n, 6: data + 12n, 7: 23n, 12: 0n }));
        const miss = await cpu.run({ stop, budget: 200n });
        assert.equal(miss.reason, 'physical-memory-fault');
        assert.equal(miss.retired, 5n);
        assert.equal(miss.state.rip, PC + 12n);
        assert.equal(miss.state.registers[7], 23n);
        assert.ok(clipped > 0, 'the admitted pair left the separate operand out of bounds');
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});
