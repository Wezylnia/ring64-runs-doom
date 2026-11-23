import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { BANK, PC, initial, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
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

test('indexed stores reuse full pages and reload the palette through a writable alias', async () => {
  const code = 'ffc9741548ffc74883c6040fb60785c074ee8b04838906ebe7';
  const indices = PC + 0x1000n;
  const palette = PC + 0x2000n;
  const output = PC + 0x4000n;
  const ram = new Uint8Array(4096);
  ram.fill(1);
  ram[0] = 2;
  ram[7] = 0;
  const colors = new Uint8Array(4096);
  new DataView(colors.buffer).setUint32(8, 0x13572468, true);

  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let resolutions = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const resolve = env.paged_loop_indexed_dword_write;
    if (typeof resolve === 'function') {
      env.paged_loop_indexed_dword_write = (...args: [bigint, bigint, bigint, number]) => {
        resolutions++;
        return resolve(...args);
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        code,
        words: [
          [0x4008n, 0xa067n],
          [0x4010n, 0xb067n],
          [0x4020n, 0xb067n],
        ],
        spans: [
          { physical: 0xa000n, bytes: ram },
          { physical: 0xb000n, bytes: colors },
        ],
      },
      async (cpu) => {
        const stop = PC + BigInt(code.length / 2);
        for (let wave = 0; wave < 2; wave++) {
          cpu.writePhysicalMemory(0xb000n, colors);
          cpu.setState(initial(PC, { 1: 65n, 3: palette, 6: output, 7: indices - 1n }));
          const before = resolutions;
          const full = await cpu.run({ stop, budget: 1000n });
          assert.equal(full.reason, 'stop');
          const bytes = cpu.readPhysicalMemory(0xb004n, 64 * 4);
          const values = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
          for (let index = 0; index < 64; index++) {
            assert.equal(values.getUint32(index * 4, true), index === 7 ? 0 : 0x13572468);
          }
          assert.ok(resolutions - before < 12, 'stable pages resolved repeatedly');
        }
        assert.ok(resolutions > 0, 'the generated indexed loop ran');

        const debug = cpu.readDebugState();
        cpu.setDebugState({ ...debug, addresses: [PC + 2n, 0n, 0n, 0n], dr7: 0x401n });
        cpu.setState(initial(PC, { 1: 65n, 3: palette, 6: output, 7: indices - 1n }));
        const watched = await cpu.run({ stop, budget: 1000n });
        assert.equal(watched.reason, 'debug-exception');
        assert.equal(watched.retired, 1n);
        assert.equal(watched.state.rip, PC + 2n);
        cpu.setDebugState(debug);

        cpu.setState(initial(PC, { 1: 65n, 3: palette, 6: output + 4088n, 7: indices - 1n }));
        const fault = await cpu.run({ stop, budget: 1000n });
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.state.rip, PC + 21n);
        assert.equal(fault.retired, 18n);
        assert.equal(
          new DataView(cpu.readPhysicalMemory(0xbffcn, 4).buffer).getUint32(0, true),
          0x13572468,
        );
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});

test('an admitted indexed store publishes both registers and TEST flags before the next fault', async () => {
  // MOVZX EAX,[RDI]; TEST EAX,EAX; JZ head; MOV EDX,[RBX+RAX*4]; MOV [RSI],EDX;
  // MOV EBP,[R8] exposes the completed group's state on a fault.
  const code = 'ffc9741848ffc74883c6040fb60785c074ee8b14838916418b28ebe4';
  const data = PC + 0x1000n;
  const ram = new Uint8Array(4096);
  const words = new DataView(ram.buffer);
  ram.fill(3, 0, 32);
  words.setUint32(0x8c, 0xfedcba98, true);
  await usingPaged(
    compiler,
    {
      code,
      bank: { ...BANK, cr0: BANK.cr0 | 0x10000n },
      words: [
        [0x4008n, 0xa067n],
        [0x4018n, 0xa065n],
      ],
      spans: [{ physical: 0xa000n, bytes: ram }],
    },
    async (cpu) => {
      const end = PC + BigInt(code.length / 2);
      const start = (suffix = data, destination = data) =>
        initial(PC, {
          0: 0xffffffffffffffffn,
          1: 33n,
          2: 7n,
          3: data + 0x80n,
          6: destination + 0xfcn,
          7: data - 1n,
          8: suffix,
        });
      cpu.setState(start());
      assert.equal((await cpu.run({ stop: end, budget: 1000n })).reason, 'stop');
      const out = 0xa100n;
      const readOut = () => new DataView(cpu.readPhysicalMemory(out, 4).buffer).getUint32(0, true);
      for (const [index, flags] of [
        [3, 6n],
        [1, 2n],
      ] as const) {
        cpu.writePhysicalMemory(0xa000n, Uint8Array.of(index));
        cpu.writePhysicalMemory(out, new Uint8Array(4));
        cpu.setState(start());
        const inside = await cpu.run({ stop: PC + 18n, budget: 1000n });
        assert.equal(inside.retired, 7n);
        assert.equal(inside.state.registers[0], BigInt(index));
        assert.equal(inside.state.registers[2], 7n);
        assert.equal(inside.state.flags, flags);
        assert.equal(readOut(), 0);

        cpu.setState(start());
        const short = await cpu.run({ stop: end, budget: 8n });
        const color = index === 3 ? 0xfedcba98 : 0;
        assert.equal(short.state.rip, PC + 21n);
        assert.equal(short.state.registers[2], BigInt(color));
        assert.equal(readOut(), 0);

        cpu.setState(start(data + 4096n));
        const fault = await cpu.run({ stop: end, budget: 1000n });
        assert.equal(fault.reason, 'page-fault');
        assert.equal(fault.retired, 9n);
        assert.equal(fault.state.rip, PC + 23n);
        assert.equal(fault.state.registers[0], BigInt(index));
        assert.equal(fault.state.registers[2], BigInt(color));
        assert.equal(fault.state.flags, flags);
        assert.equal(readOut(), color);
      }

      cpu.writePhysicalMemory(0xa000n, Uint8Array.of(3));
      cpu.writePhysicalMemory(out, new Uint8Array(4));
      cpu.setState(start(data, data + 8192n));
      const denied = await cpu.run({ stop: end, budget: 1000n });
      assert.equal(denied.reason, 'page-fault');
      assert.equal(denied.retired, 8n);
      assert.equal(denied.state.rip, PC + 21n);
      assert.equal(denied.state.registers[2], 0xfedcba98n);
      assert.equal(denied.state.flags, 6n);
      assert.equal(readOut(), 0);

      cpu.writePhysicalMemory(0xa000n, Uint8Array.of(0));
      cpu.setState(start(data + 4096n, data + 8192n));
      const transparent = await cpu.run({ stop: end, budget: 7n });
      assert.equal(transparent.state.rip, PC);
      assert.equal(transparent.state.registers[2], 7n);
      assert.equal(transparent.state.flags, 0x46n);
      assert.equal(readOut(), 0);
    },
  );
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

test('a tail read carries live paired operands only across its own loop edge', async () => {
  // DEC ECX; ADD EAX,[RBX]; ADD R12D,[RBX+4]; TEST ECX,ECX; JZ end;
  // TEST EDX,EDX; JZ head; MOV [RSI],EAX; MOV EDI,[RBX-4]; JMP head.
  const code = 'ffc903034403630485c9740b85d274f089068b7bfcebe9';
  const data = PC + 0x1000n;
  const ram = new Uint8Array(4096);
  const words = new DataView(ram.buffer);
  words.setUint32(4, 23, true);
  words.setUint32(8, 1, true);
  words.setUint32(12, 7, true);
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let pairs = 0;
  let resolvedTails = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const span = env.paged_loop_read_span;
    if (typeof span === 'function') {
      env.paged_loop_read_span = (address: bigint, bytes: number, segment: number) => {
        if (bytes === 8 && (segment === 6 || segment === 7)) pairs++;
        if (bytes === 4 && segment === 8) resolvedTails++;
        return span(address, bytes, segment);
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
        for (const draws of [1n, 0n, 1n]) {
          cpu.writePhysicalMemory(0xa000n, ram);
          cpu.setState(
            initial(PC, { 0: 1n, 1: 17n, 2: draws, 3: data + 8n, 6: data + 8n, 7: 0n, 12: 0n }),
          );
          const before = pairs;
          const tailsBefore = resolvedTails;
          const result = await cpu.run({ stop, budget: 200n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, draws ? 165n : 117n);
          assert.equal(result.state.registers[0], draws ? 131072n : 18n);
          assert.equal(result.state.registers[12], 119n);
          assert.equal(result.state.registers[7], draws ? 23n : 0n);
          const resolved = pairs - before;
          assert.ok(
            resolvedTails - tailsBefore <= 2,
            'full-page tail reads should reuse the mapping',
          );
          assert.ok(
            resolved > 0 && resolved <= 2,
            `paired lookups with draws=${draws}: ${resolved}`,
          );
        }
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});
