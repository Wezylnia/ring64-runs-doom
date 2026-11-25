import { readFile } from 'node:fs/promises';
import test from 'node:test';
import assert from 'node:assert/strict';
import type { loadParallelCompiler } from '../../sdk/src/machine.js';
import { supportsParallelMemory64 } from '../../sdk/src/parallel/memory/ram.js';
import { runParallelScenarios } from '../execution/parallel/index.js';
import { isParallelMemory64, type MachineFactory } from '../execution/parallel/fixture.js';
import { checkParallelMemory64 } from '../execution/parallel/memory64.js';

const entry = new URL('../../build/host/machine.js', import.meta.url);
const selected = process.env.RING64_PARALLEL_SCENARIO;

test('parallel public API runs real dedicated Node workers', { timeout: 120_000 }, async () => {
  const { createMachine }: { createMachine: MachineFactory } = await import(entry.href);
  const bytes = await readFile(new URL('../../build/ring64-parallel.wasm', import.meta.url));
  const result = await runParallelScenarios(createMachine, Uint8Array.from(bytes), selected).catch(
    (error: unknown) => {
      if (selected && error instanceof Error && error.cause !== undefined) throw error.cause;
      throw error;
    },
  );
  if (selected) assert.deepEqual(result.passed, [selected]);
  assert.ok(result.passed.length > 0);
  assert.equal(new Set(result.passed).size, result.passed.length);
});

test(
  'parallel compiler selects a supported profile and memory64 workers share bytes beyond 4 GiB',
  { timeout: 120_000, skip: selected !== undefined },
  async () => {
    const api: {
      createMachine: MachineFactory;
      loadParallelCompiler: typeof loadParallelCompiler;
    } = await import(entry.href);
    const supported = supportsParallelMemory64();
    const compiler = await api.loadParallelCompiler();
    assert.equal(isParallelMemory64(compiler), supported, 'automatic parallel profile');
    assert.equal(isParallelMemory64(await api.loadParallelCompiler('memory32')), false);
    if (!supported) {
      assert.notEqual(process.env.RING64_REQUIRE_PARALLEL_MEMORY64, '1');
      await assert.rejects(api.loadParallelCompiler('memory64'), { code: 'unsupported' });
      return;
    }
    const explicit = await api.loadParallelCompiler('memory64');
    assert.equal(isParallelMemory64(explicit), true, 'explicit shared memory64 profile');
    const result = await runParallelScenarios(api.createMachine, compiler);
    assert.ok(result.passed.length > 0);
    await checkParallelMemory64(api.createMachine, explicit);
  },
);
