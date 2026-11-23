import assert from 'node:assert/strict';
import test from 'node:test';
import { createCpu, type RunResult } from '../../../../../src/index.js';
import { Core } from '../../../../../src/wasm/core.js';
import { PC, initial, usingPaged } from '../../../../../../tests/execution/paging/fixture.js';
import { protectedSnapshot } from '../../../../../../tests/support/context.js';
import { LIMITS, hex } from '../../../../../../tests/support/fixture.js';
import { compiler } from '../fixture.js';

function fragmentedLoop() {
  const bytes: number[] = [];
  const labels = new Map<string, number>();
  const fixups: [number, string][] = [];
  const branch = (name: string, conditional: boolean) => {
    bytes.push(...(conditional ? [0x0f, 0x84] : [0xe9]));
    fixups.push([bytes.length, name]);
    bytes.push(0, 0, 0, 0);
  };
  const coldSide = (name: string) => {
    bytes.push(0x85, 0xd2); // TEST EDX,EDX
    branch(name, true);
    bytes.push(...Array<number>(64).fill(0x90), 0xf4);
    labels.set(name, bytes.length);
  };
  labels.set('first', 0);
  coldSide('accumulate');
  for (let i = 0; i < 10; i++) bytes.push(0xff, 0xc0); // INC EAX
  bytes.push(0xff, 0xc9); // DEC ECX
  branch('stop', true);
  branch('second', false);
  labels.set('second', bytes.length);
  coldSide('record');
  for (let i = 0; i < 10; i++) bytes.push(0xff, 0xc3); // INC EBX
  branch('first', false);
  labels.set('stop', bytes.length);
  bytes.push(0xf4);
  const code = Uint8Array.from(bytes);
  const view = new DataView(code.buffer);
  for (const [offset, name] of fixups) view.setInt32(offset, labels.get(name)! - offset - 4, true);
  return {
    code: Buffer.from(code).toString('hex'),
    stop: PC + BigInt(labels.get('stop')!),
    accumulate: labels.get('accumulate')!,
  };
}

test('retired same-page paths form a smaller-dispatch region under the original cap', async () => {
  const { code, stop, accumulate } = fragmentedLoop();
  const begin = Core.prototype.beginPagedRegionTrace;
  const compile = Core.prototype.compile;
  const instantiate = Core.prototype.instantiatePagedRegionTrace;
  const execute = Core.prototype.execute;
  const aliases = new WeakSet<ReturnType<typeof instantiate>>();
  let enabled = false;
  let regions = 0;
  let sampled = 0n;
  let yielded = false;
  Core.prototype.beginPagedRegionTrace = function (pc) {
    const accepted = enabled && begin.call(this, pc);
    if (accepted) {
      sampled = 0n;
      yielded = false;
    }
    return accepted;
  };
  Core.prototype.instantiatePagedRegionTrace = function (...args) {
    const run = instantiate.apply(this, args);
    aliases.add(run);
    return run;
  };
  Core.prototype.execute = function (run, index, budget, ...args) {
    const result = execute.call(this, run, index, budget, ...args);
    if (aliases.has(run)) {
      sampled += budget - this.current().remaining;
      assert.ok(sampled <= 2048n);
      yielded ||= result === 'budget';
    }
    return result;
  };
  Core.prototype.compile = function (pc, limits, points, observedRegion) {
    const artifact = compile.call(this, pc, limits, points, observedRegion);
    if (observedRegion && artifact) {
      regions++;
      assert.equal(sampled, 2048n);
      assert.equal(yielded, true, 'the private budget yields inside a normal native entry');
      assert.ok(artifact.metadata.entries.length <= limits.maxInstructions);
      assert.ok(artifact.bytes.length <= limits.maxArtifactBytes);
    }
    return artifact;
  };
  try {
    const comparisons: {
      runs: Record<'first' | 'warm' | 'budget' | 'debug' | 'modified', RunResult>;
      tables: Uint8Array;
    }[] = [];
    for (const adaptive of [false, true]) {
      enabled = adaptive;
      regions = 0;
      await usingPaged(compiler, { code, limits: { maxInstructions: 32 } }, async (cpu) => {
        const enter = () => cpu.setState(initial(PC, { 0: 0n, 1: 512n, 2: 0n, 3: 0n }));
        enter();
        const first = await cpu.run({ stop, budget: 20_000n });
        assert.equal(first.reason, 'stop');
        assert.equal(first.retired, 14_322n);
        assert.equal(first.state.registers[0], 5120n);
        assert.equal(first.state.registers[3], 5110n);
        assert.equal(regions, 0);

        enter();
        const repeated = await cpu.run({ stop, budget: 20_000n });
        assert.equal(repeated.retired, first.retired);
        assert.equal(regions, Number(adaptive));
        enter();
        const warm = await cpu.run({ stop, budget: 20_000n });
        if (adaptive) assert.equal(warm.resources.compilations, 0);
        assert.equal(regions, Number(adaptive));
        enter();
        const budget = await cpu.run({ stop, budget: 9n });
        assert.equal(budget.reason, 'budget');
        assert.equal(budget.retired, 9n);

        const breakpoint = PC + BigInt(accumulate);
        cpu.setDebugState({ addresses: [breakpoint, 0n, 0n, 0n], dr6: 0n, dr7: 0x401n });
        enter();
        const debug = await cpu.run({ stop, budget: 20_000n });
        assert.equal(debug.reason, 'debug-exception');
        assert.equal(debug.retired, 2n);
        assert.equal(debug.state.rip, breakpoint);
        assert.equal(regions, Number(adaptive));
        cpu.setDebugState({ addresses: [0n, 0n, 0n, 0n], dr6: 0n, dr7: 0x400n });

        cpu.writePhysicalMemory(0x9000n + BigInt(accumulate) + 1n, Uint8Array.of(0xc8));
        enter();
        const modified = await cpu.run({ stop, budget: 20_000n });
        assert.equal(modified.reason, 'stop');
        assert.equal(modified.state.registers[0], 4096n);
        assert.equal(regions, Number(adaptive));
        enter();
        const modifiedWarm = await cpu.run({ stop, budget: 20_000n });
        assert.deepEqual(modifiedWarm.state, modified.state);
        assert.equal(regions, Number(adaptive) * 2);
        comparisons.push({
          runs: { first, warm, budget, debug, modified },
          tables: cpu.readPhysicalMemory(0x1000n, 0x4000),
        });
      });
    }
    const [baseline, adaptive] = comparisons;
    assert.ok(baseline && adaptive);
    assert.ok(
      adaptive.runs.warm.resources.dispatches * 2 < baseline.runs.warm.resources.dispatches,
    );
    assert.deepEqual(adaptive.runs.first.resources, baseline.runs.first.resources);
    assert.deepEqual(adaptive.tables, baseline.tables);
    for (const sample of ['first', 'warm', 'budget', 'debug', 'modified'] as const) {
      assert.equal(adaptive.runs[sample].reason, baseline.runs[sample].reason);
      assert.equal(adaptive.runs[sample].retired, baseline.runs[sample].retired);
      assert.deepEqual(adaptive.runs[sample].state, baseline.runs[sample].state);
    }
  } finally {
    Core.prototype.beginPagedRegionTrace = begin;
    Core.prototype.compile = compile;
    Core.prototype.instantiatePagedRegionTrace = instantiate;
    Core.prototype.execute = execute;
  }
});

test('a short paged program never begins a region trace', async () => {
  const begin = Core.prototype.beginPagedRegionTrace;
  let attempted = false;
  Core.prototype.beginPagedRegionTrace = function (pc) {
    attempted = true;
    return begin.call(this, pc);
  };
  try {
    await usingPaged(compiler, { code: '90909090' }, async (cpu) => {
      const result = await cpu.run({ stop: PC + 4n, budget: 20_000n });
      assert.equal(result.reason, 'stop');
      assert.equal(result.retired, 4n);
      assert.equal(attempted, false);
    });
  } finally {
    Core.prototype.beginPagedRegionTrace = begin;
  }
});

test('flat region taps preserve the run budget and obey the artifact cache cap', async () => {
  const { code, stop } = fragmentedLoop();
  const bytes = hex(code);
  const options = {
    compiler,
    entry: PC,
    context: protectedSnapshot(),
    memory: {
      backings: [{ id: 'code', bytes }],
      mappings: [
        {
          backing: 'code',
          base: PC,
          length: bytes.length,
          offset: 0,
          permissions: { read: true, write: false, execute: true },
        },
      ],
    },
    memoryLimits: { maxBackings: 1, maxMappings: 1, maxBackingBytes: bytes.length },
  };
  const limits = { ...LIMITS, maxInstructions: 32 };
  const finish = Core.prototype.finishPagedRegionTrace;
  const instantiate = Core.prototype.instantiatePagedRegionTrace;
  let completed = 0;
  let clones = 0;
  Core.prototype.finishPagedRegionTrace = function (maxPoints) {
    const trace = finish.call(this, maxPoints);
    if (maxPoints > 0) {
      assert.equal(trace.retired, 2048);
      completed++;
    }
    return trace;
  };
  Core.prototype.instantiatePagedRegionTrace = function (...args) {
    const imports = WebAssembly.Module.imports(args[0]);
    assert.ok(imports.some(({ name }) => name === 'flat_region_retire'));
    assert.ok(imports.every(({ name }) => name !== 'debug_finish'));
    clones++;
    return instantiate.apply(this, args);
  };
  const input = initial(PC, { 0: 0n, 1: 512n, 2: 0n, 3: 0n });
  const run = { stop, budget: 20_000n };
  const cpu = await createCpu({ ...options, limits });
  try {
    cpu.setState(input);
    const cold = await cpu.run(run);
    assert.equal(cold.reason, 'stop');
    assert.equal(clones, 0);

    cpu.setState(input);
    const partial = await cpu.run({ ...run, budget: 4096n });
    assert.equal(partial.reason, 'budget');
    assert.equal(partial.retired, 4096n);
    assert.equal(completed, 1);
    assert.ok(clones > 0 && clones <= 4);

    const resumed = await cpu.run(run);
    assert.equal(resumed.reason, 'stop');
    assert.equal(partial.retired + resumed.retired, cold.retired);
    assert.deepEqual(resumed.state, cold.state);

    const capped = await createCpu({
      ...options,
      limits: { ...limits, maxCachedArtifacts: cold.resources.cachedArtifacts },
    });
    try {
      const priorClones = clones;
      capped.setState(input);
      assert.deepEqual((await capped.run(run)).state, cold.state);
      capped.setState(input);
      const denied = await capped.run(run);
      assert.equal(denied.reason, 'stop');
      assert.equal(denied.retired, cold.retired);
      assert.deepEqual(denied.state, cold.state);
      assert.equal(clones, priorClones);
      assert.equal(completed, 1);
    } finally {
      capped.dispose();
    }
  } finally {
    cpu.dispose();
    Core.prototype.finishPagedRegionTrace = finish;
    Core.prototype.instantiatePagedRegionTrace = instantiate;
  }
});

test('legacy paged entries route a same-page indirect branch within one owner', async () => {
  const target = PC + 16n;
  const ram = new Uint8Array(0xa000);
  const tables = new DataView(ram.buffer);
  tables.setUint32(0x1004, 0x2003, true);
  tables.setUint32(0x2000, 0x9003, true);
  ram.set([0xff, 0xe0], 0x9000); // JMP EAX
  ram.set([0x41, 0xf4], 0x9010); // INC ECX; HLT
  const cpu = await createCpu({
    compiler,
    memoryModel: 'physical',
    context: protectedSnapshot(),
    paging: { cr0: 0x80000033n, cr2: 0n, cr3: 0x1000n, cr4: 0x618n, efer: 0n },
    entry: PC,
    limits: LIMITS,
    memory: {
      backings: [{ id: 'ram', bytes: ram }],
      mappings: [
        {
          backing: 'ram',
          base: 0n,
          offset: 0,
          length: ram.length,
          permissions: { read: true, write: true, execute: true },
        },
      ],
    },
    memoryLimits: { maxBackings: 1, maxMappings: 1, maxBackingBytes: ram.length },
  });
  const compile = Core.prototype.compile;
  Core.prototype.compile = function (pc, limits, seeds, regionPoints) {
    if (pc === PC && !seeds?.length) return compile.call(this, pc, limits, [target]);
    return compile.call(this, pc, limits, seeds, regionPoints);
  };
  try {
    cpu.setState(initial(PC, { 0: target, 1: 5n }));
    const result = await cpu.run({ stop: target + 1n, budget: 8n });
    assert.equal(result.reason, 'stop');
    assert.equal(result.retired, 2n);
    assert.equal(result.state.registers[1], 6n);
    assert.equal(result.resources.nativeEntries, 1);
    assert.equal(result.resources.dispatches, 0);
  } finally {
    Core.prototype.compile = compile;
    cpu.dispose();
  }
});

test('a short flat capture preserves the earlier returning call region', async () => {
  const bytes = new Uint8Array(0x110).fill(0xf4);
  bytes.set([
    0x85,
    0xd2, // TEST EDX,EDX
    0x74,
    0x08, // JZ to the second MOV
    0x4a, // DEC EDX
    0xb8,
    0,
    0,
    0,
    0, // MOV EAX, first handler
    0xeb,
    0x05, // JMP to CALL
    0xb8,
    0,
    0,
    0,
    0, // MOV EAX, second handler
    0xff,
    0xd0, // CALL EAX
    0x49, // DEC ECX
    0x75,
    0xea, // JNZ to TEST
  ]);
  const view = new DataView(bytes.buffer);
  view.setUint32(6, Number(PC + 0x80n), true);
  view.setUint32(13, Number(PC + 0x100n), true);
  bytes.set([...Array<number>(12).fill(0x47), 0xc3], 0x80); // INC EDI; RET
  bytes.set([...Array<number>(12).fill(0x43), 0xc3], 0x100); // INC EBX; RET
  const stack = PC + 0x1000n;
  const cpu = await createCpu({
    compiler,
    entry: PC,
    context: protectedSnapshot(),
    memory: {
      backings: [
        { id: 'code', bytes },
        { id: 'stack', bytes: new Uint8Array(64) },
      ],
      mappings: [
        {
          backing: 'code',
          base: PC,
          length: bytes.length,
          offset: 0,
          permissions: { read: true, write: false, execute: true },
        },
        {
          backing: 'stack',
          base: stack,
          length: 64,
          offset: 0,
          permissions: { read: true, write: true, execute: false },
        },
      ],
    },
    memoryLimits: { maxBackings: 2, maxMappings: 2, maxBackingBytes: bytes.length + 64 },
    limits: { ...LIMITS, maxInstructions: 32 },
  });
  const run = async (firstCalls: bigint) => {
    cpu.setState(initial(PC, { 1: 4096n, 2: firstCalls, 3: 0n, 4: stack + 64n, 7: 0n }));
    const result = await cpu.run({ stop: PC + 22n, budget: 80_000n });
    assert.equal(result.reason, 'stop');
    assert.equal(result.retired, 4096n * 19n + firstCalls * 2n);
    assert.equal(result.state.registers[7], firstCalls * 12n);
    assert.equal(result.state.registers[3], (4096n - firstCalls) * 12n);
    assert.equal(result.state.registers[4], stack + 64n);
    return result;
  };
  try {
    await run(64n);
    // This capture sees only the first handler. The returning region learned
    // from the first run still covers the second handler for the rest.
    const warm = await run(512n);
    assert.ok(warm.resources.compilations > 0);
    assert.ok(warm.resources.dispatches < 1200, String(warm.resources.dispatches));
  } finally {
    cpu.dispose();
  }
});
