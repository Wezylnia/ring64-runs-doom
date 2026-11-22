import assert from 'node:assert/strict';
import test from 'node:test';
import { Core } from '../../../../src/wasm/core.js';
import { PC, initial, usingPaged } from '../../../../../tests/execution/paging/fixture.js';
import { compiler } from './fixture.js';

test('ordinary grouped dword reads reuse the indexed page and replay an interior split', async () => {
  const source = PC + 0x1000n;
  const stop = PC + 13n;
  const data = new Uint8Array(4096);
  data[0] = 4;
  data[4] = 6;
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let markedMisses = 0;
  let guards = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const read = env.paged_loop_read_span;
    if (typeof read === 'function') {
      env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
        if (width === 4 && segment === 17) markedMisses++;
        return read(address, width, segment);
      };
    }
    const guard = env.paged_loop_guard;
    if (typeof guard === 'function') {
      env.paged_loop_guard = (pc: bigint, width: number, flags: bigint, shadow: number) => {
        const result = guard(pc, width, flags, shadow);
        if (pc === PC && shadow < 0 && result === 1) guards++;
        return result;
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        // MOV EAX,[RDI]; ADD EAX,3; ADD EAX,[RDI+4]; XOR ECX,ECX; ADD EAX,1; JMP back.
        code: '8b0783c00303470431c983c001ebf1',
        words: [[0x4008n, 0xa003n]],
        spans: [{ physical: 0xa000n, bytes: data }],
      },
      async (cpu) => {
        const prefix = async () => {
          const result = await cpu.run({ stop, budget: 16n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 5n);
          assert.equal(result.state.registers[0], 14n);
          assert.equal((await cpu.run({ stop: PC, budget: 16n })).reason, 'stop');
        };
        cpu.setState(initial(PC, { 7: source }));
        for (let warm = 0; warm < 4; warm++) await prefix();
        assert.ok(markedMisses > 0);
        const primed = markedMisses;
        const admitted = guards;
        await prefix();
        assert.equal(markedMisses, primed);
        assert.equal(guards, admitted + 1);

        cpu.setState(initial(PC, { 7: source + 4090n }));
        const split = await cpu.run({ stop, budget: 16n });
        assert.equal(split.reason, 'page-fault');
        assert.equal(split.retired, 2n);
        assert.equal(split.state.rip, PC + 5n);
        assert.equal(split.state.registers[0], 3n);
        assert.equal(split.pageFault?.address, source + 4096n);
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});

test('same-page dword prefix backedge carries its indexed authority', async () => {
  const source = PC + 0x1000n;
  const stop = PC + 16n;
  const data = new Uint8Array(4096);
  data[0] = 4;
  data[4] = 6;
  const prototype = Core.prototype as unknown as {
    artifactImports(observed: boolean): Record<string, WebAssembly.ImportValue>;
  };
  const imports = prototype.artifactImports;
  let indexed = 0;
  let legacy = 0;
  let admitted = 0;
  prototype.artifactImports = function (observed) {
    const env = imports.call(this, observed);
    const guard = env.paged_loop_guard;
    if (typeof guard === 'function') {
      env.paged_loop_guard = (pc: bigint, width: number, flags: bigint, shadow: number) => {
        const status = guard(pc, width, flags, shadow);
        if (pc === PC && shadow < 0 && status === 1) admitted++;
        return status;
      };
    }
    const read = env.paged_loop_read_span;
    if (typeof read === 'function') {
      env.paged_loop_read_span = (address: bigint, width: number, segment: number) => {
        if (width === 4 && segment === 17) indexed++;
        if (width === 4 && segment === 16) legacy++;
        return read(address, width, segment);
      };
    }
    return env;
  };
  try {
    await usingPaged(
      compiler,
      {
        // MOV EAX,[RDI]; ADD EAX,3; ADD EAX,[RDI+4]; ADD EAX,1; SUB ECX,1; JNZ back.
        code: '8b0783c00303470483c00183e90175f0',
        words: [[0x4008n, 0xa003n]],
        spans: [{ physical: 0xa000n, bytes: data }],
      },
      async (cpu) => {
        const visits = [];
        for (let wave = 0; wave < 4; wave++) {
          const before = admitted;
          cpu.setState(initial(PC, { 1: 8n, 7: source }));
          const result = await cpu.run({ stop, budget: 64n });
          assert.equal(result.reason, 'stop');
          assert.equal(result.retired, 48n);
          assert.equal(result.state.registers[0], 14n);
          visits.push(admitted - before);
        }
        assert.ok(indexed > 0);
        assert.equal(legacy, 0);
        assert.equal(visits.at(-1), 1, JSON.stringify(visits));
      },
    );
  } finally {
    prototype.artifactImports = imports;
  }
});
