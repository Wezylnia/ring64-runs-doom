import { readFile } from 'node:fs/promises';
import test from 'node:test';
import assert from 'node:assert/strict';
import { runParallelScenarios } from '../execution/parallel/index.js';
import type { MachineFactory } from '../execution/parallel/fixture.js';

test('parallel public API runs real dedicated Node workers', { timeout: 120_000 }, async () => {
  const entry = new URL('../../build/host/machine.js', import.meta.url);
  const { createMachine }: { createMachine: MachineFactory } = await import(entry.href);
  const bytes = await readFile(new URL('../../build/ring64-parallel.wasm', import.meta.url));
  const result = await runParallelScenarios(createMachine, Uint8Array.from(bytes));
  assert.ok(result.passed.length > 0);
  assert.equal(new Set(result.passed).size, result.passed.length);
});
